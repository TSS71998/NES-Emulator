//! The whole console behind one type, for frontends that drive the emulator
//! themselves (like the launcher): load a game, run it a frame at a time,
//! and take the picture and sound. The standalone program (`main.rs`) still
//! runs the CPU directly and does its work in the frame callback.
//!
//! `run_frame` steps the CPU until the bus reports a finished frame, so the
//! CPU, PPU and APU run in exactly the order they do in `main.rs`, and the
//! controllers are read at the same moment (in the frame callback).

use crate::bus::Bus;
use crate::cartridge::Rom;
use crate::cpu::CPU;
use crate::joypad::Joypad;
use crate::mapper::{self, SharedMapper};
use crate::ppu::NesPPU;
use crate::region::{Console, Region};
use crate::state::{StateReader, StateWriter};
use std::cell::{Cell, RefCell};
use std::rc::Rc;

const STATE_MAGIC: &[u8; 8] = b"NESSTATE";
const STATE_VERSION: u16 = 1;

pub const FRAME_WIDTH: usize = 256;
pub const FRAME_HEIGHT: usize = 240;
const FRAME_BYTES: usize = FRAME_WIDTH * FRAME_HEIGHT * 3;

pub type InputHook = Box<dyn FnMut(&mut [Joypad; 2])>;

pub struct Options {
    pub region: Option<Region>,
    pub console: Option<Console>,
    pub oam_row_corruption: bool,
    pub battery_ram: Option<Vec<u8>>
}

impl Default for Options {
    fn default() -> Self {
        Options {
            region: None,
            console: None,
            oam_row_corruption: true,
            battery_ram: None
        }
    }
}

struct FrameState {
    done: Cell<bool>,
    picture: RefCell<Vec<u8>>,
    input: RefCell<Option<InputHook>>
}

pub struct Nes {
    cpu: CPU<'static>,
    cartridge: SharedMapper,
    state: Rc<FrameState>,
    picture: Vec<u8>,
    region: Region,
    region_source: &'static str,
    console: Console,
    console_source: &'static str,
    frames: u64,
    rom_checksum: u64
}

impl Nes {
    pub fn new(rom_file: &[u8], file_name: &str, options: Options) -> Result<Nes, String> {
        let rom = Rom::new(rom_file)?;
        let rom_checksum = checksum(rom_file);
        let (region, region_source) = match options.region {
            Some(region) => (region, "chosen"),
            None => Region::detect(&rom, file_name)
        };
        let (console, console_source) = match options.console {
            Some(console) => (console, "chosen"),
            None => Console::detect(file_name)
        };
        let cartridge = mapper::from_rom(rom)?;
        if let Some(data) = &options.battery_ram {
            cartridge.borrow_mut().load_battery_ram(data);
        }

        let state = Rc::new(FrameState {
            done: Cell::new(false),
            picture: RefCell::new(vec![0; FRAME_BYTES]),
            input: RefCell::new(None)
        });
        let callback_state = state.clone();
        let mut bus = Bus::new(
            cartridge.clone(),
            move |ppu: &NesPPU, joypads: &mut [Joypad; 2]| {
                callback_state
                    .picture
                    .borrow_mut()
                    .copy_from_slice(&ppu.frame.data);
                if let Some(input) = callback_state.input.borrow_mut().as_mut() {
                    input(joypads);
                }
                callback_state.done.set(true);
            }
        );
        bus.set_region(region);
        bus.set_console(console);
        bus.set_oam_row_corruption(options.oam_row_corruption);

        let mut cpu = CPU::new(bus);
        cpu.reset();
        Ok(Nes {
            cpu,
            cartridge,
            state,
            picture: vec![0; FRAME_BYTES],
            region,
            region_source,
            console,
            console_source,
            frames: 0,
            rom_checksum
        })
    }

    pub fn set_input(&mut self, input: impl FnMut(&mut [Joypad; 2]) + 'static) {
        *self.state.input.borrow_mut() = Some(Box::new(input));
    }

    pub fn set_audio_output(
        &mut self,
        sample_rate: u32,
        output: impl FnMut(&[f32]) -> f64 + 'static,
    ) {
        self.cpu.bus.set_audio_callback(sample_rate, output);
    }

