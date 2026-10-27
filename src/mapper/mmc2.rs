use super::{Chr, Mapper, bank_offset, prg_ram_size};
use crate::cartridge::{Mirroring, Rom};
use crate::state::{StateReader, StateWriter};
use std::cell::Cell;

const CHR_BANK: usize = 0x1000;

/// Mapper 9: Nintendo MMC2 (PNROM: Punch-Out!!), and mapper 10: MMC4
/// (FxROM: Fire Emblem, Famicom Wars).
///
/// | Address     | Register                                             |
/// |-------------|------------------------------------------------------|
/// | $A000-$AFFF | PRG bank at $8000: 8 KiB (MMC2) or 16 KiB (MMC4)      |
/// | $B000-$BFFF | 4 KiB CHR bank at $0000 while latch 0 is $FD          |
/// | $C000-$CFFF | 4 KiB CHR bank at $0000 while latch 0 is $FE          |
/// | $D000-$DFFF | 4 KiB CHR bank at $1000 while latch 1 is $FD          |
/// | $E000-$EFFF | 4 KiB CHR bank at $1000 while latch 1 is $FE          |
/// | $F000-$FFFF | mirroring (0 vertical, 1 horizontal)                 |
///
/// The rest of PRG is fixed to the last banks. The latches switch when the
/// PPU reads tile $FD or $FE's last row: $0FD8 or $0FE8 for latch 0 (MMC4:
/// $0FD8-$0FDF, $0FE8-$0FEF), $1FD8-$1FDF or $1FE8-$1FEF for latch 1. The
/// read that triggers it still uses the old bank, so a game can put a
/// marker tile at the edge of a picture to switch banks part-way through
/// a scanline (Punch-Out!!'s boxers).
pub struct Mmc2 {
    mmc4: bool,
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr: Chr,
    has_battery: bool,
    prg_bank: usize,
    /// CHR banks: [$0000 FD, $0000 FE, $1000 FD, $1000 FE]
    chr_banks: [usize; 4],
    mirroring: Mirroring,
    /// Whether each latch is at $FE (otherwise $FD). PPU reads set them,
    /// and those only have `&self`.
    latch_fe: [Cell<bool>; 2],
}

impl Mmc2 {
    pub fn new(rom: Rom) -> Self {
        let mmc4 = rom.mapper == 10;
        Mmc2 {
            mmc4,
            prg_ram: vec![0; if mmc4 { prg_ram_size(&rom) } else { 0 }],
            chr: Chr::new(&rom),
            has_battery: rom.has_battery,
            prg_rom: rom.prg_rom,
            prg_bank: 0,
            chr_banks: [0; 4],
            mirroring: rom.screen_mirroring,
            latch_fe: [Cell::new(true), Cell::new(true)],
        }
    }

    fn prg_bank_size(&self) -> usize {
        if self.mmc4 { 0x4000 } else { 0x2000 }
    }

    fn chr_offset(&self, addr: u16) -> usize {
        let half = (addr as usize >> 12) & 1;
        let fe = self.latch_fe[half].get() as usize;
        let bank = self.chr_banks[half * 2 + fe];
        bank_offset(bank, CHR_BANK, self.chr.len()) + (addr as usize & (CHR_BANK - 1))
    }

    fn update_latch(&self, addr: u16) {
        let addr = addr & 0x1FFF;
        let half = (addr >> 12) as usize;
        let tile = addr & 0x0FF8;
        let exact = half == 1 || self.mmc4 || addr & 7 == 0;
        if exact && tile == 0x0FD8 {
            self.latch_fe[half].set(false);
        } else if exact && tile == 0x0FE8 {
            self.latch_fe[half].set(true);
        }
    }
}

