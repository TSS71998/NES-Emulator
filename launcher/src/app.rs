//! The launcher window: a toolbar, the game list, a details panel for the
//! selected game, and a status bar; or, while a game runs, the game (and
//! its pause menu).

use crate::config::{Config, FastForward, GameRecord, LibraryView, ScreenSize, SortKey};
use crate::controls::{self, BUTTONS, PlayerControls, RESERVED_KEYS};
use crate::format;
use crate::gamelist::{self, GameList, News, Online};
use crate::library::{self, Console, Game, Region};
use crate::machine::{Machine, Report, StartGame};
use eframe::egui::{self, Align, Color32, Key, Layout, RichText, TextWrapMode};
use egui_extras::{Column, TableBuilder};
use nes_emulator::joypad::JoypadButton;
use sdl2::controller::Button;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant, SystemTime};

const ROW_HEIGHT: f32 = 24.0;

const FAVORITE: Color32 = Color32::from_rgb(240, 190, 40);

const CARD_WIDTH: f32 = 150.0;
const CARD_COVER_HEIGHT: f32 = 200.0;
const CARD_HEIGHT: f32 = 240.0;
const CARD_GAP: f32 = 14.0;

const DETAILS_COVER_HEIGHT: f32 = 200.0;

const SLOTS: usize = 4;

const NOTICE_TIME: Duration = Duration::from_secs(3);

const SLOT_PICTURE: egui::Vec2 = egui::vec2(132.0, 123.2);

const OVERSCAN: usize = 8;
const VISIBLE_WIDTH: usize = 256 - 2 * OVERSCAN;
const VISIBLE_HEIGHT: usize = 240 - 2 * OVERSCAN;

const HINT_TIME: Duration = Duration::from_secs(4);

struct Playing {
    key: String,
    title: String,
    texture: Option<egui::TextureHandle>,
    shown_frame: u64,
    hint_until: Instant,
    warning: Option<String>,
    paused: bool,
    focus_menu: bool,
    slot: usize,
    notice: Option<(String, bool, Instant)>,
    slots: Vec<Slot>
}

struct Slot {
    saved: Option<SystemTime>,
    picture: Option<egui::TextureHandle>
}

enum Cover {
    Loading,
    Ready(egui::TextureHandle),
    Missing
}

#[derive(Default)]
struct Downloads {
    list: bool,
    cover_jobs: usize,
    covers_done: usize,
    covers_total: usize,
    error: Option<String>
}

impl Downloads {
    fn busy(&self) -> bool {
        self.list || self.cover_jobs > 0
    }

    fn status(&self) -> Option<String> {
        if self.list {
            Some("Downloading the game list (3 MB)…".into())
        } else if self.cover_jobs > 0 {
            Some(format!("Downloading covers: {} of {}", self.covers_done, self.covers_total))
        } else {
            None
        }
    }
}

#[derive(Clone, Copy, PartialEq)]
struct Capture {
    player: usize,
    button: usize,
    gamepad: bool
}

const MICROPHONE: usize = 8;

#[derive(Clone, Copy)]
enum Move {
    Up,
    Down,
    Left,
    Right,
    PageUp,
    PageDown,
}

const PAD_REPEAT_DELAY: Duration = Duration::from_millis(400);
const PAD_REPEAT: Duration = Duration::from_millis(70);

enum MenuChoice {
    Resume,
    SaveState(usize),
    LoadState(usize),
    Reset,
    Fullscreen,
    Quit
}

pub struct App {
    config: Config,
    games: Vec<Game>,
    shown: Vec<usize>,
    selected: Option<String>,
    search: String,
    focus_search: bool,
    scroll_to_selected: bool,
    machine: Machine,
    playing: Option<Playing>,
    held_keys: Vec<Key>,
    keyboard_sent: ([JoypadButton; 2], bool), 
    controls_open: bool,
    controls_player: usize,
    capture: Option<Capture>,
    capture_modifiers: egui::Modifiers,
    capture_note: Option<String>,
    pad_held: Vec<Button>,
    pad_repeat: Option<(Button, Instant)>,
    pad_keys: Vec<Key>,
    fast_forward: bool,
    ctx: egui::Context,
    failed_run: Option<(String, String)>,
    message: Option<(String, bool)>,
    settings_open: bool,
    folder_input: String,
    fullscreen: bool,
    game_list: GameList, 
    online: Online,
    downloads: Downloads,
    covers: HashMap<String, Cover>,
    missing_covers: HashSet<String>,
    grid_columns: usize
}

impl App {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let config: Config = cc
            .storage
            .and_then(|s| eframe::get_value(s, eframe::APP_KEY))
            .unwrap_or_default();

        cc.egui_ctx
            .style_mut(|style| style.interaction.selectable_labels = false);
        let mut app = App {
            config,
            games: Vec::new(),
            shown: Vec::new(),
            selected: None,
            search: String::new(),
            focus_search: false,
            scroll_to_selected: false,
            machine: Machine::start(cc.egui_ctx.clone()),
            playing: None,
            held_keys: Vec::new(),
            keyboard_sent: ([JoypadButton::empty(); 2], false),
            controls_open: false,
            controls_player: 0,
            capture: None,
            capture_modifiers: egui::Modifiers::NONE,
            capture_note: None,
            pad_held: Vec::new(),
            pad_repeat: None,
            pad_keys: Vec::new(),
            fast_forward: false,
            ctx: cc.egui_ctx.clone(),
            failed_run: None,
            message: None,
            settings_open: false,
            folder_input: String::new(),
            fullscreen: false,
            game_list: GameList::load(),
            online: Online::start(cc.egui_ctx.clone()),
            downloads: Downloads::default(),
            covers: HashMap::new(),
            missing_covers: HashSet::new(),
            grid_columns: 1
        };

        app.machine.set_pad_layouts(app.config.controls.pad_layouts());
        app.send_volume();

