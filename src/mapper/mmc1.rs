use super::{bank_offset, prg_ram_size, Chr, Mapper};
use crate::cartridge::{Mirroring, Rom};

/// Mapper 1: Nintendo MMC1 (SxROM boards). 
///
/// The CPU loads its registers one bit at a time: five writes to $8000-$FFFF
/// shift bit 0 into a shift register, and the fifth write's address picks
/// which register receives the value. Writing a value with bit 7 set resets
/// the shift register.
///
/// | Register | Address     | Bits                                                   |
/// |----------|-------------|--------------------------------------------------------|
/// | Control  | $8000-$9FFF | 0-1 mirroring, 2-3 PRG bank mode, 4 CHR bank mode       |
/// | CHR 0    | $A000-$BFFF | 4 KiB CHR bank at $0000 (or 8 KiB bank, low bit ignored) |
/// | CHR 1    | $C000-$DFFF | 4 KiB CHR bank at $1000 (ignored in 8 KiB mode)          |
/// | PRG      | $E000-$FFFF | 0-3 16 KiB PRG bank, 4 PRG-RAM disable         

pub struct Mmc1 {
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr: Chr,
    has_battery: bool,

    shift: u8,
    shift_count: u8,

    control: u8,
    chr_bank0: u8,
    chr_bank1: u8,
    prg_bank: u8
}

const PRG_BANK: usize = 0x4000;
const CHR_BANK: usize = 0x1000;
const PRG_OUTER_BANK: usize = 0x40000;

impl Mmc1 {
    pub fn new(rom: Rom) -> Self {
        Mmc1 {
            prg_ram: vec![0; prg_ram_size(&rom)],
            chr: Chr::new(&rom),
            has_battery: rom.has_battery,
            prg_rom: rom.prg_rom,

            shift: 0,
            shift_count: 0,

            control: 0x0C,
            chr_bank0: 0,
            chr_bank1: 0,
            prg_bank: 0
        }
    }

    fn prg_ram_enabled(&self) -> bool {
        self.prg_bank & 0x10 == 0
    }

    fn prg_offset(&self, addr: u16) -> usize {
        let len = self.prg_rom.len();
        let outer = if len > PRG_OUTER_BANK {
            ((self.chr_bank0 as usize >> 4) & 1) * PRG_OUTER_BANK
        } else {
            0
        };

        let window_banks = len.min(PRG_OUTER_BANK) / PRG_BANK;
        let bank = (self.prg_bank & 0x0F) as usize;
        let offset_in_bank = (addr as usize) & (PRG_BANK - 1);

        let selected = match (self.control >> 2) & 0b11 {
            // 32 KiB mode: the low bit of the bank number is ignored
            0 | 1 => (bank & !1) + ((addr as usize - 0x8000) / PRG_BANK),
            // first bank fixed at $8000, switchable bank at $C000
            2 => if addr < 0xC000 { 0 } else { bank },
            // switchable bank at $8000, last bank fixed at $C000
            _ => if addr < 0xC000 { bank } else { window_banks - 1}
        };

        (outer + bank_offset(selected, PRG_BANK, len.min(PRG_OUTER_BANK)) + offset_in_bank) % len
    }

    fn chr_offset(&self, addr: u16) -> usize {
        let addr = addr as usize & 0x1FFF;
        if self.control & 0x10 == 0 {
            let bank = (self.chr_bank0 & 0x1E) as usize;
            bank_offset(bank, CHR_BANK, self.chr.len()) + addr
        } else {
            let bank = if addr < CHR_BANK { self.chr_bank0 } else { self.chr_bank1 } as usize;
            bank_offset(bank, CHR_BANK, self.chr.len()) + (addr & (CHR_BANK - 1))
        }
    }

    fn write_register(&mut self, addr: u16, value: u8) {
        match addr {
            0x8000..=0x9FFF => self.control = value,
            0xA000..=0xBFFF => self.chr_bank0 = value,
            0xC000..=0xDFFF => self.chr_bank1 = value,
            _ => self.prg_bank = value
        }
    }
}

impl Mapper for Mmc1 {
    fn cpu_read(&self, addr: u16) -> Option<u8> {
        match addr {
            0x6000..=0x7FFF if self.prg_ram_enabled() => {
                Some(self.prg_ram[(addr as usize - 0x6000) % self.prg_ram.len()])
            }
            0x8000..=0xFFFF => Some(self.prg_rom[self.prg_offset(addr)]),
            _ => None
        }    
    }

    fn cpu_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x6000..=0x7FFF => {
                if self.prg_ram_enabled() {
                    let len = self.prg_ram.len();
                    self.prg_ram[(addr as usize - 0x6000) % len] = data;
                }
            }
            0x8000..=0xFFFF => {
                if data & 0x80 != 0 {
                    self.shift = 0;
                    self.shift_count = 0;
                    self.control |= 0x0C;
                    return;
                }
                self.shift |= (data & 1) << self.shift_count;
                self.shift_count += 1;
                if self.shift_count == 5 {
                    let value = self.shift;
                    self.shift = 0;
                    self.shift_count = 0;
                    self.write_register(addr, value);
                }
            }
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
        match self.control & 0b11 {
            0 => Mirroring::SingleScreenLower,
            1 => Mirroring::SingleScreenUpper,
            2 => Mirroring::Vertical,
            _ => Mirroring::Horizontal
        }
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

