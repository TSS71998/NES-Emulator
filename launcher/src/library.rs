//! The game library: finding ROMs in the user's folders, and what the
//! launcher shows about each one (title, header details, and the region and
//! console the emulator will pick for it).
//!
//! The header parsing and the region/console guesses mirror the emulator's
//! `cartridge.rs` and `region.rs`, so the launcher shows what the emulator
//! will actually do. If those rules change there, change them here too.

use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub const SUPPORTED_MAPPERS: [u16; 6] = [0, 1, 2, 3, 4, 7];

const MAX_SCAN_DEPTH: usize = 8;

const MAX_ROM_SIZE: u64 = 16 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Region {
    Ntsc,
    Pal,
    Dendy
}

impl Region {
    pub const ALL: [Region; 3] = [Region::Ntsc, Region::Pal, Region::Dendy];

    pub fn name(self) -> &'static str {
        match self {
            Region::Ntsc => "NTSC",
            Region::Pal => "PAL",
            Region::Dendy => "Dendy"
        }
    }

    pub fn arg(self) -> &'static str {
        match self {
            Region::Ntsc => "ntsc",
            Region::Pal => "pal",
            Region::Dendy => "dendy"
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Console {
    Nes,
    TopLoader,
    Famicom
}

impl Console {
    pub const ALL: [Console; 3] = [Console::Nes, Console::TopLoader, Console::Famicom];

    pub fn name(self) -> &'static str {
        match self {
            Console::Nes => "NES",
            Console::TopLoader => "Top-loading NES",
            Console::Famicom => "Famicom",
        }
    }

    pub fn arg(self) -> &'static str {
        match self {
            Console::Nes => "nes",
            Console::TopLoader => "toploader",
            Console::Famicom => "famicom",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderFormat {
    ArchaicINes,
    INes,
    Nes2
}

impl HeaderFormat {
    pub fn name(self) -> &'static str {
        match self {
            HeaderFormat::ArchaicINes => "Archaic iNES",
            HeaderFormat::INes => "iNES",
            HeaderFormat::Nes2 => "NES 2.0"
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Timing {
    Ntsc,
    Pal,
    MultiRegion,
    Dendy
}

#[derive(Clone, Debug, PartialEq)]
pub struct RomInfo {
    pub format: HeaderFormat,
    pub mapper: u16,
    pub prg_rom: usize,
    pub chr_rom: usize,
    pub battery: bool,
    pub timing: Timing
}

const HEADER_SIZE: usize = 16;
const TRAINER_SIZE: usize = 512;
const PRG_ROM_PAGE_SIZE: usize = 16384;
const CHR_ROM_PAGE_SIZE: usize = 8192;

impl RomInfo {
    pub fn parse(raw: &[u8]) -> Result<RomInfo, String> {
        if raw.len() < HEADER_SIZE || raw[0..4] != [0x4E, 0x45, 0x53, 0x1A] {
            return Err("not an iNES file".to_string());
        }
        let format = match raw[7] & 0x0C {
            0x08 if nes2_sizes_fit(raw) => HeaderFormat::Nes2,
            0x00 if raw[12..16].iter().all(|&b| b == 0) => HeaderFormat::INes,
            _ => HeaderFormat::ArchaicINes,
        };
        let (prg_rom, chr_rom) = match format {
            HeaderFormat::Nes2 => (
                nes2_rom_size(raw[4], raw[9] & 0x0F, PRG_ROM_PAGE_SIZE)?,
                nes2_rom_size(raw[5], raw[9] >> 4, CHR_ROM_PAGE_SIZE)?,
            ),
            _ => (
                raw[4] as usize * PRG_ROM_PAGE_SIZE,
                raw[5] as usize * CHR_ROM_PAGE_SIZE,
            )
        };

        let mapper_lo = (raw[6] >> 4) as u16;
        let mapper_mid = (raw[7] & 0xF0) as u16;
        let mapper = match format {
            HeaderFormat::Nes2 => mapper_lo | mapper_mid | ((raw[8] & 0x0F) as u16) << 8,
            HeaderFormat::INes => mapper_lo | mapper_mid,
            HeaderFormat::ArchaicINes => mapper_lo
        };

        let timing = match format {
            HeaderFormat::Nes2 => match raw[12] & 0b11 {
                0 => Timing::Ntsc,
                1 => Timing::Pal,
                2 => Timing::MultiRegion,
                _ => Timing::Dendy,
            },
            HeaderFormat::INes if raw[9] & 1 != 0 => Timing::Pal,
            _ => Timing::Ntsc
        };
        let trainer = if raw[6] & 0b100 != 0 { TRAINER_SIZE } else { 0 };
        match rom_end(trainer, prg_rom, chr_rom) {
            Some(end) if end <= raw.len() => {}
            _ => return Err("the file is shorter than its header says (truncated)".to_string())
        }
        Ok(RomInfo {
            format,
            mapper,
            prg_rom,
            chr_rom,
            battery: raw[6] & 0b10 != 0,
            timing
        })
    }

    pub fn supported(&self) -> bool {
        SUPPORTED_MAPPERS.contains(&self.mapper)
    }
}

fn nes2_sizes_fit(raw: &[u8]) -> bool {
    let trainer = if raw[6] & 0b100 != 0 { TRAINER_SIZE } else { 0 }; 
    let prg = nes2_rom_size(raw[4], raw[9] & 0x0F, PRG_ROM_PAGE_SIZE);
    let chr = nes2_rom_size(raw[5], raw[9] >> 4, CHR_ROM_PAGE_SIZE);
    match (prg, chr) {
        (Ok(prg), Ok(chr)) => rom_end(trainer, prg, chr).is_some_and(|end| end <= raw.len()),
        _ => false
    }
}

fn rom_end(trainer: usize, prg: usize, chr: usize) -> Option<usize> {
    HEADER_SIZE
        .checked_add(trainer)?
        .checked_add(prg)?
        .checked_add(chr)
}

fn nes2_rom_size(lsb: u8, msb_nibble: u8, unit: usize) -> Result<usize, String> {
    if msb_nibble == 0x0F {
        let exponent = (lsb >> 2) as u32;
        let multiplier = (lsb & 0b11) as usize * 2 + 1;
        1usize
            .checked_shl(exponent)
            .and_then(|size| size.checked_mul(multiplier))
            .ok_or_else(|| "the header gives an impossible ROM size".to_string())
    } else {
        Ok((((msb_nibble as usize) << 8) | lsb as usize) * unit)
    }
}

pub fn mapper_name(mapper: u16) -> Option<&'static str> {
    Some(match mapper {
        0 => "NROM",
        1 => "MMC1",
        2 => "UxROM",
        3 => "CNROM",
        4 => "MMC3",
        5 => "MMC5",
        7 => "AxROM",
        9 => "MMC2",
        10 => "MMC4",
        11 => "Color Dreams",
        19 => "Namco 163",
        23 | 25 => "VRC2/VRC4",
        24 | 26 => "VRC6",
        66 => "GxROM",
        69 => "Sunsoft FME-7",
        71 => "Camerica",
        85 => "VRC7",
        206 => "DxROM",
        _ => return None,
    })
}

pub fn auto_region(rom: &RomInfo, file_name: &str) -> (Region, &'static str) {
    match rom.timing {
        Timing::Pal => return (Region::Pal, "ROM header"),
        Timing::Dendy => return (Region::Dendy, "ROM header"),
        Timing::Ntsc | Timing::MultiRegion => {}
    }
    if rom.format == HeaderFormat::Nes2 {
        return (Region::Ntsc, "ROM header");
    }
    let name = file_name.to_ascii_lowercase();
    let pal_tags = ["(e)", "(europe)", "(pal)", "(eu)", "(a)", "(australia)"];
    if pal_tags.iter().any(|tag| name.contains(tag)) {
        return (Region::Pal, "file name");
    }
    (Region::Ntsc, "default")
}

pub fn auto_console(file_name: &str) -> (Console, &'static str) {
    let name = file_name.to_ascii_lowercase();
    if ["(j)", "(japan)"].iter().any(|tag| name.contains(tag)) {
        return (Console::Famicom, "file name");
    }
    (Console::Nes, "default")
}

pub struct Game {
    pub path: PathBuf,
    pub file_name: String,
    pub title: String,
    pub tags: Vec<String>,
    pub size: u64,
    pub rom: Result<RomInfo, String>
}

impl Game {
    pub fn load(path: &Path) -> Game {
        let file_name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let stem = path
            .file_stem()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_default();
        let (title, tags) = clean_title(&stem);
        let size = std::fs::metadata(path).map(|m| m.len()).unwrap_or(0);
        let rom = if size > MAX_ROM_SIZE {
            Err("the file is too big to be an NES game".to_string())
        } else {
            std::fs::read(path)
                .map_err(|e| format!("can't read the file: {e}"))
                .and_then(|raw| RomInfo::parse(&raw))
        };
        Game {
            path: path.to_path_buf(),
            file_name,
            title,
            tags,
            size,
            rom
        }
    }

    pub fn key(&self) -> String {
        self.path.to_string_lossy().into_owned()
    }

    pub fn problem(&self) -> Option<String> {
        match &self.rom {
            Err(e) => Some(format!("Can't use this file: {e}.")),
            Ok(rom) if !rom.supported() => Some(format!(
                "Mapper {} isn't supported by the emulator yet.",
                rom.mapper
            )),
            Ok(_) => None
        }
    }
}

pub fn clean_title(stem: &str) -> (String, Vec<String>) {
    let mut title = String::new();
    let mut tags = Vec::new();
    let mut rest = stem;
    while let Some(open) = rest.find(['(','[']) {
        let close_char = if rest[open..].starts_with('(') {
            ')'
        } else {
            ']'
        };
        let Some(close) = rest[open..].find(close_char) else {
            break;
        };
        title.push_str(&rest[..open]);
        let tag = rest[open + 1..open + close].trim();
        if !tag.is_empty() {
            tags.push(tag.to_string());
        }
        rest = &rest[open + close + 1..];
    }
    title.push_str(rest);

    if !title.contains(' ') {
        title = title.replace('_', " ");
    }
    let mut title = title.split_whitespace().collect::<Vec<_>>().join(" ");

    for article in ["The", "A", "An"] {
        let pattern = format!(", {article}");
        if let Some(pos) = title.find(&pattern) {
            let after = &title[pos + pattern.len()..];
            if after.is_empty() || after.starts_with(" -") || after.starts_with(':') {
                title = format!("{article} {}{after}", &title[..pos]);
                break;
            }
        }
    }
    if title.is_empty() {
        title = stem.to_string();
    }
    (title, tags)
}

fn is_rom(path: &Path) -> bool {
    path.extension()
        .is_some_and(|ext| ext.eq_ignore_ascii_case("nes"))
}

fn walk(dir: &Path, depth: usize, found: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        let hidden = entry.file_name().to_string_lossy().starts_with('.');
        let Ok(kind) = entry.file_type() else {
            continue;
        };
        if kind.is_dir() || (kind.is_symlink() && path.is_dir()) {
            if !hidden && depth < MAX_SCAN_DEPTH {
                walk(&path, depth + 1, found);
            }
        } else if is_rom(&path) {
            found.push(path);
        }
    }
}

pub fn scan(folders: &[PathBuf], files: &[PathBuf]) -> Vec<Game> {
    let mut found = Vec::new();
    for folder in folders {
        walk(folder, 0, &mut found);
    }
    found.extend(files.iter().filter(|f| f.is_file()).cloned());

    let mut seen = std::collections::HashSet::new();
    let mut games: Vec<Game> = found
        .into_iter()
        .map(|p| std::path::absolute(&p).unwrap_or(p))
        .filter(|p| seen.insert(std::fs::canonicalize(p).unwrap_or_else(|_| p.clone())))
        .map(|p| Game::load(&p))
        .collect();
    games.sort_by_cached_key(|g| (g.title.to_lowercase(), g.file_name.to_lowercase()));
    games
}

pub fn is_rom_file(path: &Path) -> bool {
    is_rom(path) && path.is_file()
}

/// A path typed or pasted by the user. Quotes (from Explorer's "Copy as
/// path") are removed, and when the launcher runs on Linux, as under WSL,
/// a Windows path like `C:\Users\me\ROMs` becomes `/mnt/c/Users/me/ROMs`.
pub fn path_from_input(input: &str) -> PathBuf {
    let input = input.trim().trim_matches('"').trim();
    let bytes = input.as_bytes();
    let windows_drive = bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && (bytes[2] == b'\\' || bytes[2] == b'/');
    if cfg!(target_os = "linux") && windows_drive {
        let drive = (bytes[0] as char).to_ascii_lowercase();
        let rest = input[3..].replace('\\', "/");
        return PathBuf::from(format!("/mnt/{drive}/{rest}"));
    }
    PathBuf::from(input)
}

#[cfg(test)]
mod test {
    use super::*;

    fn header(bytes: [u8; 16], prg_banks: usize, chr_banks: usize) -> Vec<u8> {
        let mut raw = bytes.to_vec();
        raw.resize(
            16 + prg_banks * PRG_ROM_PAGE_SIZE + chr_banks * CHR_ROM_PAGE_SIZE,
            0,
        );
        raw
    }

    const INES_MMC3: [u8; 16] = [
        0x4E, 0x45, 0x53, 0x1A, 2, 1, 0x42, 0x00, 0, 0, 0, 0, 0, 0, 0, 0,
    ];

    #[test]
    fn test_path_from_input() {
        assert_eq!(path_from_input("  /home/me/roms "), PathBuf::from("/home/me/roms"));
        assert_eq!(path_from_input("\"/home/me/my roms\""), PathBuf::from("/home/me/my roms"));
        if cfg!(target_os = "linux") {
            assert_eq!(
                path_from_input("\"C:\\Users\\Me\\NES ROMs\""),
                PathBuf::from("/mnt/c/Users/Me/NES ROMs")
            );
            assert_eq!(path_from_input("D:/Games"), PathBuf::from("/mnt/d/Games"));
        }
    }

    #[test]
    fn test_clean_title() {
        let (title, tags) = clean_title("Legend of Zelda, The (USA) (Rev 1) [!]");
        assert_eq!(title, "The Legend of Zelda");
        assert_eq!(tags, ["USA", "Rev 1", "!"]);
        assert_eq!(clean_title("Super_Mario_Bros").0, "Super Mario Bros");
        assert_eq!(clean_title("Mega Man 2 (E)").0, "Mega Man 2");
        assert_eq!(
            clean_title("Adventures of Lolo, The - Part 2 (U)").0,
            "The Adventures of Lolo - Part 2"
        );
        assert_eq!(clean_title("(J)").0, "(J)", "nothing left: keep the name");
        assert_eq!(clean_title("Odd (unclosed").0, "Odd (unclosed");
    }

    #[test]
    fn test_parse_ines() {
        let rom = RomInfo::parse(&header(INES_MMC3, 2, 1)).unwrap();
        assert_eq!(rom.format, HeaderFormat::INes);
        assert_eq!(rom.mapper, 4);
        assert!(rom.battery);
        assert_eq!((rom.prg_rom, rom.chr_rom), (32768, 8192));
        assert!(rom.supported());
    }

    #[test]
    fn test_parse_rejects_truncated_and_non_roms() {
        let mut raw = header(INES_MMC3, 2, 1);
        raw.truncate(1000);
        assert!(RomInfo::parse(&raw).unwrap_err().contains("truncated"));
        assert!(RomInfo::parse(b"PK\x03\x04 not a rom file").is_err());
    }

    #[test]
    fn test_region_rules_match_the_emulator() {
        let ines = RomInfo::parse(&header(INES_MMC3, 2, 1)).unwrap();
        assert_eq!(
            auto_region(&ines, "Game (Europe).nes"),
            (Region::Pal, "file name")
        );
        assert_eq!(
            auto_region(&ines, "Game (USA).nes"),
            (Region::Ntsc, "default")
        );

        let mut nes2 = INES_MMC3;
        nes2[7] = 0x08;
        let nes2 = RomInfo::parse(&header(nes2, 2, 1)).unwrap();
        assert_eq!(nes2.format, HeaderFormat::Nes2);
        assert_eq!(
            auto_region(&nes2, "Game (Europe).nes"),
            (Region::Ntsc, "ROM header"),
            "an NES 2.0 header beats the file name"
        );

        let mut dendy = INES_MMC3;
        dendy[7] = 0x08;
        dendy[12] = 3;
        let dendy = RomInfo::parse(&header(dendy, 2, 1)).unwrap();
        assert_eq!(auto_region(&dendy, "x.nes"), (Region::Dendy, "ROM header"));

        assert_eq!(auto_console("Game (Japan).nes").0, Console::Famicom);
        assert_eq!(auto_console("Game (USA).nes").0, Console::Nes);
    }

    #[test]
    fn test_scan_finds_roms_in_subfolders_once() {
        let dir = std::env::temp_dir().join(format!("nes_launcher_scan_{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(dir.join("sub/deeper")).unwrap();
        std::fs::create_dir_all(dir.join(".hidden")).unwrap();
        let rom = header(INES_MMC3, 2, 1);
        std::fs::write(dir.join("b game (USA).nes"), &rom).unwrap();
        std::fs::write(dir.join("sub/deeper/A Game.NES"), &rom).unwrap();
        std::fs::write(dir.join(".hidden/skipped.nes"), &rom).unwrap();
        std::fs::write(dir.join("notes.txt"), "x").unwrap();

        let single = dir.join("b game (USA).nes");
        let games = scan(std::slice::from_ref(&dir), &[single]);
        let titles: Vec<_> = games.iter().map(|g| g.title.as_str()).collect();
        assert_eq!(titles, ["A Game", "b game"]);
        assert!(
            games
                .iter()
                .all(|g| g.path.is_absolute() && g.problem().is_none())
        );
        std::fs::remove_dir_all(&dir).unwrap();
    }
}