        app.rescan();
        app
    }

    fn rescan(&mut self) {
        self.games = library::scan(&self.config.folders, &self.config.files);
        self.name_games();
        self.fetch_missing_covers();
    }

    fn name_games(&mut self) {
        for game in &mut self.games {
            game.name_from(&self.game_list);
        }
        library::sort_by_title(&mut self.games);
        self.refresh_list();
    }

    fn get_titles_and_covers(&mut self) {
        self.config.get_covers = true;
        self.missing_covers.clear();
        self.downloads.list = true;
        self.downloads.error = None;
        self.online.get_game_list();
    }

    fn fetch_missing_covers(&mut self) {
        if !self.config.get_covers {
            return;
        }
        let mut names: Vec<String>  = self
            .games
            .iter()
            .filter_map(|game| game.list_name.clone())
            .filter(|name| {
                !self.missing_covers.contains(name)
                    && !gamelist::cover_path(name).is_some_and(|path| path.is_file())
            })
            .collect();
        names.sort();
        names.dedup();
        if !names.is_empty() {
            self.downloads.cover_jobs += 1;
            self.downloads.covers_total += names.len();
            self.online.get_covers(names);
        }
    }

    fn check_online(&mut self) {
        while let Some(news) = self.online.poll() {
            match news {
                News::GameList(Ok(list)) => {
                    self.downloads.list = false;
                    self.game_list = list;
                    self.name_games();
                    self.fetch_missing_covers();
                }
                News::GameList(Err(error)) => {
                    self.downloads.list = false;
                    self.downloads.error = Some(error);
                }
                News::Cover { name, found } => {
                    self.downloads.covers_done += 1;
                    if found {
                        self.covers.remove(&name);
                    } else {
                        self.missing_covers.insert(name);
                    }
                }
                News::CoversDone { error } => {
                    self.downloads.cover_jobs = self.downloads.cover_jobs.saturating_sub(1);
                    if self.downloads.cover_jobs == 0 {
                        self.downloads.covers_done = 0;
                        self.downloads.covers_total = 0;
                    }
                    if error.is_some() {
                        self.downloads.error = error;
                    }
                }
                News::Picture { name, image } => {
                    let cover = match image {
                        Some(image) => Cover::Ready(self.ctx.load_texture(
                            format!("cover {name}"),
                            image,
                            egui::TextureOptions::LINEAR,
                        )),
                        None => Cover::Missing
                    };
                    self.covers.insert(name, cover);
                }
            }
        }
    }

    fn cover(&mut self, name: &str) -> Option<(egui::TextureId, egui::Vec2)> {
        match self.covers.get(name) {
            Some(Cover::Ready(texture)) => return Some((texture.id(), texture.size_vec2())),
            Some(_) => return None,
            None => {}
        }
        let cover = if gamelist::cover_path(name).is_some_and(|path| path.is_file()) {
            self.online.read_cover(name);
            Cover::Loading
        } else {
            Cover::Missing
        };
        self.covers.insert(name.to_string(), cover);
        None
    }

    fn refresh_list(&mut self) {
        let search = self.search.to_lowercase();
        let mut shown: Vec<usize> = (0..self.games.len())
            .filter(|&i| {
                let game = &self.games[i];
                let favorite = || self.config.games.get(&game.key()).is_some_and(|r| r.favorite);
                (search.is_empty()
                    || game.title.to_lowercase().contains(&search)
                    || game.file_name.to_lowercase().contains(&search))
                    && (!self.config.favorites_only || favorite())
            })
            .collect();
        let config = &self.config;
        let games = &self.games;
        let record = |i: usize| config.games.get(&games[i].key());
        match config.sort {
            SortKey::Title => {}
            SortKey::Region => shown.sort_by_key(|&i| region(&games[i], record(i)).0.name()),
            SortKey::Mapper => {
                shown.sort_by_key(|&i| games[i].rom.as_ref().map_or(u16::MAX, |r| r.mapper))
            }
            SortKey::Size => shown.sort_by_key(|&i| games[i].size),
            SortKey::LastPlayed => {
                shown.sort_by_key(|&i| std::cmp::Reverse(record(i).and_then(|r| r.last_played)))
            }
            SortKey::PlayTime => {
                shown.sort_by_key(|&i| std::cmp::Reverse(record(i).map_or(0, |r| r.play_secs)))
            }
        }
        if config.descending {
            shown.reverse();
        }
        self.shown = shown;

        let games = &self.games;
        let listed = |key: &String| self.shown.iter().any(|&i| &games[i].key() == key);
        if !self.selected.as_ref().is_some_and(listed) {
            self.selected = self.shown.first().map(|&i| games[i].key());
        }
    }

    fn record(&self, key: &str) -> GameRecord {
        self.config.games.get(key).cloned().unwrap_or_default()
    }

    fn toggle_favorite(&mut self, key: &str) {
        let mut record = self.record(key);
        record.favorite = !record.favorite;
        self.set_record(key, record);
        self.refresh_list();
    }

    fn send_volume(&self) {
        self.machine.set_volume(self.config.volume.gain(self.config.muted));
    }

    fn set_record(&mut self, key: &str, record: GameRecord) {
        if record == GameRecord::default() {
            self.config.games.remove(key);
        } else {
            self.config.games.insert(key.to_string(), record);
        }
    }

    fn selected_game(&self) -> Option<&Game> {
        let key = self.selected.as_ref()?;
        self.games.iter().find(|g| &g.key() == key)
    }

    fn play(&mut self, key: &str) {
        let Some(game) = self.games.iter().find(|g| g.key() == key) else {
            return;
        };
        if self.playing.is_some() {
            return;
        }
        if let Some(problem) = game.problem() {
            self.message = Some((problem, true));
            return;
        }
        let record = self.record(key);
        let (auto_region, region_source) = match &game.rom {
            Ok(rom) => game.auto_region(rom),
            Err(_) => (Region::Ntsc, "default")
        };
        let (auto_console, console_source) = game.auto_console();
        let region = record.region.or((region_source == "game list").then_some(auto_region));
        let console = record.console.or((console_source == "game list").then_some(auto_console));
        self.machine.play(StartGame {
            key: game.key(),
            path: game.path.clone(),
            region: region
                .and_then(|r| nes_emulator::region::Region::parse(r.arg())),
                console: console
                    .and_then(|c| nes_emulator::region::Console::parse(c.arg())),
                    oam_row_corruption: !record.no_oam_corruption
        });
        self.failed_run = None;
        self.playing = Some(Playing {
            key: game.key(),
            title: game.title.clone(),
            texture: None, 
            shown_frame: self.machine.screen.lock().unwrap().frame,
            hint_until: Instant::now() + HINT_TIME,
            warning: None,
            paused: false,
            focus_menu: false,
            slot: 0,
            notice: None,
            slots: Vec::new()
        });
        self.ctx.send_viewport_cmd(egui::ViewportCommand::Title(format!(
            "{} - NES Launcher",
            game.title
        )));

        self.ctx.memory_mut(|m| {
            if let Some(id) = m.focused() {
                m.surrender_focus(id);
            }
        })
    }

    fn stop_game(&mut self) {
        if self.playing.take().is_some() {
            self.machine.stop();
            self.release_keys();
            self.ctx
                .send_viewport_cmd(egui::ViewportCommand::Title("NES Launcher".into()));
            self.ctx.request_repaint();
        }
    }

    fn check_machine(&mut self) {
        while let Some(report) = self.machine.poll() {
            self.handle_report(report);
        }
    }

    fn handle_report(&mut self, report: Report) {
        match report {
            Report::Started { key, warning } => {
                if let Some(playing) = self.playing.as_mut().filter(|p| p.key == key) {
                    if let Some(warning) = &warning {
                        self.message = Some((warning.clone(), true));
                    }
                    playing.warning = warning;
                }
            }
            Report::Stopped { key, played } => self.record_session(&key, played),
            Report::Failed { key, error } => {
                if self.playing.as_ref().is_some_and(|p| p.key == key) {
                    self.stop_game();
                }
                self.message = Some((format!("Couldn't start the game: {error}"), true));
                self.failed_run = Some((key, error));
            }
            Report::StateSaved { slot, result } => self.state_finished(slot, result, "Saved to"),
            Report::StateLoaded { slot, result } => self.state_finished(slot, result, "Loaded"),
            Report::Gamepad { button, pressed } => self.gamepad(button, pressed)
        }
    }

    fn save_state(&mut self, slot: usize) {
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        playing.slot = slot;
        match state_path(Path::new(&playing.key), slot) {
            Some(path) => self.machine.save_state(slot, path),
            None => self.notice("There's no folder to keep save states in", true)
        } 
    }

    fn load_state(&mut self, slot: usize) {
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        playing.slot = slot;
        match state_path(Path::new(&playing.key), slot) {
            Some(path) if path.is_file() => self.machine.load_state(slot, path), 
            _ => self.notice(&format!("Slot {} is empty", slot + 1), true)
        }
    }

    fn state_finished(&mut self, slot: usize, result: Result<(), String>, done: &str) {
        match result {
            Ok(()) => self.notice(&format!("{done} slot {}", slot + 1), false),
            Err(e) => self.notice(&format!("Slot {}: {e}", slot + 1), true),
        }
    }

    fn notice(&mut self, text: &str, error: bool) {
        if let Some(playing) = self.playing.as_mut() {
            playing.notice = Some((text.to_string(), error, Instant::now() + NOTICE_TIME));
            self.ctx.request_repaint();
        }
    }

    fn read_slots(&mut self) {
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        playing.slots = (0..SLOTS)
            .map(|slot| {
                let path = state_path(Path::new(&playing.key), slot);
                let saved = path
                    .as_ref()
                    .and_then(|p| std::fs::metadata(p).ok())
                    .and_then(|m| m.modified().ok());
                let picture = path
                    .and_then(|p| std::fs::read(p).ok())
                    .and_then(|state| {
                        let picture = nes_emulator::nes::Nes::state_picture(&state)?;
                        Some(self.ctx.load_texture(
                            format!("slot{slot}"),
                            visible_picture(picture),
                            egui::TextureOptions::LINEAR,
                        ))
                    });
                Slot { saved, picture }
            })
            .collect();
    }

    fn record_session(&mut self, key: &str, played: Duration) {
        let secs = played.as_secs();
        let mut record = self.record(key);
        record.play_secs += secs;
        record.last_played = Some(format::unix_now().saturating_sub(secs));
        self.set_record(key, record);
        self.refresh_list();
        let title = self
            .games
            .iter()
            .find(|g| g.key() == key)
            .map_or_else(|| key.to_string(), |g| g.title.clone());
        self.message = Some((
            format!("Played {title} for {}.", format::clock(secs)),
            false
        ));
    }

    fn finish_game(&mut self) {
        if self.playing.is_none() {
            return;
        }
        self.stop_game();
        let deadline = Instant::now() + Duration::from_secs(2);
        while let Some(report) = self
            .machine
            .wait(deadline.saturating_duration_since(Instant::now()))
        {
            let done = matches!(report, Report::Stopped { .. } | Report::Failed { .. });
            self.handle_report(report);
            if done {
                break;
            }
        }
    }

    fn game_keys(&mut self, ctx: &egui::Context) {
        let (events, modifiers, focused) =
            ctx.input(|i| (i.events.clone(), i.modifiers, i.focused));
        for event in events {
            let egui::Event::Key {
                key,
                physical_key,
                pressed,
                repeat: false,
                modifiers
            } = event
            else {
                continue;
            };
            if pressed && key == Key::Escape {
                self.pause_game(true);
                return;
            }
            if pressed && key == Key::R && modifiers.command {
                self.machine.reset();
                continue;
            }
            if key == Key::Tab {
                self.set_fast_forward(pressed);
                continue;
            }
            if pressed && (key == Key::F5 || key == Key::F8) {
                let slot = self.playing.as_ref().map_or(0, |p| p.slot);
                if key == Key::F5 {
                    self.save_state(slot);
                } else {
                    self.load_state(slot);
                }
                continue;
            }
            if pressed && key == Key::F9 {
                self.config.muted = !self.config.muted;
                self.send_volume();
                let text = if self.config.muted { "Sound off (F9)" } else { "Sound on" };
                self.notice(text, false);
                continue;
            }
            
            let key = physical_key.unwrap_or(key);
            self.held_keys.retain(|&k| k != key);
            if pressed {
                self.held_keys.push(key);
            }
        }

        self.send_keyboard(modifiers);
        if !focused {
            self.release_keys();
            if self.config.pause_on_focus_loss {
                self.pause_game(true);
            }
        }
    }

    fn pause_game(&mut self, paused: bool) {
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        if playing.paused == paused {
            return;
        }
        playing.paused = paused;
        playing.focus_menu = paused;
        self.machine.pause(paused);
        self.release_keys();
        if paused {
            self.read_slots();
        }
        self.ctx.request_repaint();
    }

    fn menu_keys(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::Escape)) {
            self.pause_game(false);
        } else if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::R)) {
            self.machine.reset();
            self.pause_game(false);
        }
    }

    fn send_keyboard(&mut self, modifiers: egui::Modifiers) {
        let held = controls::held_names(&self.held_keys, modifiers);
        let state = self.config.controls.keyboard_state(&held);
        if state != self.keyboard_sent {
            self.keyboard_sent = state;
            self.machine.set_keyboard(state.0, state.1);
        }
    }

    fn release_keys(&mut self) {
        self.held_keys.clear();
        self.send_keyboard(egui::Modifiers::NONE);
        self.set_fast_forward(false);
    }

    fn set_fast_forward(&mut self, on: bool) {
        if self.fast_forward != on {
            self.fast_forward = on;
            let frames = if on { self.config.fast_forward.frames() } else { 1 };
            self.machine.set_speed(frames);
            self.ctx.request_repaint();
        }
    }

    fn game_screen(&mut self, ctx: &egui::Context) {
        let (screen_size, tv_aspect) = (self.config.screen_size, self.config.tv_aspect);
        let fast_forward = self.fast_forward.then_some(self.config.fast_forward);
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        {
            let screen = self.machine.screen.lock().unwrap();
            if screen.frame != playing.shown_frame {
                playing.shown_frame = screen.frame;
                let image = visible_picture(&screen.picture);
                match &mut playing.texture {
                    Some(texture) => texture.set(image, egui::TextureOptions::NEAREST),
                    None => {
                        playing.texture = 
                            Some(ctx.load_texture("game", image, egui::TextureOptions::NEAREST))
                    }
                }
            }
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(Color32::BLACK))
            .show(ctx, |ui| {
                let area = ui.max_rect();
                if let Some(texture) = &playing.texture {
                    let size = game_size(area.size(), screen_size, tv_aspect);
                    let rect = egui::Rect::from_center_size(area.center(), size);
                    let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                    ui.painter().image(texture.id(), rect, uv, Color32::WHITE);
                }
            });
        if playing.paused {
            self.pause_menu(ctx);
            return;
        }

        ctx.set_cursor_icon(egui::CursorIcon::None);
        let now = Instant::now();
        let mut lines = Vec::new();
        if now < playing.hint_until {
            if let Some(warning) = &playing.warning {
                lines.push(RichText::new(warning));
            }
            lines.push(RichText::new(format!(
                                "{}   ·   Esc or Back+Start: menu   ·   Tab: fast forward   ·   F5/F8: save/load state   ·   F11: fullscreen",
                playing.title
            )));
            ctx.request_repaint_after(playing.hint_until - now);
        }
        if let Some((text, error, until)) = &playing.notice {
            if now < *until {
                let text = RichText::new(text).strong();
                lines.push(if *error {
                    text.color(ctx.style().visuals.error_fg_color)
                } else {
                    text
                });
                ctx.request_repaint_after(*until - now);
            }
        }
        if let Some(speed) = fast_forward {
            let text = match speed {
                FastForward::Unlimited => "▶▶".to_string(),
                speed => format!("▶▶ {}", speed.label())
            };
            egui::Area::new(egui::Id::new("fast_forward"))
                .anchor(egui::Align2::RIGHT_TOP, egui::vec2(-12.0, 12.0))
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        ui.label(RichText::new(text).strong().size(18.0));
                    });
                });
        }
        if !lines.is_empty() {
            egui::Area::new(egui::Id::new("game_hint"))
                .anchor(egui::Align2::CENTER_TOP, egui::vec2(0.0, 12.0))
                .show(ctx, |ui| {
                    egui::Frame::popup(ui.style()).show(ui, |ui| {
                        for line in lines {
                            ui.label(line);
                        }
                    });
                });
        }
    }

    fn set_fullscreen(&mut self, on: bool) {
        self.fullscreen = on;
        self.ctx
            .send_viewport_cmd(egui::ViewportCommand::Decorations(!on));
        self.ctx
            .send_viewport_cmd(egui::ViewportCommand::Fullscreen(on));
    }

        fn pause_menu(&mut self, ctx: &egui::Context) {
        let Some(playing) = self.playing.as_mut() else {
            return;
        };
        let title = playing.title.clone();
        let focus = std::mem::take(&mut playing.focus_menu);
        let fullscreen = self.fullscreen;
        let current_slot = playing.slot;
        let slots: Vec<(Option<SystemTime>, Option<egui::TextureId>)> = playing
            .slots
            .iter()
            .map(|s| (s.saved, s.picture.as_ref().map(|p| p.id())))
            .collect();

        let dim = egui::LayerId::new(egui::Order::Middle, egui::Id::new("pause_dim"));
        ctx.layer_painter(dim)
            .rect_filled(ctx.screen_rect(), 0.0, Color32::from_black_alpha(170));

        let mut choice = None;
        egui::Area::new(egui::Id::new("pause_menu"))
            .order(egui::Order::Foreground)
            .anchor(egui::Align2::CENTER_CENTER, egui::Vec2::ZERO)
            .show(ctx, |ui| {
                egui::Frame::window(ui.style()).inner_margin(16).show(ui, |ui| {
                    ui.horizontal_top(|ui| {
                        ui.vertical(|ui| {
                            ui.set_width(280.0);
                            ui.vertical_centered(|ui| {
                                ui.heading(&title);
                                ui.weak("Paused");
                            });
                            ui.add_space(10.0);
                            let button = |ui: &mut egui::Ui, text: &str, keys: &str| {
                                ui.add(
                                    egui::Button::new(text)
                                        .shortcut_text(keys)
                                        .min_size(egui::vec2(ui.available_width(), 30.0)),
                                )
                            };
                            let resume = button(ui, "▶  Resume", "Esc");
                            if focus {
                                resume.request_focus();
                            }
                            if resume.clicked() {
                                choice = Some(MenuChoice::Resume);
                            }
                            if button(ui, "Reset", "Ctrl+R").clicked() {
                                choice = Some(MenuChoice::Reset);
                            }
                            let fullscreen_text = if fullscreen { "Leave fullscreen" } else { "Fullscreen" };
                            if button(ui, fullscreen_text, "F11").clicked() {
                                choice = Some(MenuChoice::Fullscreen);
                            }
                            ui.add_space(6.0);
                            ui.separator();
                            self.video_settings(ui);
                            ui.separator();
                            ui.add_space(6.0);
                            if button(ui, "Quit to library", "").clicked() {
                                choice = Some(MenuChoice::Quit);
                            }
                        });
                        ui.add_space(8.0);
                        ui.separator();
                        ui.add_space(8.0);
                        ui.vertical(|ui| {
                            if let Some(pick) = state_slots(ui, &slots, current_slot) {
                                choice = Some(pick);
                            }
                        });
                    });
                });
            });

        match choice {
            Some(MenuChoice::Resume) => self.pause_game(false),
            Some(MenuChoice::SaveState(slot)) => {
                self.save_state(slot);
                self.pause_game(false);
            }
            Some(MenuChoice::LoadState(slot)) => {
                self.load_state(slot);
                self.pause_game(false);
            }
            Some(MenuChoice::Reset) => {
                self.machine.reset();
                self.pause_game(false);
            }
            Some(MenuChoice::Fullscreen) => self.set_fullscreen(!self.fullscreen),
            Some(MenuChoice::Quit) => self.stop_game(),
            None => {}
        }
    }

    fn video_settings(&mut self, ui: &mut egui::Ui) {
        ui.strong("Screen size");
        ui.radio_value(
            &mut self.config.screen_size,
            ScreenSize::WholePixels,
            "Whole pixels (sharpest)",
        );
        ui.radio_value(
            &mut self.config.screen_size,
            ScreenSize::FillWindow,
            "Fill the window",
        );
        ui.checkbox(&mut self.config.tv_aspect, "TV pixel shape (8:7)")
            .on_hover_text(
                "A TV drew the NES's pixels a little wider than tall. \
                 Off: square pixels, as in the standalone emulator.",
            );
        ui.checkbox(
            &mut self.config.pause_on_focus_loss,
            "Pause when the window loses focus",
        );

        ui.horizontal(|ui| {
            ui.label("Fast forward (hold Tab)");
            egui::ComboBox::from_id_salt("fast_forward")
                .selected_text(self.config.fast_forward.label())
                .show_ui(ui, |ui| {
                    for speed in FastForward::ALL {
                        ui.selectable_value(&mut self.config.fast_forward, speed, speed.label());
                    }
                });
        });

        ui.add_space(6.0);
        ui.strong("Sound");
        let before = (self.config.volume, self.config.muted);
        ui.horizontal(|ui| {
            ui.add(egui::Slider::new(&mut self.config.volume.0, 0..=100).suffix("%"));
            ui.checkbox(&mut self.config.muted, "Mute (F9)");
        });
        if (self.config.volume, self.config.muted) != before {
            self.send_volume();
        }
    }

    fn choose_folder(&mut self) {
        let started = Instant::now();
        match rfd::FileDialog::new().pick_folder() {
            Some(folder) => self.add_folder(folder),
            None => self.check_dialog_opened(started),
        }
    }

    fn check_dialog_opened(&mut self, started: Instant) {
        if cfg!(target_os = "linux") && started.elapsed() < Duration::from_millis(500) {
            self.message = Some((
                "No file dialog could open. Install zenity (sudo apt install zenity), \
                 or type the folder's path in Settings."
                    .into(),
                true,
            ));
            self.settings_open = true;
        }
    }

    fn folder_path_input(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let path = library::path_from_input(&self.folder_input);
            let is_folder = !self.folder_input.trim().is_empty() && path.is_dir();
            let add_width = 60.0;
            let field = ui.add(
                egui::TextEdit::singleline(&mut self.folder_input)
                    .hint_text(if cfg!(target_os = "linux") {
                        "/mnt/c/Users/you/ROMs  or  C:\\Users\\you\\ROMs"
                    } else {
                        "C:\\Users\\you\\ROMs"
                    })
                    .desired_width(ui.available_width() - add_width),
            );
            let entered = field.lost_focus() && ui.input(|i| i.key_pressed(Key::Enter));
            let add = ui.add_enabled(is_folder, egui::Button::new("Add"));
            if add.clicked() || (entered && is_folder) {
                self.folder_input.clear();
                self.add_folder(path);
            } else if entered {
                self.message = Some((format!("{} isn't a folder.", path.display()), true));
            }
        });
    }

    fn add_folder(&mut self, folder: PathBuf) {
        if !self.config.folders.contains(&folder) {
            let before = self.games.len();
            self.message = Some((format!("Added {}.", folder.display()), false));
            self.config.folders.push(folder);
            self.rescan();
            if let Some((text, _)) = &mut self.message {
                let found = self.games.len().saturating_sub(before);
                text.push_str(&format!(" {found} new games."));
            }
        }
    }

    fn add_files(&mut self, files: Vec<PathBuf>) {
        let Some(last) = files.last().cloned() else {
            return;
        };
        for file in files {
            if !self.config.files.contains(&file) {
                self.config.files.push(file);
            }
        }
        self.rescan();
        let last = std::path::absolute(&last).unwrap_or(last);
        self.select(Some(last.to_string_lossy().into_owned()));
    }

    fn select(&mut self, key: Option<String>) {
        self.selected = key;
        self.scroll_to_selected = true;
    }
    
    fn handle_dropped_files(&mut self, ctx: &egui::Context) {
        let dropped: Vec<PathBuf> = ctx.input(|i| {
            i.raw
                .dropped_files
                .iter()
                .filter_map(|f| f.path.clone())
                .collect()
        });
        let mut roms = Vec::new();
        for path in dropped {
            if path.is_dir() {
                self.add_folder(path);
            } else if library::is_rom_file(&path) {
                roms.push(path);
            } else {
                self.message = Some((format!("{} isn't a .nes file.", path.display()), true));
            }
        }
        self.add_files(roms);
    }

    fn handle_keys(&mut self, ctx: &egui::Context) {
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, Key::F)) {
            self.focus_search = true;
        }
        let typing = ctx.memory(|m| m.focused().is_some());
        if typing || self.shown.is_empty() {
            return;
        }
        
        let moves = [
            (Key::ArrowDown, Move::Down),
            (Key::ArrowUp, Move::Up),
            (Key::ArrowLeft, Move::Left),
            (Key::ArrowRight, Move::Right),
            (Key::PageDown, Move::PageDown),
            (Key::PageUp, Move::PageUp),
        ];
        for (key, movement) in moves {
            if ctx.input(|i| i.key_pressed(key)) {
                self.move_selection(movement);
            }
        }
        if ctx.input(|i| i.key_pressed(Key::Enter)) {
            self.play_selected();
        }
    }

    fn play_selected(&mut self) {
        if let Some(key) = self.selected.clone() {
            self.play(&key);
        }
    }

    fn move_selection(&mut self, movement: Move) {
        if self.shown.is_empty() {
            return;
        }
        let covers = self.config.view == LibraryView::Covers;
        if !covers && matches!(movement, Move::Left | Move::Right) {
            return;
        }
        let row = if covers { self.grid_columns.max(1) } else { 1 };
        let page = if covers { row * 3 } else { 10 };
        let position = self
            .selected
            .as_ref()
            .and_then(|key| self.shown.iter().position(|&i| &self.games[i].key() == key));
        let last = self.shown.len() - 1;
        let new_position = match (position, movement) {
            (None, _) => 0,
            (Some(p), Move::Down) => (p + row).min(last),
            (Some(p), Move::Up) => p.saturating_sub(row),
            (Some(p), Move::Right) => (p + 1).min(last),
            (Some(p), Move::Left) => p.saturating_sub(1),
            (Some(p), Move::PageDown) => (p + page).min(last),
            (Some(p), Move::PageUp) => p.saturating_sub(page),
        };
        self.select(Some(self.games[self.shown[new_position]].key()));
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("📁 Add folder…").clicked() {
                self.choose_folder();
            }
            if ui.button("➕ Add games…").clicked() {
                let started = Instant::now();
                match rfd::FileDialog::new()
                    .add_filter("NES ROMs", &["nes", "NES"])
                    .pick_files()
                {
                    Some(files) => self.add_files(files),
                    None => self.check_dialog_opened(started),
                }
            }
            if ui
                .button("⟳ Rescan")
                .on_hover_text("Look for new or removed games")
                .clicked()
            {
                self.rescan();
            }
            ui.separator();
            let search = ui.add(
                egui::TextEdit::singleline(&mut self.search)
                    .hint_text("🔍 Search  (Ctrl+F)")
                    .desired_width(240.0)
            );
            if self.focus_search {
                search.request_focus();
                self.focus_search = false;
            }
            if search.changed() {
                self.refresh_list();
            }
            if search.lost_focus() && ui.input(|i| i.key_pressed(Key::Escape)) {
                self.search.clear();
                self.refresh_list();
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                if ui.button("⚙ Settings").clicked() {
                    self.settings_open = !self.settings_open;
                }
                if ui.button("🎮 Controls").clicked() {
                    self.controls_open = !self.controls_open;
                    self.capture = None;
                }
                ui.separator();
                let view = self.config.view;
                ui.selectable_value(&mut self.config.view, LibraryView::Covers, "🖼 Covers");
                ui.selectable_value(&mut self.config.view, LibraryView::List, "☰ List");
                if self.config.view != view {
                    self.scroll_to_selected = true;
                }
                ui.separator();
                let favorites = ui
                    .selectable_label(self.config.favorites_only, "★ Favorites")
                    .on_hover_text("Show only your favorite games");
                if favorites.clicked() {
                    self.config.favorites_only = !self.config.favorites_only;
                    self.refresh_list();
                }
            })
        });
    }

    fn status_bar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            let count = if self.shown.len() == self.games.len() {
                format!("{} games", self.games.len())
            } else {
                format!("{} of {} games", self.shown.len(), self.games.len())
            };
            ui.label(count);
            if let Some(status) = self.downloads.status() {
                ui.separator();
                ui.spinner();
                ui.weak(status);
            }
            if let Some((text, error)) = &self.message {
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    let text = RichText::new(text);
                    ui.label(if *error {
                        text.color(ui.visuals().error_fg_color)
                    } else {
                        text.weak()
                    });
                });
            }
        });
    }

    fn game_list(&mut self, ui: &mut egui::Ui) {
        let now = format::unix_now();
        let selected_row = self
            .selected
            .as_ref()
            .and_then(|key| self.shown.iter().position(|&i| &self.games[i].key() == key));
        let mut clicked = None;
        let mut activated = None;
        let mut sort_by = None;
        let mut starred = None;

        let mut table = TableBuilder::new(ui)
            .striped(true)
            .resizable(true)
            .sense(egui::Sense::click())
            .cell_layout(Layout::left_to_right(Align::Center))
            .column(Column::remainder().at_least(220.0).clip(true))
            .column(Column::initial(70.0).at_least(50.0))
            .column(Column::initial(100.0).at_least(60.0))
            .column(Column::initial(70.0).at_least(50.0))
            .column(Column::initial(100.0).at_least(70.0))
            .column(Column::initial(90.0).at_least(60.0));
        if self.scroll_to_selected {
            if let Some(row) = selected_row {
                table = table.scroll_to_row(row, None);
            }
            self.scroll_to_selected = false;
        }

        let config = &self.config;
        let games = &self.games;
        let shown = &self.shown;
        table
            .header(ROW_HEIGHT, |mut header| {
                for (key, label) in [
                    (SortKey::Title, "Title"),
                    (SortKey::Region, "Region"),
                    (SortKey::Mapper, "Mapper"),
                    (SortKey::Size, "Size"),
                    (SortKey::LastPlayed, "Last played"),
                    (SortKey::PlayTime, "Time played"),
                ] {
                    header.col(|ui| {
                        let arrow = match (config.sort == key, config.descending) {
                            (false, _) => "",
                            (true, false) => " ⏶",
                            (true, true) => " ⏷",
                        };
                        let text = RichText::new(format!("{label}{arrow}")).strong();
                        if ui.add(egui::Button::new(text).frame(false)).clicked() {
                            sort_by = Some(key);
                        }
                    });
                }
            })
            .body(|body| {
                body.rows(ROW_HEIGHT, shown.len(), |mut row| {
                    let game = &games[shown[row.index()]];
                    let record = config.games.get(&game.key());
                    row.set_selected(selected_row == Some(row.index()));
                    let problem = game.problem();
                    row.col(|ui| {
                        ui.style_mut().wrap_mode = Some(TextWrapMode::Truncate);
                        let favorite = record.is_some_and(|r| r.favorite);
                        let star = if favorite {
                            RichText::new("★").color(FAVORITE)
                        } else {
                            RichText::new("☆").weak()
                        };
                        let star = ui.add(egui::Button::new(star).frame(false)).on_hover_text(
                            if favorite { "Take out of favorites" } else { "Add to favorites" },
                        );
                        if star.clicked() {
                            starred = Some(game.key());
                        }
                        if problem.is_some() {
                            ui.label(RichText::new("⚠").color(ui.visuals().warn_fg_color));
                        }
                        let title = RichText::new(&game.title);
                        ui.label(if problem.is_some() {
                            title.weak()
                        } else {
                            title
                        });
                        if !game.tags.is_empty() {
                            ui.label(RichText::new(game.tags.join(" · ")).weak().small());
                        }
                    });
                    row.col(|ui| {
                        let (region, overridden) = region(game, record);
                        let text = RichText::new(region.name());
                        ui.label(if overridden { text.strong() } else { text });
                    });
                    row.col(|ui| {
                        if let Ok(rom) = &game.rom {
                            ui.label(match library::mapper_name(rom.mapper) {
                                Some(name) => format!("{} {}", rom.mapper, name),
                                None => rom.mapper.to_string(),
                            });
                        } else {
                            ui.label("—");
                        }
                    });
                    row.col(|ui| {
                        ui.label(format::size(game.size));
                    });
                    row.col(|ui| {
                        ui.label(format::ago(now, record.and_then(|r| r.last_played)));
                    });
                    row.col(|ui| {
                        ui.label(format::play_time(record.map_or(0, |r| r.play_secs)));
                    });
                    let response = row.response();
                    if response.clicked() {
                        clicked = Some(game.key());
                    }
                    if response.double_clicked() {
                        activated = Some(game.key());
                    }
                    if let Some(problem) = problem {
                        response.on_hover_text(problem);
                    }
                });
            });

        if let Some(key) = starred {
            self.toggle_favorite(&key);
        }
        if let Some(key) = sort_by {
            if self.config.sort == key {
                self.config.descending = !self.config.descending;
            } else {
                self.config.sort = key;
                self.config.descending = false;
            }
            self.refresh_list();
        }
        if let Some(key) = clicked {
            self.selected = Some(key);
        }
        if let Some(key) = activated {
            self.play(&key);
        }
    }

    fn covers_prompt(&mut self, ui: &mut egui::Ui) {
        egui::Frame::group(ui.style()).show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.horizontal(|ui| {
                ui.label("Box art and proper titles come from online game lists.");
                if let Some(status) = self.downloads.status() {
                    ui.spinner();
                    ui.label(status);
                } else if ui.button("⬇ Get titles and covers").clicked() {
                    self.get_titles_and_covers();
                }
            });
            if let Some(error) = &self.downloads.error {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        });
        ui.add_space(4.0);
    }

    fn game_covers(&mut self, ui: &mut egui::Ui) {
        let columns = ((ui.available_width() - CARD_GAP) / (CARD_WIDTH + CARD_GAP)).max(1.0) as usize;
        self.grid_columns = columns;
        let rows = self.shown.len().div_ceil(columns);
        let row_height = CARD_HEIGHT + CARD_GAP;
        let selected = self.selected.clone();
        let selected_index = selected
            .as_ref()
            .and_then(|key| self.shown.iter().position(|&i| &self.games[i].key() == key));
        let scroll_to = selected_index.filter(|_| std::mem::take(&mut self.scroll_to_selected));
        let mut clicked = None;
        let mut activated = None;

        egui::ScrollArea::vertical()
            .auto_shrink(false)
            .show_viewport(ui, |ui, viewport| {
                let origin = ui.max_rect().min + egui::vec2(CARD_GAP, CARD_GAP);
                ui.set_height(rows as f32 * row_height + CARD_GAP);
                let card_rect = |index: usize| {
                    let (row, column) = (index / columns, index % columns);
                    let offset = egui::vec2(
                        column as f32 * (CARD_WIDTH + CARD_GAP),
                        row as f32 * row_height,
                    );
                    egui::Rect::from_min_size(origin + offset, egui::vec2(CARD_WIDTH, CARD_HEIGHT))
                };
                let first_row = (viewport.min.y / row_height).max(0.0) as usize;
                let last_row = ((viewport.max.y / row_height) as usize + 1).min(rows);
                let visible = (first_row * columns)..(last_row * columns).min(self.shown.len());

                for index in visible {
                    let game = &self.games[self.shown[index]];
                    let key = game.key();
                    let title = game.title.clone();
                    let problem = game.problem();
                    let hover = match &problem {
                        Some(problem) => format!("{title}\n{problem}"),
                        None if game.tags.is_empty() => title.clone(),
                        None => format!("{title} ({})", game.tags.join(", ")),
                    };
                    let cover = game.list_name.clone().and_then(|name| self.cover(&name));
                    let favorite = self.config.games.get(&key).is_some_and(|r| r.favorite);

                    let rect = card_rect(index);
                    let response = ui.interact(rect, ui.id().with(&key), egui::Sense::click());
                    let visuals = ui.visuals();
                    let painter = ui.painter();
                    let is_selected = selected.as_ref() == Some(&key);
                    if is_selected {
                        painter.rect_filled(rect.expand(5.0), 6.0, visuals.selection.bg_fill);
                    } else if response.hovered() {
                        painter.rect_filled(rect.expand(5.0), 6.0, visuals.widgets.hovered.weak_bg_fill);
                    }

                    let cover_area = egui::Rect::from_min_size(rect.min, egui::vec2(CARD_WIDTH, CARD_COVER_HEIGHT));
                    match cover {
                        Some((id, size)) => {
                            let size = size * (cover_area.width() / size.x).min(cover_area.height() / size.y);
                            let image = egui::Rect::from_min_size(
                                egui::pos2(cover_area.center().x - size.x / 2.0, cover_area.bottom() - size.y),
                                size,
                            );
                            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                            painter.image(id, image, uv, Color32::WHITE);
                        }
                        None => {
                            painter.rect_filled(cover_area, 4.0, visuals.extreme_bg_color);
                            painter.text(
                                cover_area.center(),
                                egui::Align2::CENTER_CENTER,
                                "NES",
                                egui::FontId::proportional(28.0),
                                visuals.widgets.noninteractive.bg_stroke.color,
                            );
                        }
                    }
                    if favorite {
                        let corner = cover_area.right_top() + egui::vec2(-14.0, 14.0);
                        painter.circle_filled(corner, 11.0, Color32::from_black_alpha(160));
                        painter.text(
                            corner,
                            egui::Align2::CENTER_CENTER,
                            "★",
                            egui::FontId::proportional(16.0),
                            FAVORITE,
                        );
                    }

                    let color = if problem.is_some() {
                        visuals.weak_text_color()
                    } else if is_selected {
                        visuals.strong_text_color()
                    } else {
                        visuals.text_color()
                    };
                    let text = if problem.is_some() { format!("⚠ {title}") } else { title };
                    let mut job = egui::text::LayoutJob::single_section(
                        text,
                        egui::TextFormat::simple(egui::FontId::proportional(13.0), color),
                    );
                    job.wrap = egui::text::TextWrapping {
                        max_width: CARD_WIDTH,
                        max_rows: 2,
                        break_anywhere: false,
                        overflow_character: Some('…'),
                    };
                    job.halign = Align::Center;
                    let galley = ui.fonts(|fonts| fonts.layout_job(job));
                    let text_pos = egui::pos2(rect.center().x, cover_area.bottom() + 6.0);
                    ui.painter().galley(text_pos, galley, color);

                    if response.clicked() {
                        clicked = Some(key.clone());
                    }
                    if response.double_clicked() {
                        activated = Some(key);
                    }
                    response.on_hover_text(hover);
                }
                if let Some(index) = scroll_to {
                    ui.scroll_to_rect(card_rect(index).expand(CARD_GAP / 2.0), None);
                }
            });

        if let Some(key) = clicked {
            self.selected = Some(key);
        }
        if let Some(key) = activated {
            self.play(&key);
        }
    }

    fn empty_library(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 3.0);
            if self.games.is_empty() {
                ui.heading("No games yet");
                ui.add_space(6.0);
                ui.label(
                    "Add a folder with your .nes files, or drop files or folders onto this window."
                );
                ui.add_space(12.0);
                if ui.button("📁 Add folder…").clicked() {
                    self.choose_folder();
                }
                ui.add_space(12.0);
                ui.weak("or type or paste its path:");
                ui.allocate_ui(egui::vec2(460.0, 24.0), |ui| self.folder_path_input(ui));
            } else if self.config.favorites_only && self.search.is_empty() {
                ui.heading("No favorites yet");
                ui.label("Click the ☆ next to a game (or press X on a gamepad) to add it.");
                ui.add_space(8.0);
                if ui.button("Show all games").clicked() {
                    self.config.favorites_only = false;
                    self.refresh_list();
                }
            } else {
                ui.heading("No matches");
                ui.label(format!(
                    "No game title or file name contains \"{}\".",
                    self.search
                ));
            }
        });
    }

    fn details(&mut self, ui: &mut egui::Ui) {
        let cover = self
            .selected_game()
            .and_then(|game| game.list_name.clone())
            .and_then(|name| self.cover(&name));
        let Some(game) = self.selected_game() else {
            ui.weak("Select a game to see its details.");
            return;
        };
        let key = game.key();
        let mut record = self.record(&key);

        if let Some((id, size)) = cover {
            let size = size * (ui.available_width() / size.x).min(DETAILS_COVER_HEIGHT / size.y);
            ui.add_space(6.0);
            ui.vertical_centered(|ui| ui.add(egui::Image::new((id, size))));
        }
        ui.add_space(4.0);
        ui.heading(&game.title);
        ui.label(RichText::new(&game.file_name).weak().small());
        let star = if record.favorite { "★ Favorite" } else { "☆ Add to favorites" };
        if ui.add(egui::Button::new(star).selected(record.favorite)).clicked() {
            record.favorite = !record.favorite;
        }
        if !game.tags.is_empty() {
            ui.horizontal_wrapped(|ui| {
                for tag in &game.tags {
                    egui::Frame::new()
                        .fill(ui.visuals().faint_bg_color)
                        .stroke(ui.visuals().widgets.noninteractive.bg_stroke)
                        .corner_radius(4.0)
                        .inner_margin(egui::Margin::symmetric(6, 1))
                        .show(ui, |ui| ui.small(tag));
                }
            });
        }
        ui.add_space(6.0);
        ui.separator();

        let problem = game.problem();
        match &game.rom {
            Err(_) => {}
            Ok(rom) => {
                egui::Grid::new("rom_info")
                    .num_columns(2)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        let (auto_region, region_source) = game.auto_region(rom);
                        let (auto_console, console_source) = game.auto_console();
                        ui.weak("Region");
                        ui.label(match record.region {
                            Some(region) => format!("{} (set below)", region.name()),
                            None => format!("{} (from {region_source})", auto_region.name()),
                        });
                        ui.end_row();
                        ui.weak("Console");
                        ui.label(match record.console {
                            Some(console) => format!("{} (set below)", console.name()),
                            None => format!("{} (from {console_source})", auto_console.name()),
                        });
                        ui.end_row();
                        ui.weak("Mapper");
                        let name = library::mapper_name(rom.mapper).unwrap_or("");
                        if rom.supported() {
                            ui.label(format!("{} {name}", rom.mapper));
                        } else {
                            ui.label(
                                RichText::new(format!("{} {name} (not supported)", rom.mapper))
                                    .color(ui.visuals().warn_fg_color),
                            );
                        }
                        ui.end_row();
                        ui.weak("ROM");
                        let chr = if rom.chr_rom == 0 {
                            "CHR-RAM".to_string()
                        } else {
                            format!("CHR {}", format::size(rom.chr_rom as u64))
                        };
                        ui.label(format!("PRG {} · {chr}", format::size(rom.prg_rom as u64)));
                        ui.end_row();
                        ui.weak("Saves");
                        ui.label(if rom.battery {
                            "Battery (.sav next to the ROM)"
                        } else {
                            "None"
                        });
                        ui.end_row();
                        ui.weak("Header");
                        ui.label(rom.format.name());
                        ui.end_row();
                    });
            }
        }
        if let Some(problem) = &problem {
            ui.add_space(4.0);
            ui.colored_label(ui.visuals().warn_fg_color, problem);
        }
        let (auto_region, auto_console) = match &game.rom {
            Ok(rom) => (game.auto_region(rom).0, game.auto_console().0),
            Err(_) => (Region::Ntsc, Console::Nes),
        };

        ui.add_space(6.0);
        ui.separator();
        ui.strong("Settings for this game");
        ui.add_space(2.0);
        egui::Grid::new("game_settings")
            .num_columns(2)
            .spacing([12.0, 6.0])
            .show(ui, |ui| {
                ui.label("Region");
                egui::ComboBox::from_id_salt("region")
                    .selected_text(match record.region {
                        Some(region) => region.name().to_string(),
                        None => format!("Auto ({})", auto_region.name()),
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut record.region,
                            None,
                            format!("Auto ({})", auto_region.name()),
                        );
                        for region in Region::ALL {
                            ui.selectable_value(&mut record.region, Some(region), region.name());
                        }
                    });
                ui.end_row();
                ui.label("Console");
                egui::ComboBox::from_id_salt("console")
                    .selected_text(match record.console {
                        Some(console) => console.name().to_string(),
                        None => format!("Auto ({})", auto_console.name()),
                    })
                    .show_ui(ui, |ui| {
                        ui.selectable_value(
                            &mut record.console,
                            None,
                            format!("Auto ({})", auto_console.name()),
                        );
                        for console in Console::ALL {
                            ui.selectable_value(&mut record.console, Some(console), console.name());
                        }
                    });
                ui.end_row();
            });
        let mut corruption = !record.no_oam_corruption;
        ui.checkbox(&mut corruption, "OAM row corruption")
            .on_hover_text(
                "What the hardware does when a game turns rendering off mid-frame. \
                 Turn it off only for blargg's sprite_overflow_tests.",
            );
        record.no_oam_corruption = !corruption;

        ui.add_space(10.0);
        let play = ui
            .add_enabled(
                problem.is_none(),
                egui::Button::new(RichText::new("▶  Play").size(18.0))
                    .min_size(egui::vec2(ui.available_width(), 36.0)),
            )
            .on_disabled_hover_text("This game can't run in the emulator");
        ui.add_space(4.0);
        ui.weak(format!(
            "Last played: {}   ·   Time played: {}",
            format::ago(format::unix_now(), record.last_played),
            format::play_time(record.play_secs)
        ));

        if let Some((failed_key, output)) = &self.failed_run {
            if failed_key == &key {
                ui.add_space(8.0);
                egui::CollapsingHeader::new(
                    RichText::new("Couldn't start").color(ui.visuals().error_fg_color),
                )
                .default_open(true)
                .show(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .max_height(160.0)
                        .show(ui, |ui| {
                            ui.add(
                                egui::Label::new(
                                    RichText::new(output.trim_end()).monospace().small(),
                                )
                                .wrap()
                                .selectable(true),
                            );
                        });
                });
            }
        }

        if record != self.record(&key) {
            self.set_record(&key, record);
            self.refresh_list();
        }
        if play.clicked() {
            self.play(&key);
        }
    }

    fn settings_window(&mut self, ctx: &egui::Context) {
        let mut open = self.settings_open;
        egui::Window::new("Settings")
            .open(&mut open)
            .collapsible(false)
            .resizable(true)
            .default_width(520.0)
            .show(ctx, |ui| {
                ui.strong("Game folders");
                ui.weak("Subfolders are included.");
                let mut remove_folder = None;
                for (i, folder) in self.config.folders.iter().enumerate() {
                    ui.horizontal(|ui| {
                        if ui.small_button("✖").on_hover_text("Remove from the library").clicked() {
                            remove_folder = Some(i);
                        }
                        ui.label(folder.display().to_string());
                    });
                }
                if ui.button("📁 Add folder…").clicked() {
                    self.choose_folder();
                }
                ui.weak("Or type or paste its path:");
                self.folder_path_input(ui);

                let mut remove_file = None;
                if !self.config.files.is_empty() {
                    ui.add_space(8.0);
                    ui.strong("Single games");
                    for (i, file) in self.config.files.iter().enumerate() {
                        ui.horizontal(|ui| {
                            if ui.small_button("✖").on_hover_text("Remove from the library").clicked() {
                                remove_file = Some(i);
                            }
                            ui.label(file.display().to_string());
                        });
                    }
                }
                if let Some(i) = remove_folder {
                    self.config.folders.remove(i);
                    self.rescan();
                }
                if let Some(i) = remove_file {
                    self.config.files.remove(i);
                    self.rescan();
                }
                ui.add_space(4.0);
                ui.weak("Removing a folder or game only takes it off this list; no files are deleted.");

                ui.add_space(8.0);
                ui.separator();
                self.titles_settings(ui);
                ui.add_space(8.0);
                ui.separator();
                self.video_settings(ui);
            });
        self.settings_open = open;
    }

        fn controls_window(&mut self, ctx: &egui::Context) {
        let mut open = self.controls_open;
        egui::Window::new("Controls")
            .open(&mut open)
            .collapsible(false)
            .resizable(false)
            .show(ctx, |ui| {
                ui.horizontal(|ui| {
                    for player in 0..2 {
                        let text = format!("Player {}", player + 1);
                        if ui.selectable_value(&mut self.controls_player, player, text).clicked() {
                            self.capture = None;
                        }
                    }
                });
                ui.add_space(6.0);
                let player = self.controls_player;
                egui::Grid::new("bindings")
                    .num_columns(3)
                    .spacing([12.0, 4.0])
                    .show(ui, |ui| {
                        ui.strong("NES");
                        ui.strong("Keyboard");
                        ui.strong("Gamepad");
                        ui.end_row();
                        for (button, (_, name)) in BUTTONS.iter().enumerate() {
                            ui.label(*name);
                            self.binding_box(ui, Capture { player, button, gamepad: false });
                            self.binding_box(ui, Capture { player, button, gamepad: true });
                            ui.end_row();
                        }
                        if player == 1 {
                            ui.label("Microphone");
                            self.binding_box(ui, Capture { player, button: MICROPHONE, gamepad: false });
                            ui.end_row();
                        }
                    });
                ui.add_space(6.0);
                let note = match (&self.capture_note, self.capture) {
                    (Some(note), _) => RichText::new(note).color(ui.visuals().warn_fg_color),
                    (None, Some(capture)) if capture.gamepad => {
                        RichText::new("Press a gamepad button (Esc to stop)").strong()
                    }
                    (None, Some(_)) => RichText::new("Press a key (Backspace clears it, Esc stops)").strong(),
                    (None, None) => RichText::new("Click a box, then press the key or gamepad button for it."),
                };
                ui.label(note);
                ui.weak(
                    "Esc, Tab, F5, F8, F9 and F11 are the launcher's own keys while a game runs. \
                     The first gamepad plugged in is player 1 and the second player 2; the \
                     left stick works as the d-pad too.",
                );
                ui.add_space(4.0);
                if ui.button(format!("Reset player {} to the defaults", player + 1)).clicked() {
                    self.config.controls.players[player] = PlayerControls::default_for(player);
                    if player == 1 {
                        self.config.controls.microphone = vec!["M".into()];
                    }
                    self.capture = None;
                    self.controls_changed();
                }
            });
        if !open {
            self.capture = None;
        }
        self.controls_open = open;
    }

    fn binding_box(&mut self, ui: &mut egui::Ui, which: Capture) {
        let names = self.bindings(which).clone();
        let label = if names.is_empty() {
            "—".to_string()
        } else if which.gamepad {
            names.iter().map(|n| controls::pad_label(n)).collect::<Vec<_>>().join(" / ")
        } else {
            names.iter().map(|n| controls::key_label(n)).collect::<Vec<_>>().join(" / ")
        };
        let waiting = self.capture == Some(which);
        let text = if waiting { "…".to_string() } else { label };
        let button = egui::Button::new(text)
            .selected(waiting)
            .min_size(egui::vec2(130.0, 0.0));
        if ui.add(button).clicked() {
            self.capture = if waiting { None } else { Some(which) };
            self.capture_note = None;
            self.capture_modifiers = ui.input(|i| i.modifiers);
        }
    }

    fn bindings(&mut self, which: Capture) -> &mut Vec<String> {
        let controls = &mut self.config.controls;
        if which.button == MICROPHONE {
            &mut controls.microphone
        } else if which.gamepad {
            &mut controls.players[which.player].pad[which.button]
        } else {
            &mut controls.players[which.player].keys[which.button]
        }
    }

    fn set_binding(&mut self, which: Capture, names: Vec<String>) {
        *self.bindings(which) = names;
        self.capture = None;
        self.capture_note = None;
        self.controls_changed();
    }

    fn controls_changed(&mut self) {
        self.machine.set_pad_layouts(self.config.controls.pad_layouts());
        self.ctx.request_repaint();
    }

    /// While a keyboard box waits: takes the key presses (so nothing else
    /// sees them) and binds the first one.
    fn capture_key(&mut self, ctx: &egui::Context) {
        let Some(capture) = self.capture else {
            return;
        };
        let (events, modifiers) = ctx.input_mut(|i| {
            let keys: Vec<egui::Event> = i
                .events
                .iter()
                .filter(|e| matches!(e, egui::Event::Key { .. }))
                .cloned()
                .collect();
            i.events.retain(|e| !matches!(e, egui::Event::Key { .. }));
            (keys, i.modifiers)
        });
        for event in events {
            let egui::Event::Key { key, physical_key, pressed: true, repeat: false, .. } = event else {
                continue;
            };
            let key = physical_key.unwrap_or(key);
            if key == Key::Escape {
                self.capture = None;
                self.capture_note = None;
                return;
            }
            if capture.gamepad {
                continue;
            }
            if key == Key::Backspace || key == Key::Delete {
                self.set_binding(capture, Vec::new());
            } else if RESERVED_KEYS.contains(&key) {
                self.capture_note = Some(format!("{} is one of the launcher's keys", key.name()));
            } else {
                self.set_binding(capture, vec![key.name().to_string()]);
            }
            return;
        }
        if !capture.gamepad {
            let before = self.capture_modifiers;
            for (now, was, name) in [
                (modifiers.shift, before.shift, "Shift"),
                (modifiers.ctrl, before.ctrl, "Ctrl"),
                (modifiers.alt, before.alt, "Alt"),
            ] {
                if now && !was {
                    self.set_binding(capture, vec![name.to_string()]);
                    break;
                }
            }
        }
        self.capture_modifiers = modifiers;
    }

    fn gamepad(&mut self, button: Button, pressed: bool) {
        self.pad_held.retain(|&b| b != button);
        if pressed {
            self.pad_held.push(button);
        }
        if let Some(capture) = self.capture.filter(|c| c.gamepad) {
            if pressed {
                self.set_binding(capture, vec![button.string()]);
            }
            return;
        }
        if !pressed {
            if self.pad_repeat.is_some_and(|(held, _)| held == button) {
                self.pad_repeat = None;
            }
            return;
        }
        match &self.playing {
            Some(playing) if !playing.paused => {
                let back_start = self.pad_held.contains(&Button::Back)
                    && self.pad_held.contains(&Button::Start);
                if button == Button::Guide || back_start {
                    self.pause_game(true);
                }
            }
            Some(_) => self.menu_pad(button),
            None => self.library_pad(button),
        }
    }

    fn menu_pad(&mut self, button: Button) {
        let key = match button {
            Button::DPadUp => Key::ArrowUp,
            Button::DPadDown => Key::ArrowDown,
            Button::DPadLeft => Key::ArrowLeft,
            Button::DPadRight => Key::ArrowRight,
            Button::A => Key::Enter,
            Button::B | Button::Start | Button::Guide => {
                self.pause_game(false);
                return;
            }
            _ => return,
        };
        self.pad_keys.push(key);
        self.ctx.request_repaint();
    }

    fn library_pad(&mut self, button: Button) {
        match button {
            Button::DPadUp | Button::DPadDown | Button::DPadLeft | Button::DPadRight => {
                self.pad_move(button);
                self.pad_repeat = Some((button, Instant::now() + PAD_REPEAT_DELAY));
            }
            Button::LeftShoulder => self.move_selection(Move::PageUp),
            Button::RightShoulder => self.move_selection(Move::PageDown),
            Button::A | Button::Start => self.play_selected(),
            Button::X => {
                if let Some(key) = self.selected.clone() {
                    self.toggle_favorite(&key);
                }
            }
            Button::Y => {
                self.config.view = match self.config.view {
                    LibraryView::List => LibraryView::Covers,
                    LibraryView::Covers => LibraryView::List,
                };
                self.scroll_to_selected = true;
            }
            _ => {}
        }
        self.ctx.request_repaint();
    }

    fn pad_move(&mut self, button: Button) {
        self.move_selection(match button {
            Button::DPadUp => Move::Up,
            Button::DPadDown => Move::Down,
            Button::DPadLeft => Move::Left,
            _ => Move::Right,
        });
    }

    fn repeat_pad(&mut self, ctx: &egui::Context) {
        let Some((button, at)) = self.pad_repeat else {
            return;
        };
        let now = Instant::now();
        if now >= at {
            self.pad_move(button);
            self.pad_repeat = Some((button, now + PAD_REPEAT));
            ctx.request_repaint_after(PAD_REPEAT);
        } else {
            ctx.request_repaint_after(at - now);
        }
    }

    fn titles_settings(&mut self, ui: &mut egui::Ui) {
        ui.strong("Titles and covers");
        ui.weak(
            "Names your games from No-Intro's list of NES games, and gets their box art \
             from the libretro thumbnails project (both downloaded from GitHub). Games \
             are recognized by their ROM, whatever the file is called.",
        );
        if self.game_list.is_empty() {
            ui.label("Not downloaded yet.");
        } else {
            let found = self.games.iter().filter(|g| g.list_name.is_some()).count();
            ui.label(format!(
                "{found} of your {} games found in the list ({} games in it).",
                self.games.len(),
                self.game_list.len()
            ));
        }
        ui.horizontal(|ui| {
            let text = if self.game_list.is_empty() {
                "⬇ Get titles and covers"
            } else {
                "⟳ Update titles and covers"
            };
            if ui.add_enabled(!self.downloads.busy(), egui::Button::new(text)).clicked() {
                self.get_titles_and_covers();
            }
            if let Some(status) = self.downloads.status() {
                ui.spinner();
                ui.label(status);
            }
        });
        if let Some(error) = &self.downloads.error {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if !self.game_list.is_empty() {
            let get_covers = ui.checkbox(&mut self.config.get_covers, "Get covers for games as they're added");
            if get_covers.changed() {
                self.fetch_missing_covers();
            }
        }
    }
}

