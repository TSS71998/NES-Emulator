use super::{Chr, Mapper, bank_offset, prg_ram_size};
use crate::cartridge::{Mirroring, Rom};

const PRG_BANK: usize = 0x2000;
const CHR_BANK: usize = 0x0400;

/// Mapper 4: Nintendo MMC3 (TxROM boards). Used by Super Mario Bros. 2 and 3,
/// Mega Man 3-6, Kirby's Adventure, Ninja Gaiden 2 and 3, and many more.
///
/// Registers are pairs selected by address range and bit 0 of the address:
///
/// | Address     | Even                              | Odd                         |
/// |-------------|-----------------------------------|-----------------------------|
/// | $8000-$9FFF | bank select (which R0-R7, modes)  | bank data (value for it)    |
/// | $A000-$BFFF | mirroring (0 vertical, 1 horiz.)  | PRG-RAM enable/protect      |
/// | $C000-$DFFF | IRQ reload value (latch)          | IRQ reload (clear counter)  |
/// | $E000-$FFFF | IRQ disable + acknowledge         | IRQ enable                  |
///
/// Banking is in 8 KiB PRG units and 1 KiB CHR units (R0/R1 select 2 KiB).
///
/// The IRQ counter is clocked on each rising edge of PPU address line A12,
/// which the PPU reports through [`Mapper::ppu_address`] with every fetch.
/// With sprites at $1000 and the background at $0000 (the usual setup) A12
/// rises once per scanline, at dot 262 when the first sprite pattern is fetched;
/// with the tables the other way round it rises at dot 326. Outside rendering
/// $2006/$2007 accesses move the address, so games and test ROMs can clock
/// the counter by hand too.
///
/// The MMC3 ignores a rise unless A12 has been low for a while (it really
/// counts M2 edges, roughly 3 CPU cycles). That filters out the brief drops
/// to $2xxx for nametable fetches between background tiles at $1xxx.
pub struct Mmc3 {
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr: Chr,
    has_battery: bool,
    four_screen: bool,

    bank_select: u8,
    registers: [u8; 8],
    mirroring: Mirroring,
    prg_ram_enabled: bool,
    prg_ram_write_protected: bool,

    irq_latch: u8,
    irq_counter: u8,
    irq_reload: bool,
    irq_enabled: bool,
    irq_pending: bool,
    /// MMC3A ("old" revision) only raises an IRQ when the counter reaches 0
    /// by decrementing or after an explicit reload, not every time it's
    /// reloaded with 0. NES 2.0 submapper 4 marks these boards.
    old_irq_behavior: bool,

    /// PPU cycle when A12 last went low, or None while it's high
    a12_low_since: Option<u64>,
}

/// A12 must have been low for this many PPU dots for a rise to count.
const A12_FILTER_DOTS: u64 = 10;

impl Mmc3 {
    pub fn new(rom: Rom) -> Self {
        let four_screen = rom.screen_mirroring == Mirroring::FourScreen;
        let old_irq_behavior = rom.submapper == 4;
        Mmc3 {
            prg_ram: vec![0; prg_ram_size(&rom)],
            chr: Chr::new(&rom),
            prg_rom: rom.prg_rom,
            has_battery: rom.has_battery,
            four_screen,
            bank_select: 0,
            registers: [0, 2, 4, 5, 6, 7, 0, 1],
            mirroring: rom.screen_mirroring,
            prg_ram_enabled: true,
            prg_ram_write_protected: false,
            irq_latch: 0,
            irq_counter: 0,
            irq_reload: false,
            irq_enabled: false,
            irq_pending: false,
            old_irq_behavior,
            a12_low_since: Some(0),
        }
    }

    fn prg_offset(&self, addr: u16) -> usize {
        let len = self.prg_rom.len();
        let banks = len / PRG_BANK;
        let second_last = banks.saturating_sub(2);
        let last = banks.saturating_sub(1);
        let r6 = (self.registers[6] & 0x3F) as usize;
        let r7 = (self.registers[7] & 0x3F) as usize;
        let swap_8000_c000 = self.bank_select & 0x40 != 0;

        let bank = match ((addr - 0x8000) as usize / PRG_BANK, swap_8000_c000) {
            (0, false) => r6,
            (0, true) => second_last,
            (1, _) => r7,
            (2, false) => second_last,
            (2, true) => r6,
            _ => last,
        };
        bank_offset(bank, PRG_BANK, len) + (addr as usize & (PRG_BANK - 1))
    }

