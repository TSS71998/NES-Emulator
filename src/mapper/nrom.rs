use super::Mapper;
use crate::cartridge::{Mirroring, Rom};
use crate::state::{StateReader, StateWriter};

const PRG_RAM_SIZE: usize = 0x2000;
const CHR_RAM_SIZE: usize = 0x2000;

/// Mapper 0: 16 or 32 KiB of PRG-ROM (16 KiB is mirrored into $C000-$FFFF),
/// 8 KiB of CHR-ROM or CHR-RAM, fixed mirroring and no bank switching.
pub struct Nrom {
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr: Vec<u8>,
    chr_is_ram: bool,
    mirroring: Mirroring,
}

impl Nrom {
    pub fn new(rom: Rom) -> Self {
        Nrom::from_parts(rom.prg_rom, rom.chr_rom, rom.screen_mirroring)
    }

    /// A cartridge with no CHR-ROM gets 8 KiB of CHR-RAM instead.
    pub fn from_parts(prg_rom: Vec<u8>, chr_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        let chr_is_ram = chr_rom.is_empty();
        let chr = if chr_is_ram {
            vec![0; CHR_RAM_SIZE]
        } else {
            chr_rom
        };

        Nrom {
            prg_rom,
            prg_ram: vec![0; PRG_RAM_SIZE],
            chr,
            chr_is_ram,
            mirroring,
        }
    }
}

impl Mapper for Nrom {
    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.prg_ram);
        if self.chr_is_ram {
            w.bytes(&self.chr);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) {
        r.bytes_into(&mut self.prg_ram);
        if self.chr_is_ram {
            r.bytes_into(&mut self.chr);
        }
    }
    
    fn cpu_read(&self, addr: u16) -> Option<u8> {
        match addr {
            0x6000..=0x7FFF => Some(self.prg_ram[(addr - 0x6000) as usize]),
            0x8000..=0xFFFF => Some(self.prg_rom[(addr - 0x8000) as usize % self.prg_rom.len()]),
            _ => None,
        }
    }

    fn cpu_write(&mut self, addr: u16, data: u8) {
        if let 0x6000..=0x7FFF = addr {
            self.prg_ram[(addr - 0x6000) as usize] = data;
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        self.chr[(addr & 0x1FFF) as usize]
    }

    fn ppu_write(&mut self, addr: u16, data: u8) {
        if self.chr_is_ram {
            self.chr[(addr & 0x1FFF) as usize] = data;
        }
    }

    fn mirroring(&self) -> Mirroring {
        self.mirroring
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_16k_prg_is_mirrored() {
        let mut prg = vec![0; 0x4000];
        prg[0x0123] = 0x42;
        let nrom = Nrom::from_parts(prg, vec![0; CHR_RAM_SIZE], Mirroring::Horizontal);
        assert_eq!(nrom.cpu_read(0x8123), Some(0x42));
        assert_eq!(nrom.cpu_read(0xC123), Some(0x42));
    }

    #[test]
    fn test_prg_ram() {
        let mut nrom =
            Nrom::from_parts(vec![0; 0x8000], vec![1; CHR_RAM_SIZE], Mirroring::Vertical);
        nrom.cpu_write(0x6005, 0x99);
        assert_eq!(nrom.cpu_read(0x6005), Some(0x99));
        assert_eq!(nrom.cpu_read(0x5000), None);
    }

    #[test]
    fn test_chr_rom_is_read_only() {
        let mut nrom =
            Nrom::from_parts(vec![0; 0x8000], vec![1; CHR_RAM_SIZE], Mirroring::Vertical);
        nrom.ppu_write(0x0010, 0x55);
        assert_eq!(nrom.ppu_read(0x0010), 1);
    }

    #[test]
    fn test_chr_ram_when_no_chr_rom() {
        let mut nrom = Nrom::from_parts(vec![0; 0x8000], vec![], Mirroring::Vertical);
        nrom.ppu_write(0x1FFF, 0x55);
        assert_eq!(nrom.ppu_read(0x1FFF), 0x55);
    }
}
