use nes_emulator::audio::AudioOutput;
use nes_emulator::bus::Bus;
use nes_emulator::cartridge::Rom;
use nes_emulator::cpu::CPU;
use nes_emulator::frame_limiter::FrameLimiter;
use nes_emulator::input::Input;
use nes_emulator::joypad;
use nes_emulator::mapper::{self, SharedMapper};
use nes_emulator::ppu::NesPPU;
use nes_emulator::region::{Console, Region};

use sdl2::event::Event;
use sdl2::keyboard::{Keycode, Mod};
use sdl2::pixels::PixelFormatEnum;
use sdl2::rect::Rect;
use std::cell::Cell;
use std::rc::Rc;



const OVERSCAN_LINES: u32 = 8;
const OVERSCAN_COLUMNS: u32 = 8;
const VISIBLE_LINES: u32 = 240 - 2 * OVERSCAN_LINES;
const VISIBLE_COLUMNS: u32 = 256 - 2 * OVERSCAN_COLUMNS;
const SCALE: u32 = 3;

fn main() {
    let args = parse_args();
    let rom_path = find_rom(&args.rom);
    let bytes: Vec<u8> = std::fs::read(&rom_path)
        .unwrap_or_else(|e| exit_with_error(&format!("can't read {}: {}", rom_path.display(), e)));
    let rom = Rom::new(&bytes).unwrap_or_else(|e| exit_with_error(&e));

    let file_name = rom_path
        .file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_default();
    let (region, region_source) = match args.region {
        Some(region) => (region, "--region"),
        None => Region::detect(&rom, &file_name),
    };
    let (console, console_source) = match args.console {
        Some(console) => (console, "--console"),
        None => Console::detect(&file_name),
    };
    println!(
        "{}: {} ({}), {} ({}), {:.4} fps, CPU {:.6} MHz",
        file_name,
        region.name(),
        region_source,
        console.name(),
        console_source,
        region.frames_per_second(),
        region.cpu_clock_hz() / 1e6
    );

    let cartridge = mapper::from_rom(rom).unwrap_or_else(|e| exit_with_error(&e));

    let sdl_context = sdl2::init().unwrap();
    let video_subsystem = sdl_context.video().unwrap();
    let title = format!("NES Emulator - {} ({})", file_name, region.name());
    let window = video_subsystem
        .window(&title, VISIBLE_COLUMNS * SCALE, VISIBLE_LINES * SCALE)
        .position_centered()
        .build()
        .unwrap();

    // Vsync paces frames at a fixed delay after emulation, but only works as the
    // frame clock when the display runs at the console's rate (about 60 Hz for
    // NTSC, 50 Hz for PAL and Dendy); otherwise fall back to the frame limiter.
    let refresh_rate = window
        .display_index()
        .and_then(|i| video_subsystem.current_display_mode(i))
        .map(|mode| mode.refresh_rate)
        .unwrap_or(0);
    let use_vsync = (refresh_rate as f64 - region.frames_per_second()).abs() < 1.0;

    let mut canvas_builder = window.into_canvas();
    if use_vsync {
        canvas_builder = canvas_builder.present_vsync();
    }
    let mut canvas = canvas_builder.build().unwrap();
    let mut event_pump = sdl_context.event_pump().unwrap();

    let audio_subsystem = sdl_context.audio().unwrap();
    let mut audio_output = AudioOutput::new(&audio_subsystem);
    let sample_rate = audio_output.sample_rate();
    let mut frame_limiter = FrameLimiter::new(region.frames_per_second());

    let creator = canvas.texture_creator();
    let mut texture = creator
        .create_texture_target(PixelFormatEnum::RGB24, 256, 240)
        .unwrap();

    let save_path = rom_path.with_extension("sav");
    if let Ok(data) = std::fs::read(&save_path) {
        cartridge.borrow_mut().load_battery_ram(&data);
    }
    let mut saver = BatterySaver::new(cartridge.clone(), save_path);

    let controllers = sdl_context
        .game_controller()
        .map_err(|e| eprintln!("warning: gamepads unavailable: {}", e))
        .ok();
    let mut input = Input::new(controllers);

    let reset_requested = Rc::new(Cell::new(false));
    let reset_pressed = reset_requested.clone();

    // run the game cycle
    let mut bus = Bus::new(
        cartridge,
        move |ppu: &NesPPU, joypads: &mut [joypad::Joypad; 2]| {
            saver.on_frame();

            texture.update(None, &ppu.frame.data, 256 * 3).unwrap();

            let visible = Rect::new(
                OVERSCAN_COLUMNS as i32,
                OVERSCAN_LINES as i32,
                VISIBLE_COLUMNS,
                VISIBLE_LINES,
            );
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
                    Event::KeyDown {
                        keycode: Some(Keycode::R),
                        keymod,
                        repeat: false,
                        ..
                    } if keymod.intersects(Mod::LCTRLMOD | Mod::RCTRLMOD) => {
                        reset_pressed.set(true);
                    }
                    _ => input.handle_event(&event),
                }
            }
            input.apply(joypads);
        },
    );

    bus.set_region(region);
    bus.set_console(console);
    bus.set_oam_row_corruption(!args.no_oam_corruption);
    bus.set_audio_callback(sample_rate, move |samples: &[f32]| {
        audio_output.push(samples)
    });

    let mut cpu = CPU::new(bus);

    cpu.reset();
    cpu.run_with_callback(move |cpu| {
        if reset_requested.take() {
            cpu.request_reset();
        }
    });
}

