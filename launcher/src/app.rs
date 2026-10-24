//! The launcher window: a toolbar, the game list, a details panel for the
//! selected game, and a status bar.

use crate::config::{Config, GameRecord, SortKey};
use crate::format;
use crate::launch::{self, Running};
use crate::library::{self, Console, Game, Region};
use eframe::egui::{self, Align, Color32, Key, Layout, RichText, TextWrapMode};
use egui_extras::{Column, TableBuilder};
use std::path::PathBuf;
use std::time::Duration;

const ROW_HEIGHT: f32 = 24.0;

pub struct App {
    config: Config,
    games: Vec<Game>,
    shown: Vec<usize>,
    selected: Option<String>,
    search: String,
    focus_search: bool,
    scroll_to_selected: bool,
    running: Option<Running>,
    failed_run: Option<(String, String)>,
    message: Option<(String, bool)>,
    settings_open: bool,
    emulator: Option<PathBuf>
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
            emulator: launch::find_emulator(config.emulator.as_deref()),
            config,
            games: Vec::new(),
            shown: Vec::new(),
            selected: None,
            search: String::new(),
            focus_search: false,
            scroll_to_selected: false,
            running: None,
            failed_run: None,
            message: None,
            settings_open: false
        };
        app.rescan();
        app
    }

    fn rescan(&mut self) {
        self.games = library::scan(&self.config.folders, &self.config.files);
        self.refresh_list();
    }

    fn refresh_list(&mut self) {
        let search = self.search.to_lowercase();
        let mut shown: Vec<usize> = (0..self.games.len())
            .filter(|&i| {
                let game = &self.games[i];
                search.is_empty()
                    || game.title.to_lowercase().contains(&search)
                    || game.file_name.to_lowercase().contains(&search)
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
        if let Some(running) = &self.running {
            self.message = Some((format!("{} is still running.", running.title), true));
            return;
        }
        let Some(emulator) = self.emulator.clone() else {
            self.message = Some(("Emulator not found: choose it in Settings.".into(), true));
            self.settings_open = true;
            return;
        };
        if let Some(problem) = game.problem() {
            self.message = Some((problem, true));
            return;
        }
        let record = self.record(key);
        let args = launch::emulator_args(&game.path, &record);
        match Running::start(&emulator, args, game.key(), game.title.clone()) {
            Ok(running) => {
                self.message = Some((format!("Started {}.", game.title), false));
                self.failed_run = None;
                self.running = Some(running);
            }
            Err(e) => {
                self.message = Some((format!("Couldn't start the emulator: {e}"), true));
            }
        }
    }

    fn check_running(&mut self, ctx: &egui::Context) {
        let Some(running) = &mut self.running else {
            return;
        };
        let Some(finished) = running.poll() else {
            ctx.request_repaint_after(Duration::from_millis(500));
            return;
        };
        self.running = None;

        let played = finished.played.as_secs();
        if finished.status.success() || played >= 5 {
            let mut record = self.record(&finished.key);
            record.play_secs += played;
            record.last_played = Some(format::unix_now().saturating_sub(played));
            self.set_record(&finished.key, record);
        }
        self.refresh_list();

        if finished.status.success() {
            self.message = Some((
                format!(
                    "PLayed {} for {}.",
                    finished.title,
                    format::clock(finished.played.as_secs())
                ),
                false
            ));
        } else {
            let reason = finished
                .output
                .lines()
                .rev()
                .find(|l| l.starts_with("error"))
                .map(str::to_string)
                .unwrap_or_else(|| format!("the emulator stopped ({})", finished.status));
            self.message = Some((format!("{}: {reason}", finished.title), true));
            self.failed_run = Some((finished.key, finished.output));
        }
    }

    fn add_folder(&mut self, folder: PathBuf) {
        if !self.config.folders.contains(&folder) {
            self.config.folders.push(folder);
            self.rescan();
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
        let (down, up, enter) = ctx.input(|i| {
            (
                i.key_pressed(Key::ArrowDown),
                i.key_pressed(Key::ArrowUp),
                i.key_pressed(Key::Enter)
            )
        });
        let position = self
            .selected
            .as_ref()
            .and_then(|key| self.shown.iter().position(|&i| &self.games[i].key() == key));
        let last = self.shown.len() - 1;
        let new_position = match (down, up, position) {
            (true, _, Some(p)) => Some((p + 1).min(last)),
            (_, true, Some(p)) => Some(p.saturating_sub(1)),
            (true, _, None) | (_, true, None) => Some(0),
            _ => None
        };
        if let Some(p) = new_position {
            self.select(Some(self.games[self.shown[p]].key()));
        }
        if enter {
            if let Some(key) = self.selected.clone() {
                self.play(&key);
            }
        }
    }

    fn toolbar(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            if ui.button("📁 Add folder…").clicked() {
                if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                    self.add_folder(folder);
                }
            }
            if ui.button("➕ Add games…").clicked() {
                if let Some(files) = rfd::FileDialog::new()
                    .add_filter("NES ROMs", &["nes, NES"])
                    .pick_files()
                {
                    self.add_files(files);
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
                if self.emulator.is_none() {
                    let warning =
                        RichText::new("⚠ Emulator not found").color(ui.visuals().warn_fg_color);
                    if ui.link(warning).clicked() {
                        self.settings_open = true;
                    }
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
            if let Some(running) = &mut self.running {
                ui.separator();
                ui.label(
                    RichText::new(format!(
                        "▶ Playing {}  {}",
                        running.title,
                        format::clock(running.elapsed().as_secs())
                    ))
                    .strong(),
                );
                if ui
                    .small_button("■ Stop")
                    .on_hover_text("Close the emulator")
                    .clicked()
                {
                    running.stop();
                }
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

    fn empty_library(&mut self, ui: &mut egui::Ui) {
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() / 3.0);
            if self.games.is_empty() {
                ui.heading("No games yet");
                ui.add_space(6.0);
                ui.label(
                    "Add a folder with your .nes folders, or drop files or folders onto this window."
                );
                ui.add_space(12.0);
                if ui.button("📁 Add folder…").clicked() {
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        self.add_folder(folder);
                    }
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
        let Some(game) = self.selected_game() else {
            ui.weak("Select a game to see its details.");
            return;
        };
        let key = game.key();
        let file_name = game.file_name.clone();
        let mut record = self.record(&key);

        ui.add_space(4.0);
        ui.heading(&game.title);
        ui.label(RichText::new(&game.file_name).weak().small());
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
                        let (auto_region, region_source) = library::auto_region(rom, &file_name);
                        let (auto_console, console_source) = library::auto_console(&file_name);
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
            Ok(rom) => (
                library::auto_region(rom, &file_name).0,
                library::auto_console(&file_name).0,
            ),
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
        let can_play = problem.is_none() && self.running.is_none() && self.emulator.is_some();
        let play = ui
            .add_enabled(
                can_play,
                egui::Button::new(RichText::new("▶  Play").size(18.0))
                    .min_size(egui::vec2(ui.available_width(), 36.0)),
            )
            .on_disabled_hover_text(if self.running.is_some() {
                "A game is already running"
            } else if self.emulator.is_none() {
                "Emulator not found: choose it in Settings"
            } else {
                "This game can't run in the emulator"
            });
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
                    RichText::new("Last run failed").color(ui.visuals().error_fg_color),
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
                ui.strong("Emulator");
                match &self.emulator {
                    Some(path) => {
                        ui.add(
                            egui::Label::new(
                                RichText::new(path.display().to_string()).monospace().small(),
                            )
                            .selectable(true),
                        );
                    }
                    None => {
                        let text = match &self.config.emulator {
                            Some(path) => format!("{} doesn't exist.", path.display()),
                            None => "Not found next to the launcher or in the emulator's target folder. \
                                     Build it with `cargo build --release` in the emulator's folder, \
                                     or choose it here."
                                .to_string(),
                        };
                        ui.colored_label(ui.visuals().warn_fg_color, text);
                    }
                }
                ui.horizontal(|ui| {
                    if ui.button("Choose…").clicked() {
                        if let Some(path) = rfd::FileDialog::new().pick_file() {
                            self.config.emulator = Some(path);
                            self.emulator = launch::find_emulator(self.config.emulator.as_deref());
                        }
                    }
                    let label = if self.config.emulator.is_none() {
                        "Look again"
                    } else {
                        "Find automatically"
                    };
                    if ui.button(label).clicked() {
                        self.config.emulator = None;
                        self.emulator = launch::find_emulator(None);
                    }
                });

                ui.add_space(8.0);
                ui.separator();
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
                    if let Some(folder) = rfd::FileDialog::new().pick_folder() {
                        self.add_folder(folder);
                    }
                }

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
            });
        self.settings_open = open;
    }
}

fn region(game: &Game, record: Option<&GameRecord>) -> (Region, bool) {
    if let Some(region) = record.and_then(|r| r.region) {
        return (region, true);
    }
    match &game.rom {
        Ok(rom) => (library::auto_region(rom, &game.file_name).0, false),
        Err(_) => (Region::Ntsc, false)
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.check_running(ctx);
        self.handle_dropped_files(ctx);
        self.handle_keys(ctx);

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
            } else {
                self.game_list(ui);
            }
        });
        self.settings_window(ctx);

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

    fn save(&mut self, storage: &mut dyn eframe::Storage) {
        eframe::set_value(storage, eframe::APP_KEY, &self.config);
    }

    fn auto_save_interval(&self) -> std::time::Duration {
        Duration::from_secs(5)
    }

    fn on_exit(&mut self, _gl: Option<&eframe::glow::Context>) {
        if let Some(running) = self.running.take() {
            let played = running.elapsed().as_secs();
            let mut record = self.record(&running.key);
            record.play_secs += played;
            record.last_played = Some(format::unix_now().saturating_add(played));
            self.set_record(&running.key, record);
        }
    }
}