    /// Loads a 5-bit value into the register at `addr`, the way games do.
    fn load(mmc1: &mut Mmc1, addr: u16, value: u8) {
        for i in 0..5 {
            mmc1.cpu_write(addr, (value >> i) & 1);
        }
    }

    #[test]
    fn test_power_on_fixes_last_bank_at_c000() {
        let mmc1 = Mmc1::new(banked_rom(1, 16, 0, false));
        assert_eq!(mmc1.cpu_read(0x8000), Some(0));
        assert_eq!(mmc1.cpu_read(0xFFFF), Some(15));
    }

    #[test]
    fn test_prg_mode_3_switches_8000() {
        let mut mmc1 = Mmc1::new(banked_rom(1, 16, 0, false));
        load(&mut mmc1, 0xE000, 5);
        assert_eq!(mmc1.cpu_read(0x8000), Some(5));
        assert_eq!(mmc1.cpu_read(0xBFFF), Some(5));
        assert_eq!(mmc1.cpu_read(0xC000), Some(15));
    }

    #[test]
    fn test_prg_mode_2_switches_c000() {
        let mut mmc1 = Mmc1::new(banked_rom(1, 16, 0, false));
        load(&mut mmc1, 0x8000, 0b01000);
        load(&mut mmc1, 0xE000, 7);
        assert_eq!(mmc1.cpu_read(0x8000), Some(0));
        assert_eq!(mmc1.cpu_read(0xC000), Some(7));
    }

    #[test]
    fn test_prg_32k_mode_ignores_low_bit() {
        let mut mmc1 = Mmc1::new(banked_rom(1, 16, 0, false));
        load(&mut mmc1, 0x8000, 0b00000);
        load(&mut mmc1, 0xE000, 5);
        assert_eq!(mmc1.cpu_read(0x8000), Some(4));
        assert_eq!(mmc1.cpu_read(0xC000), Some(5));
    }

    #[test]
    fn test_reset_bit_clears_shift_register() {
        let mut mmc1 = Mmc1::new(banked_rom(1, 16, 0, false));
        mmc1.cpu_write(0xE000, 1);
        mmc1.cpu_write(0xE000, 1);
        mmc1.cpu_write(0xE000, 0x80); // reset: the two bits above are dropped
        load(&mut mmc1, 0xE000, 2);
        assert_eq!(mmc1.cpu_read(0x8000), Some(2));
    }

    #[test]
    fn test_mirroring() {
        let mut mmc1 = Mmc1::new(banked_rom(1, 2, 1, false));
        for (bits, expected) in [
            (0, Mirroring::SingleScreenLower),
            (1, Mirroring::SingleScreenUpper),
            (2, Mirroring::Vertical),
            (3, Mirroring::Horizontal),
        ] {
            load(&mut mmc1, 0x8000, 0b01100 | bits);
            assert_eq!(mmc1.mirroring(), expected);
        }
    }

    #[test]
    fn test_chr_4k_banks() {
        // 4 x 8 KiB CHR-ROM = eight 4 KiB banks; each 1 KiB is tagged
        let mut mmc1 = Mmc1::new(banked_rom(1, 2, 4, false));
        load(&mut mmc1, 0x8000, 0b11100);
        load(&mut mmc1, 0xA000, 3);
        load(&mut mmc1, 0xC000, 6);
        assert_eq!(mmc1.ppu_read(0x0000), 12); // 4 KiB bank 3 = 1 KiB bank 12
        assert_eq!(mmc1.ppu_read(0x1000), 24); // 4 KiB bank 6 = 1 KiB bank 24
    }

    #[test]
    fn test_chr_8k_mode_ignores_low_bit_and_chr1() {
        let mut mmc1 = Mmc1::new(banked_rom(1, 2, 4, false));
        load(&mut mmc1, 0x8000, 0b01100);
        load(&mut mmc1, 0xA000, 3);
        load(&mut mmc1, 0xC000, 6);
        assert_eq!(mmc1.ppu_read(0x0000), 8); // banks 2+3
        assert_eq!(mmc1.ppu_read(0x1000), 12);
    }

    #[test]
    fn test_chr_ram_and_prg_ram() {
        let mut mmc1 = Mmc1::new(banked_rom(1, 16, 0, true));
        mmc1.ppu_write(0x1234, 0x56);
        assert_eq!(mmc1.ppu_read(0x1234), 0x56);

        mmc1.cpu_write(0x6010, 0x99);
        assert_eq!(mmc1.cpu_read(0x6010), Some(0x99));
        assert_eq!(mmc1.battery_ram().unwrap()[0x10], 0x99);

        load(&mut mmc1, 0xE000, 0x10); // disable PRG-RAM
        assert_eq!(mmc1.cpu_read(0x6010), None);
    }

    #[test]
    fn test_load_battery_ram() {
        let mut mmc1 = Mmc1::new(banked_rom(1, 16, 0, true));
        let mut save = vec![0; 0x2000];
        save[0x0400] = 0xAB;
        mmc1.load_battery_ram(&save);
        assert_eq!(mmc1.cpu_read(0x6400), Some(0xAB));
    }

    #[test]
    fn test_no_battery_means_no_save() {
        let mmc1 = Mmc1::new(banked_rom(1, 2, 1, false));
        assert!(mmc1.battery_ram().is_none());
    }
}