fn region(game: &Game, record: Option<&GameRecord>) -> (Region, bool) {
    if let Some(region) = record.and_then(|r| r.region) {
        return (region, true);
    }
    match &game.rom {
        Ok(rom) => (game.auto_region(rom).0, false),
        Err(_) => (Region::Ntsc, false)
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.check_machine();
        self.check_online();
        if ctx.input(|i| i.viewport().close_requested()) {
            self.finish_game();
        }
        self.capture_key(ctx);
        if ctx.input_mut(|i| i.consume_key(egui::Modifiers::NONE, Key::F11)) {
            self.set_fullscreen(!self.fullscreen);
        }
        if let Some(playing) = &self.playing {
            if playing.paused {
                self.menu_keys(ctx);
            } else {
                self.game_keys(ctx);
            }
            self.game_screen(ctx);
            return;
        }
        self.handle_dropped_files(ctx);
        self.handle_keys(ctx);
        self.repeat_pad(ctx);

        egui::TopBottomPanel::top("toolbar")
            .frame(egui::Frame::side_top_panel(&ctx.style()).inner_margin(8))
            .show(ctx, |ui| self.toolbar(ui));
        egui::TopBottomPanel::bottom("status")
            .frame(
                egui::Frame::side_top_panel(&ctx.style())
                    .inner_margin(egui::Margin::symmetric(8, 4)),
            )
            .show(ctx, |ui| self.status_bar(ui));
        egui::SidePanel::right("details")
            .resizable(true)
            .default_width(320.0)
            .width_range(260.0..=520.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.details(ui));
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            if self.shown.is_empty() {
                self.empty_library(ui);
            } else if self.config.view == LibraryView::Covers {
                if self.game_list.is_empty() {
                    self.covers_prompt(ui);
                }
                self.game_covers(ui);
            } else {
                self.game_list(ui);
            }
        });
        self.settings_window(ctx);
        self.controls_window(ctx);

        if ctx.input(|i| !i.raw.hovered_files.is_empty()) {
            let screen = ctx.screen_rect();
            let painter = ctx.layer_painter(egui::LayerId::new(
                egui::Order::Foreground,
                egui::Id::new("drop")
            ));
            painter.rect_filled(screen, 0.0, Color32::from_black_alpha(160));
            painter.text(
                screen.center(),
                egui::Align2::CENTER_CENTER,
                "Drop .nes files or folders to add them",
                egui::FontId::proportional(22.0),
                Color32::WHITE
            );
        }
    }

    fn raw_input_hook(&mut self, _ctx: &egui::Context, raw_input: &mut egui::RawInput) {
        for key in self.pad_keys.drain(..) {
            for pressed in [true, false] {
                raw_input.events.push(egui::Event::Key {
                    key,
                    physical_key: None,
                    pressed,
                    repeat: false,
                    modifiers: egui::Modifiers::NONE,
                });
            }
        }
    }

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.config);
    }

    fn auto_save_interval(&self) -> std::time::Duration {
        Duration::from_secs(5)
    }
}

