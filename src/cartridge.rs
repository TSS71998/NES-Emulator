const NES_TAG: [u8; 4] = [0x4E, 0x45, 0x53, 0x1A];
const HEADER_SIZE: usize = 16;
const TRAINER_SIZE: usize = 512;
const PRG_ROM_PAGE_SIZE: usize = 16384;
const CHR_ROM_PAGE_SIZE: usize = 8192;

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Mirroring {
    Vertical,
    Horizontal,
    FourScreen,
    SingleScreenLower,
    SingleScreenUpper,
}

impl Mirroring {
    pub fn nametable_pages(self) -> [u8; 4] {
        match self {
            Mirroring::Vertical => [0, 1, 0, 1],
            Mirroring::Horizontal => [0, 0, 1, 1],
            Mirroring::FourScreen => [0, 1, 2, 3],
            Mirroring::SingleScreenLower => [0; 4],
            Mirroring::SingleScreenUpper => [1; 4],
        }
    }
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum RomFormat {
    INes,
    ArchaicINes,
    Nes2,
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum ConsoleType {
    Nes,
    VsSystem,
    Playchoice10,
    Extended(u8),
}

#[derive(Debug, PartialEq, Clone, Copy)]
pub enum Timing {
    Ntsc,
    Pal,
    MultiRegion,
    Dendy,
}

pub struct Rom {
    pub prg_rom: Vec<u8>,
    pub chr_rom: Vec<u8>,

    pub mapper: u16,
    pub submapper: u8,
    pub screen_mirroring: Mirroring,
    pub format: RomFormat,
    pub has_battery: bool,
    pub console_type: ConsoleType,
    pub timing: Timing,

    pub prg_ram_size: usize,
    pub prg_nvram_size: usize,
    pub chr_ram_size: usize,
    pub chr_nvram_size: usize,
}

impl Rom {
    pub fn new(raw: &[u8]) -> Result<Rom, String> {
        if raw.len() < HEADER_SIZE || raw[0..4] != NES_TAG {
            return Err("File is not in iNES file format".to_string());
        }

        let format = detect_format(raw);

        let four_screen = raw[6] & 0b1000 != 0;
        let has_trainer = raw[6] & 0b100 != 0;
        let has_battery = raw[6] & 0b10 != 0;
        let vertical_mirroring = raw[6] & 0b1 != 0;
        let screen_mirroring = match (four_screen, vertical_mirroring) {
            (true, _) => Mirroring::FourScreen,
            (false, true) => Mirroring::Vertical,
            (false, false) => Mirroring::Horizontal,
        };

        let (prg_rom_size, chr_rom_size) = match format {
            RomFormat::Nes2 => (
                nes2_rom_size(raw[4], raw[9] & 0x0f, PRG_ROM_PAGE_SIZE)?,
                nes2_rom_size(raw[5], raw[9] >> 4, CHR_ROM_PAGE_SIZE)?,
            ),
            _ => (
                raw[4] as usize * PRG_ROM_PAGE_SIZE,
                raw[5] as usize * CHR_ROM_PAGE_SIZE,
            ),
        };

        let mapper_lo = (raw[6] >> 4) as u16;
        let mapper_mid = (raw[7] & 0xf0) as u16;
        let (mapper, submapper) = match format {
            RomFormat::Nes2 => (
                mapper_lo | mapper_mid | ((raw[8] & 0x0f) as u16) << 8,
                raw[8] >> 4,
            ),
            RomFormat::INes => (mapper_lo | mapper_mid, 0),
            RomFormat::ArchaicINes => (mapper_lo, 0),
        };

        let console_type = match format {
            RomFormat::Nes2 => match raw[7] & 0b11 {
                0 => ConsoleType::Nes,
                1 => ConsoleType::VsSystem,
                2 => ConsoleType::Playchoice10,
                _ => ConsoleType::Extended(raw[13] & 0x0f),
            },
            RomFormat::INes if raw[7] & 0b01 != 0 => ConsoleType::VsSystem,
            RomFormat::INes if raw[7] & 0b10 != 0 => ConsoleType::Playchoice10,
            _ => ConsoleType::Nes,
        };

        let timing = match format {
            RomFormat::Nes2 => match raw[12] & 0b11 {
                0 => Timing::Ntsc,
                1 => Timing::Pal,
                2 => Timing::MultiRegion,
                _ => Timing::Dendy,
            },
            RomFormat::INes if raw[9] & 1 != 0 => Timing::Pal,
            _ => Timing::Ntsc,
        };

        let (prg_ram_size, prg_nvram_size, chr_ram_size, chr_nvram_size) = match format {
            RomFormat::Nes2 => (
                // byte 10: PRG-RAM (low nibble) / PRG-NVRAM (high nibble) shift counts
                // byte 11: CHR-RAM (low nibble) / CHR-NVRAM (high nibble) shift counts
                ram_size_from_shift(raw[10] & 0x0f),
                ram_size_from_shift(raw[10] >> 4),
                ram_size_from_shift(raw[11] & 0x0f),
                ram_size_from_shift(raw[11] >> 4),
            ),
            // iNES has no reliable RAM fields: byte 8 is PRG-RAM in 8 KiB units
            // (0 means 8 KiB), and a cart with no CHR-ROM has 8 KiB of CHR-RAM.
            _ => {
                let prg_ram = if format == RomFormat::INes {
                    (raw[8].max(1)) as usize * 8192
                } else {
                    8192
                };
                let chr_ram = if chr_rom_size == 0 { 8192 } else { 0 };
                if has_battery {
                    (0, prg_ram, chr_ram, 0)
                } else {
                    (prg_ram, 0, chr_ram, 0)
                }
            }
        };

        let trainer_size = if has_trainer { TRAINER_SIZE } else { 0 };
        let prg_rom_start = HEADER_SIZE + trainer_size;
        let chr_rom_start = prg_rom_start + prg_rom_size;

        match rom_end(trainer_size, prg_rom_size, chr_rom_size) {
            Some(end) if end <= raw.len() => {}
            _ => {
                return Err(format!(
                    "ROM file is truncated: the header describes {} KiB of PRG-ROM and {} KiB of CHR-ROM, but the file is only {} bytes",
                    prg_rom_size / 1024,
                    chr_rom_size / 1024,
                    raw.len()
                ));
            }
        }

        Ok(Rom {
            prg_rom: raw[prg_rom_start..(prg_rom_start + prg_rom_size)].to_vec(),
            chr_rom: raw[chr_rom_start..(chr_rom_start + chr_rom_size)].to_vec(),
            mapper,
            submapper,
            screen_mirroring,
            format,
            has_battery,
            console_type,
            timing,
            prg_ram_size,
            prg_nvram_size,
            chr_ram_size,
            chr_nvram_size,
        })
    }
}

fn detect_format(raw: &[u8]) -> RomFormat {
    match raw[7] & 0x0c {
        0x08 if nes2_sizes_fit(raw) => RomFormat::Nes2,
        0x00 if raw[12..16].iter().all(|&b| b == 0) => RomFormat::INes,
        _ => RomFormat::ArchaicINes,
    }
}

fn nes2_sizes_fit(raw: &[u8]) -> bool {
    let trainer_size = if raw[6] & 0b100 != 0 { TRAINER_SIZE } else { 0 };
    let prg = nes2_rom_size(raw[4], raw[9] & 0x0f, PRG_ROM_PAGE_SIZE);
    let chr = nes2_rom_size(raw[5], raw[9] >> 4, CHR_ROM_PAGE_SIZE);
    match (prg, chr) {
        (Ok(prg), Ok(chr)) => rom_end(trainer_size, prg, chr).is_some_and(|end| end <= raw.len()),
        _ => false,
    }
}

fn rom_end(trainer_size: usize, prg_rom_size: usize, chr_rom_size: usize) -> Option<usize> {
    HEADER_SIZE
        .checked_add(trainer_size)?
        .checked_add(prg_rom_size)?
        .checked_add(chr_rom_size)
}

fn nes2_rom_size(lsb: u8, msb_nibble: u8, unit: usize) -> Result<usize, String> {
    if msb_nibble == 0x0f {
        let exponent = (lsb >> 2) as u32;
        let multiplier = (lsb & 0b11) as usize * 2 + 1;
        1usize
            .checked_shl(exponent)
            .and_then(|size| size.checked_mul(multiplier))
            .ok_or_else(|| "NES 2.0 header gives a ROM size too large to load".to_string())
    } else {
        Ok(((msb_nibble as usize) << 8 | lsb as usize) * unit)
    }
}

fn ram_size_from_shift(shift: u8) -> usize {
    if shift == 0 { 0 } else { 64 << shift }
}

#[cfg(test)]
pub mod test {
    use super::*;
    use crate::mapper::{self, SharedMapper};

    struct TestRom {
        header: Vec<u8>,
        trainer: Option<Vec<u8>>,
        prg_rom: Vec<u8>,
        chr_rom: Vec<u8>,
    }

    fn create_rom(rom: TestRom) -> Vec<u8> {
        let mut result = Vec::with_capacity(
            rom.header.len()
                + rom.trainer.as_ref().map_or(0, |t| t.len())
                + rom.prg_rom.len()
                + rom.chr_rom.len(),
        );

        result.extend(&rom.header);
        if let Some(t) = rom.trainer {
            result.extend(t);
        }

        result.extend(&rom.prg_rom);
        result.extend(&rom.chr_rom);

        result
    }

    pub fn test_mapper() -> SharedMapper {
        test_mapper_containing(vec![])
    }

    /// NROM cartridge with `program` at $8000 and the rest of the 32 KiB zero-filled.
    pub fn test_mapper_containing(program: Vec<u8>) -> SharedMapper {
        mapper::from_rom(test_rom_containing(program)).unwrap()
    }

    pub fn test_rom_containing(program: Vec<u8>) -> Rom {
        let mut prg_rom_contents = program;
        prg_rom_contents.resize(2 * PRG_ROM_PAGE_SIZE, 0);

        let test_rom = create_rom(TestRom {
            header: vec![
                0x4E, 0x45, 0x53, 0x1A, 0x02, 0x01, 0x01, 00, 00, 00, 00, 00, 00, 00, 00, 00,
            ],
            trainer: None,
            prg_rom: prg_rom_contents,
            chr_rom: vec![2; CHR_ROM_PAGE_SIZE],
        });

        Rom::new(&test_rom).unwrap()
    }

    #[test]
    fn test() {
        let test_rom = create_rom(TestRom {
            header: vec![
                0x4E, 0x45, 0x53, 0x1A, 0x02, 0x01, 0x31, 00, 00, 00, 00, 00, 00, 00, 00, 00,
            ],
            trainer: None,
            prg_rom: vec![1; 2 * PRG_ROM_PAGE_SIZE],
            chr_rom: vec![2; CHR_ROM_PAGE_SIZE],
        });

        let rom: Rom = Rom::new(&test_rom).unwrap();

        assert_eq!(rom.chr_rom, vec!(2; CHR_ROM_PAGE_SIZE));
        assert_eq!(rom.prg_rom, vec!(1; 2 * PRG_ROM_PAGE_SIZE));
        assert_eq!(rom.mapper, 3);
        assert_eq!(rom.screen_mirroring, Mirroring::Vertical);
        assert_eq!(rom.format, RomFormat::INes);
    }

    #[test]
    fn test_nes2_header() {
        let test_rom = create_rom(TestRom {
            header: vec![
                0x4E, 0x45, 0x53, 0x1A, 0x02, // 2 x 16 KiB PRG-ROM
                0x01, // 1 x 8 KiB CHR-ROM
                0x31, // mapper bits 0-3 = 3, vertical mirroring
                0x08, // NES 2.0 identifier, mapper bits 4-7 = 0
                0x21, // submapper 2, mapper bits 8-11 = 1
                0x00, // ROM size MSBs
                0x70, // PRG-NVRAM 64 << 7 = 8 KiB, no PRG-RAM
                0x07, // CHR-RAM 64 << 7 = 8 KiB
                0x01, // PAL
                00, 00, 00,
            ],
            trainer: None,
            prg_rom: vec![1; 2 * PRG_ROM_PAGE_SIZE],
            chr_rom: vec![2; CHR_ROM_PAGE_SIZE],
        });

        let rom = Rom::new(&test_rom).unwrap();

        assert_eq!(rom.format, RomFormat::Nes2);
        assert_eq!(rom.mapper, 0x103);
        assert_eq!(rom.submapper, 2);
        assert_eq!(rom.prg_rom, vec!(1; 2 * PRG_ROM_PAGE_SIZE));
        assert_eq!(rom.chr_rom, vec!(2; CHR_ROM_PAGE_SIZE));
        assert_eq!(rom.screen_mirroring, Mirroring::Vertical);
        assert_eq!(rom.prg_ram_size, 0);
        assert_eq!(rom.prg_nvram_size, 8192);
        assert_eq!(rom.chr_ram_size, 8192);
        assert_eq!(rom.chr_nvram_size, 0);
        assert_eq!(rom.timing, Timing::Pal);
        assert_eq!(rom.console_type, ConsoleType::Nes);
    }

    #[test]
    fn test_nes2_exponent_rom_size() {
        let test_rom = create_rom(TestRom {
            header: vec![
                0x4E, 0x45, 0x53, 0x1A, 0x02,
                0x34, // with MSB nibble $F: 2^13 * 1 = 8 KiB of CHR-ROM
                0x00, 0x08, 0x00, 0xF0, // CHR size uses exponent-multiplier notation
                00, 00, 00, 00, 00, 00,
            ],
            trainer: None,
            prg_rom: vec![1; 2 * PRG_ROM_PAGE_SIZE],
            chr_rom: vec![2; 8192],
        });

        let rom = Rom::new(&test_rom).unwrap();

        assert_eq!(rom.format, RomFormat::Nes2);
        assert_eq!(rom.prg_rom.len(), 2 * PRG_ROM_PAGE_SIZE);
        assert_eq!(rom.chr_rom, vec!(2; 8192));
    }

    #[test]
    fn test_nes2_rom_size_encodings() {
        assert_eq!(
            nes2_rom_size(0x02, 0x1, PRG_ROM_PAGE_SIZE),
            Ok(0x102 * PRG_ROM_PAGE_SIZE)
        );
        // 0b000111_01: 2^7 * (1 * 2 + 1) = 384 bytes
        assert_eq!(nes2_rom_size(0x1D, 0xF, PRG_ROM_PAGE_SIZE), Ok(384));
        // 2^63 * 7 doesn't fit in usize
        assert!(nes2_rom_size(0xFF, 0xF, PRG_ROM_PAGE_SIZE).is_err());
    }

    #[test]
    fn test_nes2_flag_with_impossible_size_is_treated_as_archaic() {
        // byte 7 says NES 2.0, but byte 9 would make PRG-ROM 4 MiB, far more than the file
        let test_rom = create_rom(TestRom {
            header: vec![
                0x4E, 0x45, 0x53, 0x1A, 0x02, 0x01, 0x31, 0x18, 00, 0x01, 00, 00, 00, 00, 00, 00,
            ],
            trainer: None,
            prg_rom: vec![1; 2 * PRG_ROM_PAGE_SIZE],
            chr_rom: vec![2; CHR_ROM_PAGE_SIZE],
        });

        let rom = Rom::new(&test_rom).unwrap();

        assert_eq!(rom.format, RomFormat::ArchaicINes);
        assert_eq!(rom.mapper, 3);
        assert_eq!(rom.prg_rom.len(), 2 * PRG_ROM_PAGE_SIZE);
    }

    #[test]
    fn test_archaic_ines_ignores_junk_in_header() {
        let mut header = vec![0x4E, 0x45, 0x53, 0x1A, 0x02, 0x01, 0x31];
        header.extend(b"DiskDude!");
        let test_rom = create_rom(TestRom {
            header,
            trainer: None,
            prg_rom: vec![1; 2 * PRG_ROM_PAGE_SIZE],
            chr_rom: vec![2; CHR_ROM_PAGE_SIZE],
        });

        let rom = Rom::new(&test_rom).unwrap();

        assert_eq!(rom.format, RomFormat::ArchaicINes);
        assert_eq!(rom.mapper, 3); // not 0x43 from the 'D' in byte 7
        assert_eq!(rom.timing, Timing::Ntsc);
    }

    #[test]
    fn test_truncated_rom_is_an_error() {
        let test_rom = create_rom(TestRom {
            header: vec![
                0x4E, 0x45, 0x53, 0x1A, 0x02, 0x01, 0x31, 00, 00, 00, 00, 00, 00, 00, 00, 00,
            ],
            trainer: None,
            prg_rom: vec![1; PRG_ROM_PAGE_SIZE], // header says 2 pages
            chr_rom: vec![],
        });

        let err = Rom::new(&test_rom).err().unwrap();
        assert!(err.contains("truncated"), "{}", err);
    }

    #[test]
    fn test_not_a_nes_file() {
        assert!(Rom::new(&[0u8; 64]).is_err());
        assert!(Rom::new(&[0x4E, 0x45, 0x53]).is_err());
    }
}