    fn chr_offset(&self, addr: u16) -> usize {
        let addr = addr as usize & 0x1FFF;
        let mut slot = addr / CHR_BANK;
        if self.bank_select & 0x80 != 0 {
            slot ^= 4;
        }
        let r = &self.registers;
        let bank = match slot {
            0 => r[0] & 0xFE,
            1 => r[0] | 0x01,
            2 => r[1] & 0xFE,
            3 => r[1] | 0x01,
            4 => r[2],
            5 => r[3],
            6 => r[4],
            _ => r[5],
        } as usize;
        bank_offset(bank, CHR_BANK, self.chr.len()) + (addr & (CHR_BANK - 1))
    }

    /// One rising edge of A12: reload or decrement the counter, then raise
    /// the IRQ if it's 0.
    fn clock_irq_counter(&mut self) {
        let was_reload = self.irq_reload;
        let was_nonzero = self.irq_counter != 0;
        if self.irq_counter == 0 || self.irq_reload {
            self.irq_counter = self.irq_latch;
            self.irq_reload = false;
        } else {
            self.irq_counter -= 1;
        }

        let raise = if self.old_irq_behavior {
            self.irq_counter == 0 && (was_nonzero || was_reload)
        } else {
            self.irq_counter == 0
        };
        if raise && self.irq_enabled {
            self.irq_pending = true;
        }
    }
}

impl Mapper for Mmc3 {
    fn cpu_read(&self, addr: u16) -> Option<u8> {
        match addr {
            0x6000..=0x7FFF if self.prg_ram_enabled => {
                Some(self.prg_ram[(addr as usize - 0x6000) % self.prg_ram.len()])
            }
            0x8000..=0xFFFF => Some(self.prg_rom[self.prg_offset(addr)]),
            _ => None,
        }
    }

