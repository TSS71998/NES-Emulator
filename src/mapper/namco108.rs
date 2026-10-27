use super::{Chr, Mapper, bank_offset};
use crate::cartridge::{Mirroring, Rom};
use crate::state::{StateReader, StateWriter};

const PRG_BANK: usize = 0x2000;
const CHR_BANK: usize = 0x0400;

/// Mapper 206: Namco 108 / DxROM (Gauntlet, Karnov, Ring King, Babel no
/// Tou). The chip the MMC3 grew out of: the same bank select ($8000, even)
/// and bank data ($8001, odd) pair and the same R0-R7, but no PRG or CHR
/// mode bits, no IRQ and no mirroring control.
///
/// R0/R1 select 2 KiB CHR banks at $0000/$0800, R2-R5 1 KiB banks at
/// $1000-$1C00, R6/R7 8 KiB PRG banks at $8000/$A000; $C000-$FFFF is the
/// last 16 KiB.
pub struct Namco108 {
    prg_rom: Vec<u8>,
    chr: Chr,
    mirroring: Mirroring,
    bank_select: u8,
    registers: [u8; 8],
}

impl Namco108 {
    pub fn new(rom: Rom) -> Self {
        Namco108 {
            chr: Chr::new(&rom),
            mirroring: rom.screen_mirroring,
            prg_rom: rom.prg_rom,
            bank_select: 0,
            registers: [0, 2, 4, 5, 6, 7, 0, 1],
        }
    }

    fn chr_offset(&self, addr: u16) -> usize {
        let addr = addr as usize & 0x1FFF;
        let r = &self.registers;
        let bank = match addr / CHR_BANK {
            0 => r[0] & 0x3E,
            1 => r[0] | 1,
            2 => r[1] & 0x3E,
            3 => r[1] | 1,
            slot => r[slot - 2],
        } as usize;
        bank_offset(bank & 0x3F, CHR_BANK, self.chr.len()) + (addr & (CHR_BANK - 1))
    }
}

impl Mapper for Namco108 {
    fn save_state(&self, w: &mut StateWriter) {
        self.chr.save_state(w);
        w.u8(self.bank_select);
        w.bytes(&self.registers);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        self.chr.load_state(r);
        self.bank_select = r.u8();
        r.bytes_into(&mut self.registers);
    }

    fn cpu_read(&self, addr: u16) -> Option<u8> {
        if addr < 0x8000 {
            return None;
        }
        let len = self.prg_rom.len();
        let slot = (addr as usize - 0x8000) / PRG_BANK;
        let base = match slot {
            0 | 1 => bank_offset((self.registers[6 + slot] & 0x0F) as usize, PRG_BANK, len),
            _ => len - 0x4000 + (slot - 2) * PRG_BANK,
        };
        Some(self.prg_rom[(base + (addr as usize & (PRG_BANK - 1))) % len])
    }

    fn cpu_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x8000..=0x9FFF if addr & 1 == 0 => self.bank_select = data & 7,
            0x8000..=0x9FFF => self.registers[self.bank_select as usize] = data,
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
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mapper::test_util::banked_rom;

    #[test]
    fn test_banks() {
        let mut m = Namco108::new(banked_rom(206, 8, 8, false));
        for (reg, value) in [(0, 9), (1, 12), (2, 20), (5, 33), (6, 5), (7, 8)] {
            m.cpu_write(0x8000, reg);
            m.cpu_write(0x8001, value);
        }
        assert_eq!(m.ppu_read(0x0000), 8, "2 KiB banks ignore bit 0");
        assert_eq!(m.ppu_read(0x0400), 9);
        assert_eq!(m.ppu_read(0x0800), 12);
        assert_eq!(m.ppu_read(0x1000), 20);
        assert_eq!(m.ppu_read(0x1C00), 33);
        assert_eq!(m.cpu_read(0x8000), Some(2)); // 8 KiB bank 5
        assert_eq!(m.cpu_read(0xA000), Some(4)); // 8 KiB bank 8
        assert_eq!(m.cpu_read(0xC000), Some(7));
        assert_eq!(m.cpu_read(0xFFFF), Some(0x87));
    }
}