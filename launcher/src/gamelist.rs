//! Proper titles and box art for the games, from two online sources:
//!
//! - The game list: No-Intro's list of NES games (from the libretro
//!   database), which gives each game's name and the checksum (CRC32) of
//!   its ROM. A file is matched by the checksum of the ROM after its 16-byte
//!   header, so it doesn't matter which header the file has.
//! - Box art from the libretro thumbnails project, where each picture is
//!   named after the game's No-Intro name.
//!
//! Both are kept in the launcher's data folder, so they're only downloaded
//! once. Downloads run on their own thread, and so does reading the
//! pictures, so the window never waits for either.

use eframe::egui;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::mpsc::{self, Receiver, Sender};
use std::time::Duration;

const GAME_LIST_URL: &str = "https://raw.githubusercontent.com/libretro/libretro-database/master/metadat/no-intro/Nintendo%20-%20Nintendo%20Entertainment%20System.dat";
const COVERS_URL: &str = "https://raw.githubusercontent.com/libretro-thumbnails/Nintendo_-_Nintendo_Entertainment_System/master/Named_Boxarts/";

const COVER_HEIGHT: usize = 320;

const HEADER_SIZE: usize = 16;
const TRAINER_SIZE: usize = 512;

pub fn rom_checksum(file: &[u8]) -> u32 {
    let trainer = file.get(6).is_some_and(|&flags| flags & 0b100 != 0);
    let start = HEADER_SIZE + if trainer { TRAINER_SIZE } else { 0 };
    crc32fast::hash(file.get(start..).unwrap_or(&[]))
}

#[derive(Default)]
pub struct GameList {
    names: HashMap<u32, String>
}

impl GameList {
    pub fn from_dat(text: &str) -> GameList {
        let mut names = HashMap::new();
        let mut game = None;
        for line in text.lines().map(str::trim) {
            if let Some(name) = line.strip_prefix("name \"") {
                game = name.strip_suffix('"').map(str::to_string);
            } else if line.starts_with("rom (") && line.contains(".unh\"") {
                let crc = line
                    .split_once(" crc ")
                    .and_then(|(_, rest)| rest.split_whitespace().next())
                    .and_then(|crc| u32::from_str_radix(crc, 16).ok());
                if let (Some(crc), Some(game)) = (crc, &game) {
                    names.entry(crc).or_insert_with(|| game.clone());
                }
            }
        }
        GameList { names }
    }

    pub fn load() -> GameList {
        let Some(text) = list_path().and_then(|path| std::fs::read_to_string(path).ok()) else {
            return GameList::default();
        };
        let names = text
            .lines()
            .filter_map(|line| {
                let (crc, name) = line.split_once('\t')?;
                Some((u32::from_str_radix(crc, 16).ok()?, name.to_string()))
            })
            .collect();
        GameList { names }
    }

    fn save(&self) -> Result<(), String> {
        let path  = list_path().ok_or("there's no folder to keep it in")?;
        let mut names: Vec<_> = self.names.iter().collect();
        names.sort_by(|a, b| a.1.cmp(b.1));
        let text: String = names
            .iter()
            .map(|(crc, name)| format!("{crc:08X}\t{name}\n"))
            .collect();
        write_file(&path, text.as_bytes())
    }

    pub fn name(&self, checksum: u32) -> Option<&str> {
        self.names.get(&checksum).map(String::as_str)
    }

    pub fn len(&self) -> usize {
        self.names.len()
    }

    pub fn is_empty(&self) -> bool {
        self.names.is_empty()
    }
}

fn data_dir() -> Option<PathBuf> {
    eframe::storage_dir("NES Launcher")
}

fn list_path() -> Option<PathBuf> {
    Some(data_dir()?.join("game-list.txt"))
}

pub fn cover_path(name: &str) -> Option<PathBuf> {
    Some(data_dir()?.join("covers").join(format!("{}.png", file_name(name))))
}

fn file_name(name: &str) -> String {
    name.chars()
        .map(|c| if "&*/:`<>?\\|\"".contains(c) { '_' } else { c })
        .collect()
}

fn cover_url(name: &str) -> String {
    let mut url = COVERS_URL.to_string();
    for byte in format!("{}.png", file_name(name)).bytes() {
        if byte.is_ascii_alphanumeric() || b"-._~".contains(&byte) {
            url.push(byte as char);
        } else {
            url.push_str(&format!("%{byte:02X}"));
        }
    }
    url
}

