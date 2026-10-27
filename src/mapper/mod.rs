mod axrom;
mod cnrom;
mod discrete;
mod fme7;
mod mmc1;
mod mmc2;
mod mmc3;
mod mmc5;
mod namco108;
mod nrom;
mod uxrom;
mod vrc4;
mod vrc6;
mod vrc_irq;

pub use axrom::Axrom;
pub use cnrom::Cnrom;
pub use discrete::{Board, Discrete};
pub use fme7::Fme7;
pub use mmc1::Mmc1;
pub use mmc2::Mmc2;
pub use mmc3::Mmc3;
pub use mmc5::Mmc5;
pub use namco108::Namco108;
pub use nrom::Nrom;
pub use uxrom::Uxrom;
pub use vrc4::Vrc4;
pub use vrc6::Vrc6;

use crate::cartridge::{Mirroring, Rom};
use crate::state::{StateReader, StateWriter};
use std::cell::RefCell;
use std::rc::Rc;

pub trait Mapper {
    /// CPU read in $4020-$FFFF. `None` means the cartridge doesn't drive the
    /// data bus at that address, so the CPU sees open bus.
    fn cpu_read(&self, addr: u16) -> Option<u8>;

    /// CPU write in $4020-$FFFF: PRG-RAM, or bank-switching registers on
    /// most mappers.
    fn cpu_write(&mut self, addr: u16, data: u8);

    fn cpu_write_on_cycle(&mut self, addr: u16, data: u8, _cpu_cycle: usize) {
        self.cpu_write(addr, data);
    }

    fn ppu_read(&self, addr: u16) -> u8;

    /// PPU pattern-table write, $0000-$1FFF. Only does something with CHR-RAM.
    fn ppu_write(&mut self, addr: u16, data: u8);

    fn mirroring(&self) -> Mirroring;

    fn ppu_address(&mut self, _addr: u16, _ppu_cycle: u64) {}

    fn irq_pending(&self) -> bool {
        false
    }

    fn watches_ppu_bus(&self) -> bool {
        false
    }

    fn clocked(&self) -> bool {
        false
    }

    fn cpu_clock(&mut self) {}

    fn audio_output(&self) -> f32 {
        0.0
    }

    fn watches_ppu_registers(&self) -> bool {
        false
    }

    fn ppu_register_write(&mut self, _addr: u16, _data: u8) {}

    fn nametable_pages(&self) -> [u8; 4] {
        self.mirroring().nametable_pages()
    }

    fn maps_nametables(&self) -> bool {
        false
    }

    fn nametable_read(&self, _addr: u16) -> Option<u8> {
        None
    }

    fn nametable_write(&mut self, _addr: u16, _data: u8) -> bool {
        false
    }

    fn battery_ram(&self) -> Option<&[u8]> {
        None
    }

    fn load_battery_ram(&mut self, _data: &[u8]) {}

    fn save_state(&self, w: &mut StateWriter);

    fn load_state(&mut self, r: &mut StateReader);
}

pub type SharedMapper = Rc<RefCell<dyn Mapper>>;

/// The mappers the emulator has, by iNES number, with the boards' names.
pub const SUPPORTED: [(u16, &str); 32] = [
    (0, "NROM"),
    (1, "MMC1"),
    (2, "UxROM"),
    (3, "CNROM"),
    (4, "MMC3"),
    (5, "MMC5"),
    (7, "AxROM"),
    (9, "MMC2"),
    (10, "MMC4"),
    (11, "Color Dreams"),
    (13, "CPROM"),
    (21, "VRC4"),
    (22, "VRC2"),
    (23, "VRC2/VRC4"),
    (24, "VRC6"),
    (25, "VRC2/VRC4"),
    (26, "VRC6"),
    (34, "BNROM/NINA-001"),
    (66, "GxROM"),
    (69, "Sunsoft FME-7"),
    (70, "Bandai"),
    (71, "Camerica"),
    (78, "Irem/Jaleco 78"),
    (79, "NINA-03/06"),
    (87, "Jaleco 87"),
    (94, "UN1ROM"),
    (118, "TxSROM"),
    (119, "TQROM"),
    (140, "Jaleco 140"),
    (152, "Bandai 152"),
    (180, "Crazy Climber"),
    (206, "Namco 108"),
];