    fn cpu_write(&mut self, addr: u16, data: u8) {
        let even = addr & 1 == 0;
        match addr {
            0x6000..=0x7FFF => {
                if self.prg_ram_enabled && !self.prg_ram_write_protected {
                    let len = self.prg_ram.len();
                    self.prg_ram[(addr as usize - 0x6000) % len] = data;
                }
            }
            0x8000..=0x9FFF if even => self.bank_select = data,
            0x8000..=0x9FFF => self.registers[(self.bank_select & 0x07) as usize] = data,
            0xA000..=0xBFFF if even => {
                if !self.four_screen {
                    self.mirroring = if data & 1 == 0 {
                        Mirroring::Vertical
                    } else {
                        Mirroring::Horizontal
                    };
                }
            }
            0xA000..=0xBFFF => {
                self.prg_ram_enabled = data & 0x80 != 0;
                self.prg_ram_write_protected = data & 0x40 != 0;
            }
            0xC000..=0xDFFF if even => self.irq_latch = data,
            0xC000..=0xDFFF => {
                self.irq_counter = 0;
                self.irq_reload = true;
            }
            0xE000..=0xFFFF if even => {
                self.irq_enabled = false;
                self.irq_pending = false;
            }
            0xE000..=0xFFFF => self.irq_enabled = true,
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        self.chr.read(self.chr_offset(addr))
    }

    fn ppu_write(&mut self, addr: u16, data: u8) {
        let offset = self.chr_offset(addr);
        self.chr.write(offset, data);
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn ppu_address(&mut self, addr: u16, ppu_cycle: u64) {
        if addr & 0x1000 != 0 {
            if let Some(low_since) = self.a12_low_since {
                if ppu_cycle.wrapping_sub(low_since) >= A12_FILTER_DOTS {
                    self.clock_irq_counter();
                }
            }
            self.a12_low_since = None;
        } else if self.a12_low_since.is_none() {
            self.a12_low_since = Some(ppu_cycle);
        }
    }

    fn irq_pending(&self) -> bool {
        self.irq_pending
    }

    fn watches_ppu_bus(&self) -> bool {
        true
    }

    fn battery_ram(&self) -> Option<&[u8]> {
        self.has_battery.then_some(&self.prg_ram[..])
    }

    fn load_battery_ram(&mut self, data: &[u8]) {
        let n = data.len().min(self.prg_ram.len());
        self.prg_ram[..n].copy_from_slice(&data[..n]);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mapper::test_util::banked_rom;

    /// 16 x 16 KiB PRG = 32 x 8 KiB banks; 4 x 8 KiB CHR = 32 x 1 KiB banks.
    fn mmc3() -> Mmc3 {
        Mmc3::new(banked_rom(4, 16, 4, true))
    }

    /// What the PPU does on a rendered line with sprites at $1000: background
    /// fetches from $0xxx, then sprite fetches from $1xxx at dot 262.
    fn scanline(m: &mut Mmc3, clock: &mut u64) {
        m.ppu_address(0x0FF0, *clock + 5);
        m.ppu_address(0x2000, *clock + 257);
        m.ppu_address(0x1FF0, *clock + 262);
        *clock += 341;
    }

    fn set_register(mmc3: &mut Mmc3, select: u8, value: u8) {
        mmc3.cpu_write(0x8000, select);
        mmc3.cpu_write(0x8001, value);
    }

    /// banked_rom tags each 16 KiB bank, so 8 KiB bank n reads as n / 2
    /// (with 0x80 set on the very last byte of odd-numbered 8 KiB banks).
    fn prg_bank_at(mmc3: &Mmc3, addr: u16) -> u8 {
        mmc3.cpu_read(addr).unwrap()
    }

    #[test]
    fn test_prg_mode_0() {
        let mut m = mmc3();
        set_register(&mut m, 6, 4); // 8 KiB bank 4 = 16 KiB bank 2, first half
        set_register(&mut m, 7, 7); // 8 KiB bank 7 = 16 KiB bank 3, second half
        assert_eq!(prg_bank_at(&m, 0x8000), 2);
        assert_eq!(prg_bank_at(&m, 0xA000), 3);
        assert_eq!(prg_bank_at(&m, 0xBFFF), 0x80 | 3);
        assert_eq!(prg_bank_at(&m, 0xC000), 15); // second-last 8 KiB bank
        assert_eq!(prg_bank_at(&m, 0xFFFF), 0x80 | 15); // last
    }

    #[test]
    fn test_prg_mode_1_swaps_8000_and_c000() {
        let mut m = mmc3();
        set_register(&mut m, 0x46, 4);
        assert_eq!(prg_bank_at(&m, 0x8000), 15); // second-last
        assert_eq!(prg_bank_at(&m, 0xC000), 2); // R6
    }

    #[test]
    fn test_chr_banks_and_inversion() {
        let mut m = mmc3();
        set_register(&mut m, 0, 9); // R0: 2 KiB, low bit ignored -> 1 KiB banks 8, 9
        set_register(&mut m, 1, 12); // R1: banks 12, 13
        for (r, bank) in [(2, 20), (3, 21), (4, 22), (5, 23)] {
            set_register(&mut m, r, bank);
        }
        assert_eq!(m.ppu_read(0x0000), 8);
        assert_eq!(m.ppu_read(0x0400), 9);
        assert_eq!(m.ppu_read(0x0BFF), 0x80 | 12);
        assert_eq!(m.ppu_read(0x1000), 20);
        assert_eq!(m.ppu_read(0x1C00), 23);

        m.cpu_write(0x8000, 0x80); // CHR inversion
        assert_eq!(m.ppu_read(0x0000), 20);
        assert_eq!(m.ppu_read(0x1000), 8);
        assert_eq!(m.ppu_read(0x1800), 12);
    }

    #[test]
    fn test_mirroring_and_prg_ram() {
        let mut m = mmc3();
        m.cpu_write(0xA000, 0);
        assert_eq!(m.mirroring(), Mirroring::Vertical);
        m.cpu_write(0xA000, 1);
        assert_eq!(m.mirroring(), Mirroring::Horizontal);

        m.cpu_write(0x6000, 0x42);
        assert_eq!(m.cpu_read(0x6000), Some(0x42));
        m.cpu_write(0xA001, 0xC0); // enabled, write-protected
        m.cpu_write(0x6000, 0x99);
        assert_eq!(m.cpu_read(0x6000), Some(0x42));
        m.cpu_write(0xA001, 0x00); // disabled
        assert_eq!(m.cpu_read(0x6000), None);
        assert_eq!(m.battery_ram().unwrap()[0], 0x42);
    }

    #[test]
    fn test_irq_after_latch_plus_one_scanlines() {
        let mut m = mmc3();
        let mut t = 0;
        m.cpu_write(0xC000, 3); // latch
        m.cpu_write(0xC001, 0); // reload on next clock
        m.cpu_write(0xE001, 0); // enable
        scanline(&mut m, &mut t); // reload to 3
        scanline(&mut m, &mut t); // 2
        scanline(&mut m, &mut t); // 1
        assert!(!m.irq_pending());
        scanline(&mut m, &mut t); // 0 -> IRQ
        assert!(m.irq_pending());

        m.cpu_write(0xE000, 0); // acknowledge (and disable)
        assert!(!m.irq_pending());
        m.cpu_write(0xE001, 0);
        scanline(&mut m, &mut t); // reload to 3, counting continues
        assert!(!m.irq_pending());
    }

    #[test]
    fn test_counter_runs_while_irq_disabled() {
        let mut m = mmc3();
        let mut t = 0;
        m.cpu_write(0xC000, 1);
        m.cpu_write(0xC001, 0);
        scanline(&mut m, &mut t); // 1
        scanline(&mut m, &mut t); // 0, but disabled: no IRQ
        assert!(!m.irq_pending());
        m.cpu_write(0xE001, 0);
        scanline(&mut m, &mut t); // reload 1
        scanline(&mut m, &mut t); // 0 -> IRQ
        assert!(m.irq_pending());
    }

    #[test]
    fn test_a12_rising_edge_clocks_counter() {
        let mut m = mmc3();
        m.cpu_write(0xC000, 0); // latch 0: every clock reloads 0 and raises IRQ
        m.cpu_write(0xE001, 0);
        m.ppu_address(0x0FFF, 100);
        m.ppu_address(0x0000, 110);
        assert!(!m.irq_pending(), "A12 never went high");
        m.ppu_address(0x1000, 120);
        assert!(m.irq_pending(), "rising edge");
        m.cpu_write(0xE000, 0);
        m.cpu_write(0xE001, 0);
        m.ppu_address(0x1FFF, 130);
        assert!(!m.irq_pending(), "still high: no new edge");
    }

    #[test]
    fn test_short_a12_drops_are_filtered() {
        let mut m = mmc3();
        m.cpu_write(0xC000, 0);
        m.cpu_write(0xE001, 0);
        m.ppu_address(0x1000, 100); // first rise after a long low: counts
        m.cpu_write(0xE000, 0);
        m.cpu_write(0xE001, 0);
        // background tiles at $1xxx: nametable fetches at $2xxx drop A12
        // for only a few dots between pattern fetches
        for tile in 0..32 {
            let dot = 200 + tile * 8;
            m.ppu_address(0x2000, dot);
            m.ppu_address(0x23C0, dot + 2);
            m.ppu_address(0x1000, dot + 4);
            m.ppu_address(0x1008, dot + 6);
        }
        assert!(!m.irq_pending());
        // sprites at $0xxx keep it low for 64 dots, so the next rise counts
        m.ppu_address(0x0FF0, 500);
        m.ppu_address(0x1000, 564);
        assert!(m.irq_pending());
    }

    #[test]
    fn test_old_revision_irq_on_zero_reload_only_after_clear() {
        let mut rom = banked_rom(4, 16, 4, false);
        rom.submapper = 4; // MMC3A
        let mut m = Mmc3::new(rom);
        let mut t = 0;
        m.cpu_write(0xC000, 0);
        m.cpu_write(0xE001, 0);
        m.irq_counter = 0;
        scanline(&mut m, &mut t); // counter was already 0: reloads 0, no IRQ on MMC3A
        assert!(!m.irq_pending());
        m.cpu_write(0xC001, 0); // explicit reload
        scanline(&mut m, &mut t);
        assert!(m.irq_pending());
    }

    #[test]
    fn test_battery_ram_round_trip() {
        let mut m = mmc3();
        let mut save = vec![0; 0x2000];
        save[0x123] = 0x77;
        m.load_battery_ram(&save);
        assert_eq!(m.cpu_read(0x6123), Some(0x77));
    }
}
