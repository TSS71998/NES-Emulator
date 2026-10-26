//! NES Launcher: a game library window for the emulator. It finds your
//! .nes files, shows what it knows about each one, and plays the one you
//! pick in its window (with the emulator as a library, `nes_emulator`).

// release builds are a windowed app on Windows: no console window
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod app;
mod config;
mod format;
mod gamelist;
mod library;
mod machine;

use eframe::egui;

fn main() -> eframe::Result {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title("NES Launcher")
            .with_inner_size([1100.0, 660.0])
            .with_min_inner_size([720.0, 400.0])
            .with_drag_and_drop(true),
        ..Default::default()
    };
    eframe::run_native(
        "NES Launcher", 
        options, 
        Box::new(|cc| Ok(Box::new(app::App::new(cc))))
    )
}