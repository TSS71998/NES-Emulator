pub mod apu;
pub mod audio;
pub mod bus;
pub mod cartridge;
pub mod cpu;
pub mod frame_limiter;
pub mod joypad;
pub mod opcodes;
pub mod ppu;
pub mod render;
pub mod trace;
pub mod mapper;

use audio::AudioOutput;
use bus::Bus;
use cartridge::Rom;
use cpu::CPU;
use frame_limiter::FrameLimiter;
use mapper::SharedMapper;
use ppu::NesPPU;
use render::frame::Frame;

use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use std::collections::HashMap;

#[macro_use]
extern crate bitflags;

const OVERSCANE_LINES: u32 = 8;
const VISIBLE_LINES: u32 = 240 - 2 * OVERSCANE_LINES;
const SCALE: u32 = 3;

fn main() {
    let sdl_context = sdl2::init().unwrap();
    let video_subsystem = sdl_context.video().unwrap();
    let window = video_subsystem
        .window("NES Emulator", 256 * SCALE, VISIBLE_LINES * SCALE)
        .position_centered()
        .build()
        .unwrap();

    // Vsync paces frames at a fixed delay after emulation, but only works as the
    // frame clock on a ~60 Hz display; otherwise fall back to the frame limiter.
    let refresh_rate = window
        .display_index()
        .and_then(|i| video_subsystem.current_display_mode(i))
        .map(|mode| mode.refresh_rate)
        .unwrap_or(0);
    let use_vsync = (59..=61).contains(&refresh_rate);

    let mut canvas_builder = window.into_canvas();
    if use_vsync {
        canvas_builder = canvas_builder.present_vsync();
    }
    let mut canvas = canvas_builder.build().unwrap();
    let mut event_pump = sdl_context.event_pump().unwrap();

    let audio_subsystem = sdl_context.audio().unwrap();
    let mut audio_output = AudioOutput::new(&audio_subsystem);
    let sample_rate = audio_output.sample_rate();
    let mut frame_limiter = FrameLimiter::new(frame_limiter::NES_FPS);

    let creator = canvas.texture_creator();
    let mut texture = creator
        .create_texture_target(PixelFormatEnum::RGB24, 256, 240)
        .unwrap();

    let rom_arg = std::env::args().nth(1).unwrap_or_else(|| "smb.nes".to_string());
    let rom_path = find_rom(&rom_arg);
    let bytes: Vec<u8> = std::fs::read(&rom_path)
        .unwrap_or_else(|e| exit_with_error(&format!("can't read {}: {}", rom_path.display(), e)));
    let rom = Rom::new(&bytes).unwrap_or_else(|e| exit_with_error(&e));
    let cartridge = mapper::from_rom(rom).unwrap_or_else(|e| exit_with_error(&e));

    let save_path = rom_path.with_extension("sav");
    if let Ok(data) = std::fs::read(&save_path) {
        cartridge.borrow_mut().load_battery_ram(&data);
    }
    let mut saver = BatterySaver::new(cartridge.clone(), save_path);

    let mut frame = Frame::new();

    let mut key_map = HashMap::new();
    key_map.insert(Keycode::Down, joypad::JoypadButton::DOWN);
    key_map.insert(Keycode::Up, joypad::JoypadButton::UP);
    key_map.insert(Keycode::Right, joypad::JoypadButton::RIGHT);
    key_map.insert(Keycode::Left, joypad::JoypadButton::LEFT);
    key_map.insert(Keycode::Space, joypad::JoypadButton::SELECT);
    key_map.insert(Keycode::Return, joypad::JoypadButton::START);
    key_map.insert(Keycode::A, joypad::JoypadButton::BUTTON_A);
    key_map.insert(Keycode::S, joypad::JoypadButton::BUTTON_B);

    // run the game cycle
    let mut bus = Bus::new(cartridge, move |ppu: &NesPPU, joypads: &mut [joypad::Joypad; 2]| {
        let joypad = &mut joypads[0];
        saver.on_frame();
        render::render(ppu, &mut frame);
        texture.update(None, &frame.data, 256 * 3).unwrap();

        let visible = Rect::new(0, OVERSCANE_LINES as i32, 256, VISIBLE_LINES);
        canvas.copy(&texture, visible, None).unwrap();

        if !use_vsync {
            frame_limiter.wait();
        }
        canvas.present();
        for event in event_pump.poll_iter() {
            match event {
                Event::Quit { .. }
                | Event::KeyDown {
                    keycode: Some(Keycode::Escape),
                    ..
                } => {
                    saver.save();
                    std::process::exit(0)
                }
                Event::KeyDown { keycode, .. } => {
                    if let Some(key) = key_map.get(&keycode.unwrap_or(Keycode::Ampersand)) {
                        joypad.set_button_pressed_status(*key, true);
                    }
                }
                Event::KeyUp { keycode, .. } => {
                    if let Some(key) = key_map.get(&keycode.unwrap_or(Keycode::Ampersand)) {
                        joypad.set_button_pressed_status(*key, false);
                    }
                }
                _ => { /* do nothing */ }
            }
        }
    });

    bus.set_audio_callback(sample_rate, move |samples: &[f32]| {
        audio_output.push(samples)
    });

    let mut cpu = CPU::new(bus);

    cpu.reset();
    cpu.run();
}

fn exit_with_error(message: &str) -> ! {
    eprintln!("error: {}", message);
    std::process::exit(1);
}

fn find_rom(name: &str) -> std::path::PathBuf {
    let path = std::path::Path::new(name);
    let in_roms = std::path::Path::new("roms").join(path);
    if !path.exists() && in_roms.exists() {
        in_roms
    } else {
        path.to_path_buf()
    }
}

struct BatterySaver {
    cartridge: SharedMapper,
    path: std::path::PathBuf,
    last_saved: Vec<u8>,
    frames: u32
}

impl BatterySaver {
    const SAVE_INTERVAL_FRAMES: u32 = 300;

    fn new(cartridge: SharedMapper, path: std::path::PathBuf) -> Self {
        let last_saved = cartridge.borrow().battery_ram().map(|r| r.to_vec()).unwrap_or_default();
        BatterySaver { cartridge, path, last_saved, frames: 0 }
    }

    fn on_frame(&mut self) {
        self.frames += 1;
        if self.frames >= Self::SAVE_INTERVAL_FRAMES {
            self.frames = 0;
            self.save();
        }
    }

    fn save(&mut self) {
        let cartridge = self.cartridge.borrow();
        let Some(ram) = cartridge.battery_ram() else { return };
        if ram == &self.last_saved[..] {
            return;
        }
        match std::fs::write(&self.path, ram) {
            Ok(()) => self.last_saved = ram.to_vec(),
            Err(e) => eprintln!("warning: couldn't write {}: {}", self.path.display(), e)
        }
    }
}