    pub fn run_frame(&mut self) -> &[u8] {
        self.state.done.set(false);
        while !self.state.done.get() {
            self.cpu.step();
        }
        self.frames += 1;
        self.picture.copy_from_slice(&self.state.picture.borrow());
        &self.picture
    }

    pub fn picture(&self) -> &[u8] {
        &self.picture
    }

    pub fn reset(&mut self) {
        self.cpu.request_reset();
    }

    pub fn battery_ram(&self) -> Option<Vec<u8>> {
        self.cartridge.borrow().battery_ram().map(|r| r.to_vec())
    }

    pub fn region(&self) -> Region {
        self.region
    }

    pub fn region_source(&self) -> &'static str {
        self.region_source
    }

    pub fn console(&self) -> Console {
        self.console
    }

    pub fn console_source(&self) -> &'static str {
        self.console_source
    }

    pub fn frames_per_second(&self) -> f64 {
        self.region.frames_per_second()
    }

    pub fn frames(&self) -> u64 {
        self.frames
    }

    pub fn save_state(&self) -> Vec<u8> {
        let mut w = StateWriter::new();
        for &b in STATE_MAGIC {
            w.u8(b);
        }
        w.u16(STATE_VERSION);
        w.u64(self.rom_checksum);
        w.u8(region_code(self.region));
        w.u8(console_code(self.console));
        w.u64(self.frames);
        w.bytes(&self.picture);
        self.cpu.save_state(&mut w);
        w.into_bytes()
    }

    pub fn load_state(&mut self, data: &[u8]) -> Result<(), String> {
        let backup = self.save_state();
        let result = self.read_state(data);
        if result.is_err() {
            // put back what a damaged state may have half overwritten
            let _ = self.read_state(&backup);
        }
        result
    }

    fn read_state(&mut self, data: &[u8]) -> Result<(), String> {
        let mut r = StateReader::new(data);
        let mut magic = [0; 8];
        for b in &mut magic {
            *b = r.u8();
        }
        if &magic != STATE_MAGIC {
            return Err("this isn't a save state".into());
        }
        if r.u16() != STATE_VERSION {
            return Err("this save state is from a different version of the emulator".into());
        }
        if r.u64() != self.rom_checksum {
            return Err("this save state is for a different game".into());
        }
        if r.u8() != region_code(self.region) {
            return Err(format!(
                "this save state wasn't made in {} mode, as the game is running now",
                self.region.name()
            ));
        }
        if r.u8() != console_code(self.console) {
            return Err(format!(
                "this save state wasn't made on the {}, as the game is running now",
                self.console.name()
            ));
        }
        let frames = r.u64();
        let mut picture = vec![0; FRAME_BYTES];
        r.bytes_into(&mut picture);
        self.cpu.load_state(&mut r);
        r.finish()?;
        self.frames = frames;
        self.state.picture.borrow_mut().copy_from_slice(&picture);
        self.picture = picture;
        Ok(())
    }
}

/// FNV-1a: a quick fingerprint of the ROM file.
fn checksum(data: &[u8]) -> u64 {
    data.iter().fold(0xcbf2_9ce4_8422_2325, |hash, &b| {
        (hash ^ b as u64).wrapping_mul(0x0100_0000_01b3)
    })
}

fn region_code(region: Region) -> u8 {
    match region {
        Region::Ntsc => 0,
        Region::Pal => 1,
        Region::Dendy => 2,
    }
}

fn console_code(console: Console) -> u8 {
    match console {
        Console::Nes => 0,
        Console::TopLoader => 1,
        Console::Famicom => 2,
    }
}
#[cfg(test)]
mod test {
    use super::*;
    use crate::joypad::JoypadButton;