struct Args {
    rom: String,
    region: Option<Region>,
    console: Option<Console>,
    no_oam_corruption: bool,
}

/// `nes_emulator [--region ntsc|pal|dendy] [--console nes|toploader|famicom]
/// [--no-oam-corruption] [rom]`
fn parse_args() -> Args {
    let mut rom = None;
    let mut region = None;
    let mut console = None;
    let mut no_oam_corruption = false;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        // "--name value" or "--name=value"
        let mut option = |name: &str| -> Option<String> {
            if arg == name {
                Some(args.next().unwrap_or_default())
            } else {
                arg.strip_prefix(name)?
                    .strip_prefix('=')
                    .map(str::to_string)
            }
        };
        if let Some(v) = option("--region") {
            region = Some(Region::parse(&v).unwrap_or_else(|| {
                exit_with_error(&format!("unknown region '{}' (use ntsc, pal or dendy)", v))
            }));
        } else if let Some(v) = option("--console") {
            console = Some(Console::parse(&v).unwrap_or_else(|| {
                exit_with_error(&format!(
                    "unknown console '{}' (use nes, toploader or famicom)",
                    v
                ))
            }));
        } else if arg == "--no-oam-corruption" {
            no_oam_corruption = true;
        } else {
            rom = Some(arg);
        }
    }
    Args {
        rom: rom.unwrap_or_else(|| "Alter_Ego.nes".to_string()),
        region,
        console,
        no_oam_corruption,
    }
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
    frames: u32,
}

impl BatterySaver {
    const SAVE_INTERVAL_FRAMES: u32 = 300;

    fn new(cartridge: SharedMapper, path: std::path::PathBuf) -> Self {
        let last_saved = cartridge
            .borrow()
            .battery_ram()
            .map(|r| r.to_vec())
            .unwrap_or_default();
        BatterySaver {
            cartridge,
            path,
            last_saved,
            frames: 0,
        }
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
        let Some(ram) = cartridge.battery_ram() else {
            return;
        };
        if ram == &self.last_saved[..] {
            return;
        }
        match std::fs::write(&self.path, ram) {
            Ok(()) => self.last_saved = ram.to_vec(),
            Err(e) => eprintln!("warning: couldn't write {}: {}", self.path.display(), e),
        }
    }
}
