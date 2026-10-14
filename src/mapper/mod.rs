mod axrom;
mod cnrom;
mod mmc1;
mod nrom;
mod uxrom;

pub use axrom::Axrom;
pub use cnrom::Cnrom;
pub use mmc1::Mmc1;
pub use nrom::Nrom;
pub use uxrom::Uxrom;

use crate::cartridge::{Mirroring, Rom};
use std::cell::RefCell;
use std::rc::Rc;

pub trait Mapper {
    /// CPU read in $4020-$FFFF. `None` means the cartridge doesn't drive the
    /// data bus at that address, so the CPU sees open bus.
    fn cpu_read(&self, addr: u16) -> Option<u8>;

    /// CPU write in $4020-$FFFF: PRG-RAM, or bank-switching registers on
    /// most mappers.
    fn cpu_write(&mut self, addr: u16, data: u8);

    /// PPU pattern-table read, $0000-$1FFF. Takes `&self` because the
    /// renderer only has shared access to the PPU; mappers that latch on PPU
    /// reads (MMC2/MMC4) can use a `Cell` for that state.
    fn ppu_read(&self, addr: u16) -> u8;

    /// PPU pattern-table write, $0000-$1FFF. Only does something with CHR-RAM.
    fn ppu_write(&mut self, addr: u16, data: u8);

    fn mirroring(&self) -> Mirroring;

    fn irq_pending(&self) -> bool {
        false
    }

    /// Battery-backed PRG-RAM to write to a .sav file, if the cartridge has any.
    fn battery_ram(&self) -> Option<&[u8]> {
        None
    }

    /// Restores battery-backed PRG-RAM from a .sav file.
    fn load_battery_ram(&mut self, _data: &[u8]) {}
}

pub type SharedMapper = Rc<RefCell<dyn Mapper>>;

pub fn from_rom(rom: Rom) -> Result<SharedMapper, String> {
    match rom.mapper {
        0 => Ok(Rc::new(RefCell::new(Nrom::new(rom)))),
        1 => Ok(Rc::new(RefCell::new(Mmc1::new(rom)))),
        2 => Ok(Rc::new(RefCell::new(Uxrom::new(rom)))),
        3 => Ok(Rc::new(RefCell::new(Cnrom::new(rom)))),
        7 => Ok(Rc::new(RefCell::new(Axrom::new(rom)))),
        n => {
            return Err(format!(
                "mapper {} is not supported yet (supported: 0 NROM, 1 MMC1, 2 UxROM, 3 CNROM, 7 AxROM)",
                n
            ))
        }
    }
}

/// Offset into a ROM/RAM of `len` bytes for `bank` of `bank_size` bytes.
/// Bank numbers wrap, because games often write bank numbers with unused
/// high bits set, and smaller carts ignore the extra address lines.
fn bank_offset(bank: usize, bank_size: usize, len: usize) -> usize {
    (bank * bank_size) % len
}

/// PRG-RAM size for carts that have it: whatever the header says, at least 8 KiB.
/// (iNES headers often leave the size as 0.)
fn prg_ram_size(rom: &Rom) -> usize {
    (rom.prg_ram_size + rom.prg_nvram_size).max(0x2000)
}

/// Pattern-table memory: the cartridge's CHR-ROM, or CHR-RAM if it has none.
struct Chr {
    data: Vec<u8>,
    is_ram: bool
}

impl Chr {
    fn new(rom: &Rom) -> Self {
        if rom.chr_rom.is_empty() {
            let size = (rom.chr_ram_size + rom.chr_nvram_size).max(0x2000);
            Chr { data: vec![0; size], is_ram: true }
        } else {
            Chr { data: rom.chr_rom.clone(), is_ram: false }
        }
    }

    fn read(&self, offset: usize) -> u8 {
        self.data[offset % self.data.len()]
    }

    fn write(&mut self, offset: usize, data: u8) {
        if self.is_ram {
            let len = self.data.len();
            self.data[offset % len] = data;
        }
    }

    fn len(&self) -> usize {
        self.data.len()
    }
}

#[cfg(test)]
pub(crate) mod test_util {
    use crate::cartridge::{ConsoleType, Mirroring, Rom, RomFormat, Timing};

    /// A Rom for mapper tests. Each 16 KiB PRG bank is filled with its bank
    /// number and each 1 KiB CHR bank with its bank number, so a read shows
    /// which bank is mapped.
    pub fn banked_rom(mapper: u16, prg_banks: usize, chr_banks_8k: usize, has_battery: bool) -> Rom {
        let mut prg_rom = vec![0; prg_banks * 0x4000];
        for (i, b) in prg_rom.iter_mut().enumerate() {
            *b = (i / 0x4000) as u8;
        }
        let mut chr_rom = vec![0; chr_banks_8k * 0x2000];
        for (i, b) in chr_rom.iter_mut().enumerate() {
            *b = (i / 0x400) as u8;
        }
        Rom {
            prg_rom,
            chr_rom,
            mapper,
            submapper: 0,
            screen_mirroring: Mirroring::Horizontal,
            format: RomFormat::INes,
            has_battery,
            console_type: ConsoleType::Nes,
            timing: Timing::Ntsc,
            prg_ram_size: 0,
            prg_nvram_size: 0,
            chr_ram_size: 0,
            chr_nvram_size: 0,
        }
    }
}