fn write_file(path: &Path, data: &[u8]) -> Result<(), String> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir).map_err(|e| format!("can't make {}: {e}", dir.display()))?;
    }
    let new = path.with_extension("new");
    std::fs::write(&new, data)
        .and_then(|_| std::fs::rename(&new, path))
        .map_err(|e| format!("can't write {}: {e}", path.display()))
}

enum Job {
    GameList,
    Covers(Vec<String>)
}

pub enum News {
    GameList(Result<GameList, String>),
    Cover { name: String, found: bool },
    CoversDone { error: Option<String> },
    Picture {
        name: String,
        image: Option<egui::ColorImage>
    }
}

pub struct Online {
    downloads: Sender<Job>,
    pictures: Sender<String>,
    news: Receiver<News>
}

impl Online {
    pub fn start(ctx: egui::Context) -> Online {
        let (downloads, download_jobs) = mpsc::channel();
        let (pictures, picture_jobs) = mpsc::channel::<String>();
        let (news_sender, news) = mpsc::channel();

        let sender = news_sender.clone();
        let download_ctx = ctx.clone();
        std::thread::Builder::new()
            .name("downloads".into())
            .spawn(move || {
                let agent = agent();
                for job in download_jobs {
                    match job {
                        Job::GameList => {
                            let _ = sender.send(News::GameList(download_list(&agent)));
                            download_ctx.request_repaint();
                        }
                        Job::Covers(names) => {
                            download_covers(&agent, names, &sender, &download_ctx)
                        }
                    }
                }
            })
            .expect("couldn't start the download thread");

        std::thread::Builder::new()
            .name("covers".into())
            .spawn(move || {
                for name in picture_jobs {
                    let image = cover_path(&name)
                        .and_then(|path| std::fs::read(path).ok())   
                        .and_then(|data| read_png(&data));
                    let _ = news_sender.send(News::Picture { name, image });
                    ctx.request_repaint();
                }
            })
            .expect("couldn't start the cover thread");

        Online {
            downloads,
            pictures,
            news
        }
    }

    pub fn get_game_list(&self) {
        let _ = self.downloads.send(Job::GameList);
    }

    pub fn get_covers(&self, names: Vec<String>) {
        let _ = self.downloads.send(Job::Covers(names));
    }

    pub fn read_cover(&self, name: &str) {
        let _ = self.pictures.send(name.to_string());
    }

    pub fn poll(&self) -> Option<News> {
        self.news.try_recv().ok()
    }
}

fn agent() -> ureq::Agent {
    ureq::Agent::config_builder()
        .timeout_global(Some(Duration::from_secs(60)))
        .tls_config(
            ureq::tls::TlsConfig::builder()
                .root_certs(ureq::tls::RootCerts::PlatformVerifier)
                .build()
        )
        .build()
        .into()
}

fn download_list(agent: &ureq::Agent) -> Result<GameList, String> {
    let text = agent
        .get(GAME_LIST_URL)
        .call()
        .and_then(|mut response| {
            response
                .body_mut()
                .with_config()
                .limit(64 * 1024 * 1024)
                .read_to_string()
        })
        .map_err(|e| format!("Couldn't download the game list: {e}"))?;
    let list = GameList::from_dat(&text);
    if list.is_empty() {
        return Err("The downloaded game list has no games in it".into());
    }
    list.save()
        .map_err(|e| format!("Couldn't save the game list: {e}"))?;
    Ok(list)
}

fn download_covers(
    agent: &ureq::Agent,
    names: Vec<String>,
    news: &Sender<News>,
    ctx: &egui::Context,
) {
    let mut error = None;
    for name in names {
        let Some(path) = cover_path(&name) else {
            error = Some("There's no folder to keep covers in".to_string());
            break;
        };
        let found = if path.is_file() {
            true
        } else {
            match download_cover(agent, &name, &path) {
                Ok(found) => found,
                Err(e) => {
                    error = Some(e);
                    break;
                }
            }
        };
        let _ = news.send(News::Cover { name, found });
        ctx.request_repaint();
    }
    let _ = news.send(News::CoversDone { error });
    ctx.request_repaint();
}

fn download_cover(agent: &ureq::Agent, name: &str, path: &Path) -> Result<bool, String> {
    let response = agent.get(&cover_url(name)).call();
    let data = match response {
        Ok(mut response) => response
            .body_mut()
            .with_config()
            .limit(16 * 1024 * 1024)
            .read_to_vec(),
        Err(ureq::Error::StatusCode(404)) => return Ok(false),
        Err(e) => Err(e),
    }
    .map_err(|e| format!("Couldn't download covers: {e}"))?;
    if read_png(&data).is_none() {
        return Ok(false);
    }
    write_file(path, &data)?;
    Ok(true)
}