impl Mapper for Mmc2 {
    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.prg_ram);
        self.chr.save_state(w);
        w.usize(self.prg_bank);
        for bank in self.chr_banks {
            w.usize(bank);
        }
        w.mirroring(self.mirroring);
        w.bool(self.latch_fe[0].get());
        w.bool(self.latch_fe[1].get());
    }

    fn load_state(&mut self, r: &mut StateReader) {
        r.bytes_into(&mut self.prg_ram);
        self.chr.load_state(r);
        self.prg_bank = r.usize();
        for bank in &mut self.chr_banks {
            *bank = r.usize();
        }
        self.mirroring = r.mirroring();
        self.latch_fe[0].set(r.bool());
        self.latch_fe[1].set(r.bool());
    }

    fn cpu_read(&self, addr: u16) -> Option<u8> {
        let size = self.prg_bank_size();
        let len = self.prg_rom.len();
        match addr {
            0x6000..=0x7FFF if !self.prg_ram.is_empty() => {
                Some(self.prg_ram[(addr as usize - 0x6000) % self.prg_ram.len()])
            }
            0x8000..=0xFFFF => {
                let offset = addr as usize - 0x8000;
                let base = if offset < size {
                    bank_offset(self.prg_bank, size, len)
                } else {
                    // the rest is the end of the ROM
                    len - (0x8000 - size) - size
                };
                Some(self.prg_rom[(base + offset) % len])
            }
            _ => None,
        }
    }

    fn cpu_write(&mut self, addr: u16, data: u8) {
        let d = data as usize;
        match addr {
            0x6000..=0x7FFF if !self.prg_ram.is_empty() => {
                let len = self.prg_ram.len();
                self.prg_ram[(addr as usize - 0x6000) % len] = data;
            }
            0xA000..=0xAFFF => self.prg_bank = d & 0x0F,
            0xB000..=0xBFFF => self.chr_banks[0] = d & 0x1F,
            0xC000..=0xCFFF => self.chr_banks[1] = d & 0x1F,
            0xD000..=0xDFFF => self.chr_banks[2] = d & 0x1F,
            0xE000..=0xEFFF => self.chr_banks[3] = d & 0x1F,
            0xF000..=0xFFFF => {
                self.mirroring = if data & 1 == 0 {
                    Mirroring::Vertical
                } else {
                    Mirroring::Horizontal
                };
            }
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        let data = self.chr.read(self.chr_offset(addr));
        self.update_latch(addr);
        data
    }

    fn ppu_write(&mut self, addr: u16, data: u8) {
        let offset = self.chr_offset(addr);
        self.chr.write(offset, data);
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }

    fn battery_ram(&self) -> Option<&[u8]> {
        (self.has_battery && !self.prg_ram.is_empty()).then_some(&self.prg_ram[..])
    }

    fn load_battery_ram(&mut self, data: &[u8]) {
        let len = data.len().min(self.prg_ram.len());
        self.prg_ram[..len].copy_from_slice(&data[..len]);
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mapper::test_util::banked_rom;

    #[test]
    fn test_mmc2_prg_banks() {
        let mut m = Mmc2::new(banked_rom(9, 8, 16, false));
        m.cpu_write(0xA000, 5); // 8 KiB bank 5 = second half of 16 KiB bank 2
        assert_eq!(m.cpu_read(0x8000), Some(2));
        assert_eq!(m.cpu_read(0x9FFF), Some(0x82));
        assert_eq!(m.cpu_read(0xA000), Some(6), "$A000 is the third-last 8 KiB");
        assert_eq!(m.cpu_read(0xFFFF), Some(0x87));
    }

    #[test]
    fn test_mmc4_prg_banks() {
        let mut m = Mmc2::new(banked_rom(10, 8, 16, true));
        m.cpu_write(0xA000, 3);
        assert_eq!(m.cpu_read(0x8000), Some(3));
        assert_eq!(m.cpu_read(0xC000), Some(7));
        m.cpu_write(0x6000, 0x42);
        assert_eq!(m.battery_ram().unwrap()[0], 0x42);
    }

    #[test]
    fn test_latches_switch_after_the_marker_tile() {
        let mut m = Mmc2::new(banked_rom(9, 8, 16, false));
        m.cpu_write(0xB000, 2); // $0000 at FD: 4 KiB bank 2 (1 KiB tag 8)
        m.cpu_write(0xC000, 3); // $0000 at FE: bank 3 (tag 12)
        m.cpu_write(0xD000, 4);
        m.cpu_write(0xE000, 5);
        assert_eq!(m.ppu_read(0x0000), 12, "latches start at FE");
        assert_eq!(m.ppu_read(0x0FD8), 15, "the marker read still uses the old bank");
        assert_eq!(m.ppu_read(0x0000), 8);
        m.ppu_read(0x0FE9); // MMC2: only $0FE8 itself counts for latch 0
        assert_eq!(m.ppu_read(0x0000), 8);
        m.ppu_read(0x1FDD); // latch 1 takes the whole row
        assert_eq!(m.ppu_read(0x1000), 16);
        m.ppu_read(0x0FE8);
        assert_eq!(m.ppu_read(0x0000), 12);
    }
}