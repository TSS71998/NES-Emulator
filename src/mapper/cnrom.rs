use super::{bank_offset, Chr, Mapper};
use crate::cartridge::{Mirroring, Rom};

const CHR_BANK: usize = 0x2000;

/// Mapper 3: CNROM. NROM-style fixed 16 or 32 KiB PRG, plus an 8 KiB CHR-ROM
/// bank selected by any write to $8000-$FFFF.

pub struct Cnrom {
    prg_rom: Vec<u8>,
    chr: Chr,
    mirroring: Mirroring,
    chr_bank: usize
}

impl Cnrom {
    pub fn new(rom: Rom) -> Self {
        Cnrom {
            chr: Chr::new(&rom),
            mirroring: rom.screen_mirroring,
            prg_rom: rom.prg_rom,
            chr_bank: 0
        }
    }

    fn chr_offset(&self, addr: u16) -> usize {
        bank_offset(self.chr_bank, CHR_BANK, self.chr.len()) + (addr as usize & 0x1FFF)
    }
}

impl Mapper for Cnrom {
    fn cpu_read(&self, addr: u16) -> Option<u8> {
        match addr {
            0x8000..=0xFFFF => Some(self.prg_rom[(addr as usize - 0x8000) % self.prg_rom.len()]),
            _ => None
        }
    }

    fn cpu_write(&mut self, addr: u16, data: u8) {
        if addr >= 0x8000 {
            self.chr_bank = data as usize;
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
    fn test_chr_bank_switching() {
        let mut cnrom = Cnrom::new(banked_rom(3, 2, 4, false));
        assert_eq!(cnrom.ppu_read(0x0000), 0);
        cnrom.cpu_write(0x8000, 2);
        assert_eq!(cnrom.ppu_read(0x0000), 16); // 8 KiB bank 2 = 1 KiB bank 16
        assert_eq!(cnrom.ppu_read(0x1C00), 23);
        cnrom.ppu_write(0x0000, 0xFF); // CHR-ROM: ignored
        assert_eq!(cnrom.ppu_read(0x0000), 16);
    }

    #[test]
    fn test_16k_prg_is_mirrored() {
        let cnrom = Cnrom::new(banked_rom(3, 1, 1, false));
        assert_eq!(cnrom.cpu_read(0xC000), cnrom.cpu_read(0x8000));
    }
}