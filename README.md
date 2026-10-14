# NES Emulator

A Nintendo Entertainment System (NES) emulator written in Rust, using SDL2 for video, audio and input. It runs games on the most common cartridge boards, including *Super Mario Bros.*, *Tetris* and *Final Fantasy*, with full sound and battery saves.

This project was built by following [Writing NES Emulator in Rust](https://bugzmanov.github.io/nes_ebook/) by bugzmanov, then extended with an APU, more accurate timing and NES 2.0 ROM support.

## Features

- **CPU (6502)**: all official opcodes plus the common unofficial ones. Cycle counts match the `nestest` reference log for every instruction, including page-crossing and branch penalties.
- **PPU**: background, 8×8 and 8×16 sprites, CHR-ROM and CHR-RAM, scrolling with mid-frame splits (e.g. the SMB status bar), sprite-0 hit, the shared `$2005`/`$2006` write latch, and the odd-frame dot skip.
- **APU**: both pulse channels, triangle, noise and DMC, the frame sequencer (envelopes, sweeps, length counters), the nonlinear mixer and the NES's output filters.
- **Timing**: runs at the NTSC rate of 60.0988 frames per second. The CPU stalls for OAM DMA (513–514 cycles) and DMC sample fetches like the real hardware.
- **Audio sync**: dynamic rate control keeps about 25 ms of audio buffered. This avoids crackles from the sound card's clock drifting against the system clock.
- **ROM loading**: iNES, NES 2.0 and archaic iNES headers. This includes 12-bit mapper numbers, submappers, RAM sizes, CPU/PPU timing and console type.
- **Mappers**: 0 (NROM), 1 (MMC1), 2 (UxROM), 3 (CNROM) and 7 (AxROM). Together these cover a large share of the NES library.
- **Battery saves**: games with battery-backed RAM (like *Final Fantasy* and *Zelda*) save to a `.sav` file next to the ROM, every few seconds while the RAM changes and when you quit.
- **Input**: keyboard as controller 1. The bus emulates both controller ports.

## Limitations

- **No MMC3 (mapper 4) yet.** It needs a scanline IRQ and per-scanline rendering, because games like *Super Mario Bros. 3* switch CHR banks mid-frame. The emulator exits with a message for unsupported mappers.
- **Whole-frame rendering.** The frame is drawn from the PPU's state at the end of the frame. Mid-frame scroll splits work, but mid-frame CHR bank or palette changes don't show.
- **MMC1 variants.** SOROM/SXROM boards with 16–32 KiB of banked PRG-RAM are treated as having one 8 KiB bank.
- **NTSC only.** PAL and Dendy timing in a NES 2.0 header is read but not emulated.
- **Not implemented:** keyboard mapping for controller 2, and the APU's frame and DMC interrupts. The CPU's IRQ line is wired up, but nothing raises it yet.
- **Instruction-stepped CPU.** The PPU and APU catch up after each instruction rather than on every memory access, so register reads and writes can land a few cycles early.

## Building and running

You need:

- **Rust 1.85 or newer** (the crate uses the 2024 edition).
- **SDL2 development libraries:**
  - Linux: `sudo apt install libsdl2-dev`
  - macOS: `brew install sdl2`
  - Windows: see the [rust-sdl2 setup guide](https://github.com/Rust-SDL2/rust-sdl2#windows-msvc). It explains where to put `SDL2.lib` and `SDL2.dll`.

Then from the project folder:

```sh
cargo run --release -- ff.nes          # roms/ff.nes
cargo run --release -- path/to/game.nes
```

Pass the ROM to play as an argument. A bare file name is looked up in the current folder, then in `roms/`. With no argument it loads `smb.nes`.

Build with `--release`. An unoptimised debug build may not keep up with real time, and then the audio crackles.

## Controls

| NES button | Key        |
|------------|------------|
| D-pad      | Arrow keys |
| A          | `A`        |
| B          | `S`        |
| Start      | `Enter`    |
| Select     | `Space`    |
| Quit       | `Esc`      |

## Tests

```sh
cargo test
```

The tests cover CPU instructions, PPU registers and VRAM mirroring, APU channels and sample generation, and iNES / NES 2.0 header parsing, and bank switching for each mapper.

## Project layout

| File | Purpose |
|------|---------|
| `src/main.rs` | SDL window, input, and the main loop |
| `src/cpu.rs`, `src/opcodes.rs` | 6502 CPU and opcode table |
| `src/bus.rs` | Memory map; steps the PPU and APU once per CPU cycle and handles DMA stalls |
| `src/ppu/` | Picture processing unit and its registers |
| `src/render/` | Turns PPU state into a 256×240 frame |
| `src/apu.rs` | Audio processing unit: channels, frame sequencer, mixer and resampling |
| `src/audio.rs` | SDL audio output with dynamic rate control |
| `src/frame_limiter.rs` | Keeps the emulator at 60.0988 fps when vsync isn't used |
| `src/cartridge.rs` | iNES / NES 2.0 ROM loading |
| `src/mapper/` | Cartridge boards: PRG/CHR banking, mirroring and save RAM, one file per mapper |
| `src/joypad.rs` | Controller |
| `src/trace.rs` | CPU trace output in `nestest` log format |

## Acknowledgements and references

- [Writing NES Emulator in Rust](https://bugzmanov.github.io/nes_ebook/) by bugzmanov is the tutorial this emulator is built on. It covers the CPU, bus, cartridge, PPU, rendering and joypad.
- [TetaNES](https://github.com/lukexor/tetanes/tree/main) by lukexor is a full-featured NES emulator in Rust, used as a reference.
- [NESdev Wiki](https://www.nesdev.org/wiki/Nesdev_Wiki) is the main hardware reference for CPU, PPU and APU behaviour and timing, and for the iNES and NES 2.0 file formats.