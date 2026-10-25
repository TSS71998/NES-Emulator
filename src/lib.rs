//! The NES emulator as a library. The standalone program (`main.rs`) and
//! the launcher are both built on it; `nes::Nes` is the simplest way in.

#[macro_use]
extern crate bitflags;

pub mod apu;
pub mod audio;
pub mod bus;
pub mod cartridge;
pub mod cpu;
pub mod frame_limiter;
pub mod input;
pub mod joypad;
pub mod mapper;
pub mod nes;
pub mod opcodes;
pub mod ppu;
pub mod region;
pub mod render;
pub mod state;
pub mod trace;