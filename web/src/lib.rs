use nes_emulator::joypad::JoypadButton;
use nes_emulator::nes::{FRAME_HEIGHT, FRAME_WIDTH, Nes, Options};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const TARGET_LATENCY: f64 = 0.05;

const MAX_RATE_LATENCY: f64 = 0.005;
const KP: f64 = 0.02;
const KI: f64 = 0.00005;

struct RateControl {
    target: f64,
    smoothed_fill: f64,
    trim: f64
}

impl RateControl {
    fn new(sample_rate: u32) -> Self {
        let target = TARGET_LATENCY * sample_rate as f64;
        RateControl { target, smoothed_fill: target, trim: 0.0 }
    }

    fn adjust(&mut self, fill: f64) -> f64 {
        self.smoothed_fill += 0.05 * (fill - self.smoothed_fill);
        let error = (self.target - self.smoothed_fill) / self.target;
        self.trim = (self.trim + KI * error).clamp(-MAX_RATE_LATENCY, MAX_RATE_LATENCY);
        1.0 + (self.trim + KP * error).clamp(-MAX_RATE_LATENCY, MAX_RATE_LATENCY)
    }
}

struct Machine {
    nes: Nes,
    buttons: Rc<Cell<[u8; 2]>>,
    audio: Rc<RefCell<Vec<f32>>>,
    audio_fill: Rc<Cell<f64>>
}

#[derive(Default)]
struct Page {
    machine: Option<Machine>,
    rom: Vec<u8>,
    frame: Vec<u8>,
    audio: Vec<f32>,
    error: String
}

thread_local! {
    static PAGE: RefCell<Page> = RefCell::new(Page::default());
}

fn with_page<T>(f: impl FnOnce(&mut Page) -> T) -> T {
    PAGE.with(|page| f(&mut page.borrow_mut()))
}

#[unsafe(no_mangle)]
pub extern "C" fn rom_buffer(len: usize) -> *mut u8 {
    with_page(|page| {
        page.rom = vec![0; len];
        page.rom.as_mut_ptr()
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn load(sample_rate: u32) -> u32 {
    with_page(|page| {
        page.machine = None;
        match start(&page.rom, sample_rate) {
            Ok(machine) => {
                page.machine = Some(machine);
                page.frame = vec![0; FRAME_WIDTH * FRAME_HEIGHT * 4];
                page.error.clear();
                0
            }
            Err(error) => {
                page.error = error;
                1
            }
        }
    })
}

fn start(rom: &[u8], sample_rate: u32) -> Result<Machine, String> {
    let mut nes = Nes::new(rom, "game.nes", Options::default())?;

    let buttons = Rc::new(Cell::new([0u8; 2]));
    let hook_buttons = buttons.clone();
    nes.set_input(move |pads| {
        for (pad, bits) in pads.iter_mut().zip(hook_buttons.get()) {
            let pressed = JoypadButton::from_bits_truncate(bits);
            for button in [
                JoypadButton::BUTTON_A,
                JoypadButton::BUTTON_B,
                JoypadButton::SELECT,
                JoypadButton::START,
                JoypadButton::UP,
                JoypadButton::DOWN,
                JoypadButton::LEFT,
                JoypadButton::RIGHT,
            ] {
                pad.set_button_pressed_status(button, pressed.contains(button));
            }
        }
    });

    let audio = Rc::new(RefCell::new(Vec::new()));
    let audio_fill = Rc::new(Cell::new(0.0));
    let (out, fill) = (audio.clone(), audio_fill.clone());
    let mut rate = RateControl::new(sample_rate);
    nes.set_audio_output(sample_rate, move |samples| {
        out.borrow_mut().extend_from_slice(samples);
        rate.adjust(fill.get())
    });

    Ok(Machine { nes , buttons, audio, audio_fill })
}

#[unsafe(no_mangle)]
pub extern "C" fn error_ptr() -> *const u8 {
    with_page(|page| page.error.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn error_len() -> usize {
    with_page(|page| page.error.len())
}

#[unsafe(no_mangle)]
pub extern "C" fn set_buttons(player: u32, bits: u32) {
    with_page(|page| {
        if let Some(machine) = &page.machine {
            let mut buttons = machine.buttons.get();
            if let Some(slot) = buttons.get_mut(player as usize) {
                *slot = bits as u8;
            }
            machine.buttons.set(buttons);
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn set_audio_fill(samples: u32) {
    with_page(|page| {
        if let Some(machine) = &page.machine {
            machine.audio_fill.set(samples as f64);
        } 
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn run_frame() {
    with_page(|page| {
        let Some(machine) = page.machine.as_mut() else { return };
        let rgb = machine.nes.run_frame();
        for (rgba, rgb) in page.frame.chunks_exact_mut(4).zip(rgb.chunks_exact(3)) {
            rgba[..3].copy_from_slice(rgb);
            rgba[3] = 255;
        }
        page.audio.clear();
        page.audio.append(&mut machine.audio.borrow_mut());
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn frame_ptr() -> *const u8 {
    with_page(|page| page.frame.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn audio_ptr() -> *const f32 {
    with_page(|page| page.audio.as_ptr())
}

#[unsafe(no_mangle)]
pub extern "C" fn audio_len() -> usize {
    with_page(|page| page.audio.len())
}

#[unsafe(no_mangle)]
pub extern "C" fn reset() {
    with_page(|page| {
        if let Some(machine) = page.machine.as_mut() {
            machine.nes.reset();
        }
    })
}

#[unsafe(no_mangle)]
pub extern "C" fn frames_per_second() -> f64 {
    with_page(|page| page.machine.as_ref().map_or(60.0988, |m| m.nes.frames_per_second()))
}