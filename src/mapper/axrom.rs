use super::{Chr, Mapper, bank_offset};
use crate::cartridge::{Mirroring, Rom};
use crate::state::{StateReader, StateWriter};

const PRG_BANK: usize = 0x8000;

/// Mapper 7: AxROM (ANROM, AMROM, AOROM). Writes to $8000-$FFFF select a
/// 32 KiB PRG bank (bits 0-2) and which 1 KiB of VRAM all four nametables
/// show (bit 4). CHR is 8 KiB of RAM.
pub struct Axrom {
    prg_rom: Vec<u8>,
    chr: Chr,
    bank: usize,
    upper_screen: bool,
}

impl Axrom {
    pub fn new(rom: Rom) -> Self {
        Axrom {
            chr: Chr::new(&rom),
            prg_rom: rom.prg_rom,
            bank: 0,
            upper_screen: false,
        }
    }
}

impl Mapper for Axrom {
    fn save_state(&self, w: &mut StateWriter) {
        self.chr.save_state(w);
        w.usize(self.bank);
        w.bool(self.upper_screen);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        self.chr.load_state(r);
        self.bank = r.usize();
        self.upper_screen = r.bool();
    }
    
    fn cpu_read(&self, addr: u16) -> Option<u8> {
        match addr {
            0x8000..=0xFFFF => {
                let base = bank_offset(self.bank, PRG_BANK, self.prg_rom.len());
                Some(self.prg_rom[(base + (addr as usize - 0x8000)) % self.prg_rom.len()])
            }
            _ => None,
        }
    }

    fn cpu_write(&mut self, addr: u16, data: u8) {
        if addr >= 0x8000 {
            self.bank = (data & 0x07) as usize;
            self.upper_screen = data & 0x10 != 0;
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        self.chr.read(addr as usize & 0x1FFF)
    }

    fn ppu_write(&mut self, addr: u16, data: u8) {
        self.chr.write(addr as usize & 0x1FFF, data);
    }

    fn mirroring(&self) -> Mirroring {
        if self.upper_screen {
            Mirroring::SingleScreenUpper
        } else {
            Mirroring::SingleScreenLower
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mapper::test_util::banked_rom;

    #[test]
    fn test_bank_and_mirroring() {
        let mut axrom = Axrom::new(banked_rom(7, 8, 0, false));
        assert_eq!(axrom.mirroring(), Mirroring::SingleScreenLower);
        axrom.cpu_write(0x8000, 0x12); // 32 KiB bank 2, upper screen
        assert_eq!(axrom.cpu_read(0x8000), Some(4));
        assert_eq!(axrom.cpu_read(0xC000), Some(5));
        assert_eq!(axrom.cpu_read(0xFFFF), Some(0x85));
        assert_eq!(axrom.mirroring(), Mirroring::SingleScreenUpper);
    }
}
