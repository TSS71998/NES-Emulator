use super::{bank_offset, Chr, Mapper};
use crate::cartridge::{Mirroring, Rom};

const PRG_BANK: usize = 0x4000;

/// Mapper 2: UxROM (UNROM, UOROM). A switchable 16 KiB PRG bank at $8000
/// and the last bank fixed at $C000; any write to $8000-$FFFF selects the
/// bank. CHR is usually 8 KiB of RAM.

pub struct Uxrom {
    prg_rom: Vec<u8>,
    chr: Chr,
    mirroring: Mirroring,
    bank: usize
}

impl Uxrom {
    pub fn new(rom: Rom) -> Self {
        Uxrom {
            chr: Chr::new(&rom),
            mirroring: rom.screen_mirroring,
            prg_rom: rom.prg_rom,
            bank: 0
        }
    }
}

impl Mapper for Uxrom {
    fn cpu_read(&self, addr: u16) -> Option<u8> {
        let len = self.prg_rom.len();
        let offset = addr as usize & (PRG_BANK - 1);
        match addr {
            0x8000..=0xBFFF => Some(self.prg_rom[bank_offset(self.bank, PRG_BANK, len)]),
            0xC000..=0xFFFF => Some(self.prg_rom[len - PRG_BANK + offset]),
            _ => None
        }
    }

    fn cpu_write(&mut self, addr: u16, data: u8) {
        if addr >= 0x8000 {
            self.bank = data as usize;
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        self.chr.read(addr as usize & 0x1FFF)
    }

    fn ppu_write(&mut self, addr: u16, data: u8) {
        self.chr.write(addr as usize & 0x1FFF, data);
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
    fn test_bank_switching() {
        let mut uxrom = Uxrom::new(banked_rom(2, 8, 0, false));
        assert_eq!(uxrom.cpu_read(0x8000), Some(0));
        assert_eq!(uxrom.cpu_read(0xC000), Some(7));
        uxrom.cpu_write(0x8000, 3);
        assert_eq!(uxrom.cpu_read(0x8000), Some(3));
        assert_eq!(uxrom.cpu_read(0xFFFF), Some(7));
        uxrom.cpu_write(0xFFFF, 9); // wraps to bank 1 on an 8-bank cart
        assert_eq!(uxrom.cpu_read(0xBFFF), Some(1));
    }

    #[test]
    fn test_chr_ram() {
        let mut uxrom = Uxrom::new(banked_rom(2, 8, 0, false));
        uxrom.ppu_write(0x0042, 0x77);
        assert_eq!(uxrom.ppu_read(0x0042), 0x77);
    }
}