fn state_slots(
    ui: &mut egui::Ui,
    slots: &[(Option<SystemTime>, Option<egui::TextureId>)],
    current: usize,
) -> Option<MenuChoice> {
    let mut choice = None;
    ui.strong("Save states");
    ui.weak("F5 saves and F8 loads the outlined slot.");
    ui.add_space(4.0);
    let now = format::unix_now();
    egui::Grid::new("state_slots")
        .num_columns(2)
        .spacing([10.0, 10.0])
        .show(ui, |ui| {
            for (slot, (saved, picture)) in slots.iter().enumerate() {
                ui.vertical(|ui| {
                    let (rect, _) = ui.allocate_exact_size(SLOT_PICTURE, egui::Sense::hover());
                    let painter = ui.painter();
                    match picture {
                        Some(id) => {
                            let uv = egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
                            painter.image(*id, rect, uv, Color32::WHITE);
                        }
                        None => {
                            painter.rect_filled(rect, 4.0, ui.visuals().extreme_bg_color);
                            painter.text(
                                rect.center(),
                                egui::Align2::CENTER_CENTER,
                                "Empty",
                                egui::FontId::proportional(13.0),
                                ui.visuals().weak_text_color(),
                            );
                        }
                    }
                    if slot == current {
                        painter.rect_stroke(
                            rect,
                            2.0,
                            ui.visuals().selection.stroke,
                            egui::StrokeKind::Outside,
                        );
                    }
                    let when = saved
                        .and_then(|t| t.duration_since(SystemTime::UNIX_EPOCH).ok())
                        .map_or("empty".to_string(), |t| format::ago(now, Some(t.as_secs())));
                    ui.small(format!("Slot {}  ·  {when}", slot + 1));
                    ui.horizontal(|ui| {
                        if ui.small_button("Save").clicked() {
                            choice = Some(MenuChoice::SaveState(slot));
                        }
                        let load = ui.add_enabled(saved.is_some(), egui::Button::new("Load").small());
                        if load.clicked() {
                            choice = Some(MenuChoice::LoadState(slot));
                        }
                    });
                });
                if slot % 2 == 1 {
                    ui.end_row();
                }
            }
        });
    choice
}

