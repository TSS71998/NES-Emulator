mod axrom;
mod cnrom;
mod mmc1;
mod nrom;
mod uxrom;
mod mmc3;

pub use axrom::Axrom;
pub use cnrom::Cnrom;
pub use mmc1::Mmc1;
pub use nrom::Nrom;
pub use uxrom::Uxrom;
pub use mmc3::Mmc3;

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

    /// Called once per scanline while rendering is enabled: after each of
    /// the 240 visible lines and the pre-render line, at the point where the
    /// PPU fetches sprite patterns (about dot 260). This is when MMC3's IRQ
    /// counter is clocked, by the PPU's A12 address line going high.
    fn scanline(&mut self) {}

    /// Called with the PPU's VRAM address whenever it changes while
    /// rendering is off: after the second $2006 write and after each $2007
    /// access. On real hardware the cartridge sees this address on the PPU
    /// bus, so MMC3 can be clocked by toggling bit 12 (A12) through $2006.
    fn ppu_address(&mut self, _addr: u16) {}

    /// Level of the cartridge's IRQ line. The CPU takes the interrupt when
    /// this is true and its I flag is clear, so a mapper keeps returning true
    /// until the game acknowledges the IRQ (for MMC3, a write to $E000).
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
        4 => Ok(Rc::new(RefCell::new(Mmc3::new(rom)))),
        7 => Ok(Rc::new(RefCell::new(Axrom::new(rom)))),
        n => {
            return Err(format!(
                "mapper {} is not supported yet (supported: 0 NROM, 1 MMC1, 2 UxROM, 3 CNROM, 4 MMC2, 7 AxROM)",
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
    /// which bank is mapped. The last byte of each bank is tagged with
    /// `0x80 | bank` instead, so a read that lands in the right bank but at
    /// the wrong offset fails too.
    pub fn banked_rom(mapper: u16, prg_banks: usize, chr_banks_8k: usize, has_battery: bool) -> Rom {
        let mut prg_rom = vec![0; prg_banks * 0x4000];
        for (i, b) in prg_rom.iter_mut().enumerate() {
            let bank = (i / 0x4000) as u8;            
            *b = if i % 0x4000 == 0x3FFF { 0x80 | bank } else { bank };
        }
        let mut chr_rom = vec![0; chr_banks_8k * 0x2000];
        for (i, b) in chr_rom.iter_mut().enumerate() {
            let bank = (i / 0x400) as u8;            
            *b = if i % 0x400 == 0x3FF { 0x80 | bank } else { bank };
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

    pub struct ScanlineCounter {
        pub prg_rom: Vec<u8>,
        chr: Vec<u8>,
        pub scanlines: usize,
        pub irq_every: Option<usize>,
        pub irq: bool
    }

    impl ScanlineCounter {
        pub fn new(prg_rom:Vec<u8>, irq_every: Option<usize>) -> Self {
            ScanlineCounter { prg_rom, chr: vec![0; 0x2000], scanlines: 0, irq_every, irq: false }
        }
    }

    impl super::Mapper for ScanlineCounter {
        fn cpu_read(&self, addr: u16) -> Option<u8> {
            (addr >= 0x8000).then(|| self.prg_rom[(addr as usize - 0x8000) % self.prg_rom.len()])
        }

        fn cpu_write(&mut self, addr: u16, _data: u8) {
            if addr >= 0xE000 {
                self.irq = false
            }
        }

        fn ppu_read(&self, addr: u16) -> u8 {
            self.chr[addr as usize & 0x1FFF]
        }

        fn ppu_write(&mut self, addr: u16, data: u8) {
            self.chr[addr as usize & 0x1FFF] = data;
        }

        fn mirroring(&self) -> Mirroring {
            Mirroring::Vertical
        }

        fn scanline(&mut self) {
            self.scanlines += 1;
            if let Some(n) = self.irq_every {
                if self.scanlines % n == 0 {
                    self.irq = true;
                }
            }
        }

        fn irq_pending(&self) -> bool {
            self.irq
        }
    }
}