pub fn from_rom(rom: Rom) -> Result<SharedMapper, String> {
    fn shared(mapper: impl Mapper + 'static) -> Result<SharedMapper, String> {
        Ok(Rc::new(RefCell::new(mapper)))
    }
    if let Some(board) = Board::for_rom(&rom) {
        return shared(Discrete::new(rom, board));
    }
    match rom.mapper {
        0 => shared(Nrom::new(rom)),
        1 => shared(Mmc1::new(rom)),
        2 => shared(Uxrom::new(rom)),
        3 => shared(Cnrom::new(rom)),
        4 | 118 | 119 => shared(Mmc3::new(rom)),
        5 => shared(Mmc5::new(rom)),
        7 => shared(Axrom::new(rom)),
        9 | 10 => shared(Mmc2::new(rom)),
        21 | 22 | 23 | 25 => shared(Vrc4::new(rom)),
        24 | 26 => shared(Vrc6::new(rom)),
        69 => shared(Fme7::new(rom)),
        206 => shared(Namco108::new(rom)),
        n => {
            let supported: Vec<String> = SUPPORTED
                .iter()
                .map(|(number, name)| format!("{number} {name}"))
                .collect();
            Err(format!(
                "mapper {n} is not supported yet (supported: {})",
                supported.join(", ")
            ))
        }
    }
}

fn bank_offset(bank: usize, bank_size: usize, len: usize) -> usize {
    (bank * bank_size) % len
}

fn prg_ram_size(rom: &Rom) -> usize {
    (rom.prg_ram_size + rom.prg_nvram_size).max(0x2000)
}

/// Pattern-table memory: the cartridge's CHR-ROM, or CHR-RAM if it has none.
struct Chr {
    data: Vec<u8>,
    is_ram: bool,
}

impl Chr {
    fn new(rom: &Rom) -> Self {
        if rom.chr_rom.is_empty() {
            let size = (rom.chr_ram_size + rom.chr_nvram_size).max(0x2000);
            Chr {
                data: vec![0; size],
                is_ram: true,
            }
        } else {
            Chr {
                data: rom.chr_rom.clone(),
                is_ram: false,
            }
        }
    }

    fn ram(size: usize) -> Self {
        Chr {
            data: vec![0; size],
            is_ram: true
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

    fn save_state(&self, w: &mut StateWriter) {
        if self.is_ram {
            w.bytes(&self.data);
        }
    }

    fn load_state(&mut self, r: &mut StateReader) {
        if self.is_ram {
            r.bytes_into(&mut self.data);
        }
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
    pub fn banked_rom(
        mapper: u16,
        prg_banks: usize,
        chr_banks_8k: usize,
        has_battery: bool,
    ) -> Rom {
        let mut prg_rom = vec![0; prg_banks * 0x4000];
        for (i, b) in prg_rom.iter_mut().enumerate() {
            let bank = (i / 0x4000) as u8;
            *b = if i % 0x4000 == 0x3FFF {
                0x80 | bank
            } else {
                bank
            };
        }
        let mut chr_rom = vec![0; chr_banks_8k * 0x2000];
        for (i, b) in chr_rom.iter_mut().enumerate() {
            let bank = (i / 0x400) as u8;
            *b = if i % 0x400 == 0x3FF {
                0x80 | bank
            } else {
                bank
            };
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
        pub irq: bool,
        a12_high: bool,
        a12_low_since: u64,
        /// (ppu_cycle / 341, ppu_cycle % 341) of each A12 rise
        pub rises_at: Vec<(u64, u64)>,
        pub writes: Vec<(u16, u8)>,
    }

    impl ScanlineCounter {
        pub fn new(prg_rom: Vec<u8>, irq_every: Option<usize>) -> Self {
            ScanlineCounter {
                prg_rom,
                chr: vec![0; 0x2000],
                scanlines: 0,
                irq_every,
                irq: false,
                a12_high: false,
                a12_low_since: 0,
                rises_at: vec![],
                writes: vec![],
            }
        }
    }

    impl super::Mapper for ScanlineCounter {
        fn save_state(&self, _w: &mut crate::state::StateWriter) {}

        fn load_state(&mut self, _r: &mut crate::state::StateReader) {}

        fn cpu_read(&self, addr: u16) -> Option<u8> {
            (addr >= 0x8000).then(|| self.prg_rom[(addr as usize - 0x8000) % self.prg_rom.len()])
        }

        fn cpu_write(&mut self, addr: u16, data: u8) {
            self.writes.push((addr, data));
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

        /// Counts rising edges of A12 after it was low for 10+ dots, like
        /// MMC3 (the PPU drops A12 briefly between sprite fetches).
        fn ppu_address(&mut self, addr: u16, ppu_cycle: u64) {
            let a12 = addr & 0x1000 != 0;
            if !a12 && self.a12_high {
                self.a12_low_since = ppu_cycle;
            }
            if a12 && !self.a12_high && ppu_cycle - self.a12_low_since >= 10 {
                self.scanlines += 1;
                self.rises_at.push((ppu_cycle / 341, ppu_cycle % 341));
                if let Some(n) = self.irq_every {
                    if self.scanlines % n == 0 {
                        self.irq = true;
                    }
                }
            }
            self.a12_high = a12;
        }

        fn irq_pending(&self) -> bool {
            self.irq
        }

        fn watches_ppu_bus(&self) -> bool {
            true
        }
    }
}