fn state_path(game: &Path, slot: usize) -> Option<PathBuf> {
    let data = eframe::storage_dir("NES Launcher")?;
    let name: String = game
        .file_stem()?
        .to_string_lossy()
        .chars()
        .map(|c| if c.is_alphanumeric() || " -_().".contains(c) { c } else { '_' })
        .take(60)
        .collect();
    let id = game
        .to_string_lossy()
        .bytes()
        .fold(0x811c_9dc5_u32, |hash, b| (hash ^ b as u32).wrapping_mul(0x0100_0193));
    let folder = data.join("states").join(format!("{name}-{id:08x}"));
    Some(folder.join(format!("slot{}.state", slot + 1)))
}

fn visible_picture(picture: &[u8]) -> egui::ColorImage {
    let mut pixels = Vec::with_capacity(VISIBLE_WIDTH * VISIBLE_HEIGHT * 3);
    for row in picture.chunks_exact(256 * 3).skip(OVERSCAN).take(VISIBLE_HEIGHT) {
        pixels.extend_from_slice(&row[OVERSCAN * 3..(OVERSCAN + VISIBLE_WIDTH) * 3]);
    }
    egui::ColorImage::from_rgb([VISIBLE_WIDTH, VISIBLE_HEIGHT], &pixels)
}