    /// An NROM test program: copies controller 1 ($4016) into $0000 every
    /// frame (in NMI), so the test can see when input arrives.
    fn test_rom() -> Vec<u8> {
        let mut prg = vec![0xEA; 0x4000];
        let reset = [
            0xA9, 0x80, // LDA #$80
            0x8D, 0x00, 0x20, // STA $2000 (NMI on)
            0x4C, 0x05, 0xC0, // JMP $C005 (loop)
        ];
        let nmi = [
            0xA9, 0x01, 0x8D, 0x16, 0x40, // LDA #1, STA $4016 (strobe)
            0xA9, 0x00, 0x8D, 0x16, 0x40, // LDA #0, STA $4016
            0xAD, 0x16, 0x40, // LDA $4016 (button A)
            0x29, 0x01, // AND #1
            0x85, 0x00, // STA $00
            0x40, // RTI
        ];
        prg[..reset.len()].copy_from_slice(&reset);
        prg[0x100..0x100 + nmi.len()].copy_from_slice(&nmi);
        prg[0x3FFA..].copy_from_slice(&[0x00, 0xC1, 0x00, 0xC0, 0x00, 0xC1]);
        let mut file = vec![0x4E, 0x45, 0x53, 0x1A, 1, 1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0];
        file.extend(prg);
        file.extend(vec![0; 0x2000]);
        file
    }

    #[test]
    fn test_runs_a_frame_at_a_time() {
        let mut nes = Nes::new(&test_rom(), "test (Europe).nes", Options::default()).unwrap();
        assert_eq!(
            (nes.region(), nes.region_source()),
            (Region::Pal, "file name")
        );
        assert_eq!(nes.run_frame().len(), FRAME_WIDTH * FRAME_HEIGHT * 3);
        nes.run_frame();
        assert_eq!(nes.frames(), 2);
    }

    #[test]
    fn test_input_hook_reaches_the_game() {
        let mut nes = Nes::new(&test_rom(), "test.nes", Options::default()).unwrap();
        let pressed = Rc::new(Cell::new(false));
        let hook_pressed = pressed.clone();
        nes.set_input(move |pads| {
            pads[0].set_button_pressed_status(JoypadButton::BUTTON_A, hook_pressed.get());
        });
        for _ in 0..3 {
            nes.run_frame();
        }
        assert_eq!(nes.cpu.bus.peek(0x0000), 0);
        pressed.set(true);
        for _ in 0..2 {
            nes.run_frame();
        }
        assert_eq!(nes.cpu.bus.peek(0x0000), 1);
    }

    #[test]
    fn test_chosen_settings_win() {
        let options = Options {
            region: Some(Region::Dendy),
            console: Some(Console::Famicom),
            ..Options::default()
        };
        let nes = Nes::new(&test_rom(), "test (Europe).nes", options).unwrap();
        assert_eq!(
            (nes.region(), nes.region_source()),
            (Region::Dendy, "chosen")
        );
        assert_eq!(nes.console(), Console::Famicom);
    }

        #[test]
    fn test_save_state_carries_on_exactly() {
        let mut a = Nes::new(&test_rom(), "test.nes", Options::default()).unwrap();
        for _ in 0..3 {
            a.run_frame();
        }
        let state = a.save_state();
        let mut b = Nes::new(&test_rom(), "test.nes", Options::default()).unwrap();
        b.load_state(&state).unwrap();
        assert_eq!(b.frames(), 3);
        assert_eq!(b.save_state(), state, "saving again gives the same bytes");
        for _ in 0..3 {
            a.run_frame();
            b.run_frame();
        }
        assert_eq!(a.save_state(), b.save_state());
    }

    #[test]
    fn test_states_only_load_into_their_game_and_region() {
        let mut nes = Nes::new(&test_rom(), "test.nes", Options::default()).unwrap();
        nes.run_frame();
        let state = nes.save_state();

        let mut other_rom = test_rom();
        other_rom[0x100] ^= 0xFF;
        let mut other = Nes::new(&other_rom, "test.nes", Options::default()).unwrap();
        assert!(other.load_state(&state).unwrap_err().contains("different game"));

        let options = Options {
            region: Some(Region::Pal),
            ..Options::default()
        };
        let mut pal = Nes::new(&test_rom(), "test.nes", options).unwrap();
        assert!(pal.load_state(&state).unwrap_err().contains("PAL"));

        assert!(nes.load_state(b"hello").unwrap_err().contains("isn't a save state"));
    }

    #[test]
    fn test_damaged_state_leaves_the_console_as_it_was() {
        let mut nes = Nes::new(&test_rom(), "test.nes", Options::default()).unwrap();
        nes.run_frame();
        let before = nes.save_state();
        let mut damaged = before.clone();
        damaged.truncate(damaged.len() - 100);
        assert!(nes.load_state(&damaged).is_err());
        assert_eq!(nes.save_state(), before);
    }
}