fn read_png(data: &[u8]) -> Option<egui::ColorImage> {
    let mut decoder = png::Decoder::new(std::io::Cursor::new(data));
    decoder.set_transformations(png::Transformations::normalize_to_color8());
    let mut reader = decoder.read_info().ok()?;
    let mut pixels = vec![0; reader.output_buffer_size()?];
    let frame = reader.next_frame(&mut pixels).ok()?;
    let (width, height) = (frame.width as usize, frame.height as usize);
    let channels = frame.color_type.samples();
    let rgba: Vec<u8> = pixels[..frame.buffer_size()]
        .chunks_exact(channels)
        .flat_map(|p| match p {
            [l] => [*l, *l, *l, 255],
            [l, a] => [*l, *l, *l, *a],
            [r, g, b] => [*r, *g, *b, 255],
            [r, g, b, a] => [*r, *g, *b, *a],
            _ => [0, 0, 0, 0],
        })
        .collect();
    if width == 0 || height == 0 || rgba.len() != width * height * 4 {
        return None;
    }
    let (rgba, width, height) = if height > COVER_HEIGHT {
        let new_width = (width * COVER_HEIGHT / height).max(1);
        (shrink(&rgba, width, height, new_width, COVER_HEIGHT), new_width, COVER_HEIGHT)
    } else {
        (rgba, width, height)
    };
    Some(egui::ColorImage::from_rgba_unmultiplied([width, height], &rgba))
}

fn shrink(rgba: &[u8], width: usize, height: usize, new_width: usize, new_height: usize) -> Vec<u8> {
    let span = |i: usize, old: usize, new: usize| {
        let start = i * old / new;
        start..((i + 1) * old / new).max(start + 1)
    };
    let mut out = Vec::with_capacity(new_width * new_height * 4);
    for y in 0..new_height {
        let rows = span(y, height, new_height);
        for x in 0..new_width {
            let columns = span(x, width, new_width);
            let mut sum = [0u32; 4];
            for row in rows.clone() {
                for column in columns.clone() {
                    let pixel = &rgba[(row * width + column) * 4..][..4];
                    for (total, &value) in sum.iter_mut().zip(pixel) {
                        *total += value as u32;
                    }
                }
            }
            let count = (rows.len() * columns.len()) as u32;
            out.extend(sum.map(|total| ((total + count / 2) / count) as u8));
        }
    }
    out
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_reads_headerless_entries_from_the_dat() {
        let dat = "clrmamepro (\n\tname \"Nintendo - Nintendo Entertainment System\"\n)\n\n\
            game (\n\tname \"Tetris (USA)\"\n\tregion \"USA\"\n\
            \trom ( name \"Tetris (USA).nes\" size 49168 crc 1394F57E md5 0 sha1 0 )\n)\n\
            game (\n\tname \"Tetris (USA)\"\n\tregion \"USA\"\n\
            \trom ( name \"Tetris (USA).unh\" size 49152 crc 6D72C53A md5 0 sha1 0 )\n)\n";
        let list = GameList::from_dat(dat);
        assert_eq!(list.len(), 1);
        assert_eq!(list.name(0x6D72C53A), Some("Tetris (USA)"));
        assert_eq!(list.name(0x1394F57E), None);
    }

    #[test]
    fn test_checksum_skips_the_header_and_trainer() {
        let rom = [1u8, 2, 3, 4];
        let mut file = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        file.extend(rom);
        assert_eq!(rom_checksum(&file), crc32fast::hash(&rom));
        let mut with_trainer = file[..16].to_vec();
        with_trainer[6] |= 0b100;
        with_trainer.extend([0xAA; TRAINER_SIZE]);
        with_trainer.extend(rom);
        assert_eq!(rom_checksum(&with_trainer), crc32fast::hash(&rom));
    }

    #[test]
    fn test_cover_names_and_urls() {
        assert_eq!(
            file_name("Mario & Yoshi: Cool/Hot? (USA)"),
            "Mario _ Yoshi_ Cool_Hot_ (USA)"
        );
        assert_eq!(
            cover_url("Super Mario Bros. (World)"),
            format!("{COVERS_URL}Super%20Mario%20Bros.%20%28World%29.png")
        );
    }

    #[test]
    fn test_shrink_averages() {
        let rgba = [0, 0, 0, 255, 200, 100, 50, 255];
        assert_eq!(shrink(&rgba, 2, 1, 1, 1), vec![100, 50, 25, 255]);
    }
}