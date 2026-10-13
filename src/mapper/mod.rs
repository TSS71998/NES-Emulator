mod nrom;

pub use nrom::Nrom;

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
}

pub type SharedMapper = Rc<RefCell<dyn Mapper>>;

pub fn from_rom(rom: Rom) -> Result<SharedMapper, String> {
    match rom.mapper {
        0 => Ok(Rc::new(RefCell::new(Nrom::new(rom)))),
        n => Err(format!("mapper {} is not supported yet", n))
    }
}