fn game_size(available: egui::Vec2, size: ScreenSize, tv_aspect: bool) -> egui::Vec2 {
    let pixel_width = if tv_aspect { 8.0 / 7.0 } else { 1.0 };
    let width = VISIBLE_WIDTH as f32 * pixel_width;
    let height = VISIBLE_HEIGHT as f32;
    let scale = (available.x / width).min(available.y / height);
    let scale = match size {
        ScreenSize::WholePixels if scale >= 1.0 => scale.floor(),
        _ => scale,
    };
    egui::vec2(width * scale, height * scale)
}
#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_visible_picture_crops_the_overscan() {
        let mut picture = vec![0; 256 * 240 * 3];
        // a red pixel at the first visible spot, (8, 8)
        let i = (8 * 256 + 8) * 3;
        picture[i] = 255;
        let image = visible_picture(&picture);
        assert_eq!(image.size, [240, 224]);
        assert_eq!(image.pixels[0], Color32::from_rgb(255, 0, 0));
        assert_eq!(image.pixels[1], Color32::from_rgb(0, 0, 0));
    }

    #[test]
    fn test_game_size_keeps_whole_pixels() {
        let whole = |w, h| game_size(egui::vec2(w, h), ScreenSize::WholePixels, false);
        assert_eq!(whole(800.0, 700.0), egui::vec2(720.0, 672.0));
        assert_eq!(whole(1100.0, 500.0), egui::vec2(480.0, 448.0));
        assert_eq!(whole(120.0, 112.0), egui::vec2(120.0, 112.0));
    }

    #[test]
    fn test_game_size_fill_and_tv_shape() {
        let fill = game_size(egui::vec2(800.0, 700.0), ScreenSize::FillWindow, false);
        assert_eq!(fill, egui::vec2(750.0, 700.0));
        // 3x high, and 8/7 as wide as square pixels would be
        let tv = game_size(egui::vec2(1100.0, 700.0), ScreenSize::WholePixels, true);
        assert_eq!(tv.y, 672.0);
        assert!((tv.x - 720.0 * 8.0 / 7.0).abs() < 0.01, "{tv:?}");
    }

     #[test]
    fn test_state_paths_are_per_game_and_slot() {
        let a = state_path(Path::new("/roms/Game (USA).nes"), 0);
        let b = state_path(Path::new("/other/Game (USA).nes"), 0);
        let a2 = state_path(Path::new("/roms/Game (USA).nes"), 1);
        if let (Some(a), Some(b), Some(a2)) = (a, b, a2) {
            assert_ne!(a.parent(), b.parent(), "same name, different ROM");
            assert_eq!(a.parent(), a2.parent());
            assert!(a.ends_with("slot1.state") && a2.ends_with("slot2.state"));
            assert!(a.parent().unwrap().file_name().unwrap().to_string_lossy().starts_with("Game (USA)-"));
        }
    }
}