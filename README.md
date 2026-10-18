# NES Emulator

A Nintendo Entertainment System (NES) emulator written in Rust, using SDL2 for video, audio and input. It runs games on the most common cartridge boards, including *Super Mario Bros.*, *Tetris* and *Final Fantasy*, with full sound and battery saves.

This project was built by following [Writing NES Emulator in Rust](https://bugzmanov.github.io/nes_ebook/) by bugzmanov, then extended with an APU, more accurate timing and NES 2.0 ROM support.

## Features

- **CPU (6502)**: all official and unofficial opcodes (including the unstable `SHX`/`SHY`/`SHA`/`TAS` stores), passing all of blargg's `instr_test-v5`, `nes_instr_test` and `instr_timing` tests. Cycle counts match the `nestest` reference log, including page-crossing and branch penalties. Every memory access takes its own cycle, and the PPU and APU run up to that point first, so a register read or write lands on the exact PPU dot it would on hardware. NMI and IRQ are polled on an instruction's second-to-last cycle, like the real 6502 (so an IRQ waiting on `CLI` runs after the next instruction), including the edge cases: an NMI arriving during BRK or an IRQ takes over its vector, a handler's first instruction always runs before another interrupt, and a taken branch that doesn't cross a page delays a new IRQ by one instruction (blargg's `cpu_interrupts_v2` passes 5 of 5). The reset button works as on the console: A, X, Y and RAM are kept, S drops by 3, the APU is silenced and its frame counter restarted, and PPUCTRL/PPUMASK are cleared (blargg's `cpu_reset` and `apu_reset` pass). The 6502's dummy accesses happen on the bus too: indexed reads that cross a page first read the wrong address, indexed stores always read before writing, read-modify-write instructions write the old value back before the new one, one-byte instructions read the next byte, indexed zero-page modes read the unindexed address, stack pulls and `JSR` read the stack, `RTS` reads its return address, and taken branches read the next opcode. Games that rely on these side effects on `$2002`, `$2007`, `$4015` or `$4016` behave as on hardware (blargg's `cpu_dummy_reads`, `cpu_dummy_writes` and `instr_misc` pass).
- **PPU**: dot-accurate. Each of the 341 dots per line does what the real PPU does on it: background tile fetches every 8 dots into shift registers, sprite evaluation and pattern fetches for the next line on dots 257–320, and one pixel out per dot. Scrolling uses the real internal registers (loopy `v`, `t`, `x` and `w`), so mid-line changes to scroll, CHR banks, palettes or PPUMASK show up on the dot they happen. Also: 8×8 and 8×16 sprites, 8 sprites per line with the overflow flag (including its hardware bug), sprite priority, left-column clipping, sprite-0 hit on the exact pixel, greyscale, and vblank/NMI timing down to the dot (including the `$2002` read race and the odd-frame dot skip). The PPU's own open bus is emulated too: an I/O latch that every register write fills, which write-only registers and undefined bits read back, and whose bits fade after about 600 ms. Passes all 10 of blargg's `ppu_vbl_nmi` tests, all 11 `sprite_hit_tests`, `ppu_open_bus`, `ppu_read_buffer` and `oam_stress`.
- **Mapper IRQs**: cartridges see every address the PPU puts on its bus, with the dot it happened on, and can raise the CPU's IRQ line. MMC3 counts rising edges of address line A12 with the chip's filter for short dips, exactly as the hardware does.
- **APU**: both pulse channels, triangle, noise and DMC, the frame counter (envelopes, sweeps, length counters) with its frame interrupt, the DMC interrupt, the nonlinear mixer and the NES's output filters. Timing is to the CPU cycle: `$4017` writes land 3 or 4 cycles later, length counter writes interact correctly with clocks on the same cycle, and the power-on state matches hardware. Passes all 8 of blargg's `apu_test` and all 11 of his 2005 APU tests.
- **Regions**: NTSC, PAL and Dendy, with clocks derived exactly from each console's master crystal. DMA works as on hardware: OAM DMA and DMC sample fetches halt the CPU on its next read cycle (repeating that read, side effects and all), then take turns on alternating get/put cycles, so OAM DMA takes 513 or 514 cycles and a DMC fetch 1–4. Passes blargg's `sprdma_and_dmc_dma` and `dmc_dma_during_read4` tests.

  | | NTSC | PAL | Dendy |
  |---|---|---|---|
  | CPU clock | 1.789773 MHz | 1.662607 MHz | 1.773448 MHz |
  | PPU dots per CPU cycle | 3 | 3.2 | 3 |
  | Scanlines per frame (vblank from line) | 262 (241) | 312 (241) | 312 (291) |
  | Odd-frame dot skip | yes | no | no |
  | APU noise/DMC/frame sequencer tables | NTSC | PAL | NTSC |
  | Frame rate | 60.0988 | 50.0070 | 50.0070 |
- **Audio sync**: dynamic rate control keeps about 25 ms of audio buffered. This avoids crackles from the sound card's clock drifting against the system clock.
- **ROM loading**: iNES, NES 2.0 and archaic iNES headers. This includes 12-bit mapper numbers, submappers, RAM sizes, CPU/PPU timing and console type.
- **Mappers**: 0 (NROM), 1 (MMC1), 2 (UxROM), 3 (CNROM), 4 (MMC3) and 7 (AxROM). Together these cover most of the NES library. MMC1 ignores a write on the cycle right after another, as the real chip does, so read-modify-write instructions on its registers work. MMC3 passes blargg's `mmc3_test_2` tests 1–5, including `4-scanline_timing` (IRQ timing to the PPU dot), and 6 for boards marked as MMC3A in a NES 2.0 header.
- **Battery saves**: games with battery-backed RAM (like *Final Fantasy* and *Zelda*) save to a `.sav` file next to the ROM, every few seconds while the RAM changes and when you quit.
- **Input**: keyboard as controller 1. The bus emulates both controller ports.
- **Display**: the 8-pixel border a TV hides is cropped on every side (`OVERSCAN_LINES` and `OVERSCAN_COLUMNS` in `main.rs`), showing 240×224. Without the side crop, games that clip the left column (such as *Super Mario Bros. 3*) show an 8-pixel strip of the background colour at the left, as real hardware does.

## Limitations

- **Unsupported mappers.** The emulator exits with a message for mappers other than 0, 1, 2, 3, 4 and 7.
- **Sprite evaluation timing.** The 8 sprites for the next line are found all at once on dot 257, not one by one over dots 65–256. So the overflow flag is set a little late (blargg's `sprite_overflow_tests` pass 4 of 5; `3.Timing` fails), and OAM writes during rendering don't corrupt OAM as they would on hardware.
- **MMC1 variants.** SOROM/SXROM boards with 16–32 KiB of banked PRG-RAM are treated as having one 8 KiB bank.
- **PAL colours.** PAL's swapped red/green emphasis bits and its slightly different palette aren't modelled (colour emphasis isn't emulated in any region).
- **Not implemented:** keyboard mapping for controller 2.
- **PPU after reset.** On a front-loading NES the PPU ignores writes to some registers until the end of the first frame after reset; that isn't modelled (the Famicom doesn't do it either).

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
cargo run --release -- --region pal "Game (Europe).nes"
```

Pass the ROM to play as an argument. A bare file name is looked up in the current folder, then in `roms/`. With no argument it loads `Alter_Ego.nes`.

Build with `--release`. An unoptimised debug build may not keep up with real time, and then the audio crackles.

## Controls

| NES button | Key        |
|------------|------------|
| D-pad      | Arrow keys |
| A          | `A`        |
| B          | `S`        |
| Start      | `Enter`    |
| Select     | `Space`    |
| Reset      | `Ctrl+R`   |
| Quit       | `Esc`      |

## Tests

```sh
cargo test
```

The tests cover CPU instructions, PPU registers, VRAM mirroring, scrolling, sprites and frame timing, APU channels and sample generation, and iNES / NES 2.0 header parsing, and bank switching for each mapper.

## Project layout

| File | Purpose |
|------|---------|
| `src/main.rs` | SDL window, input, and the main loop |
| `src/cpu.rs`, `src/opcodes.rs` | 6502 CPU and opcode table |
| `src/bus.rs` | Memory map; runs the PPU and APU around every CPU memory access, samples interrupts and handles DMA stalls |
| `src/ppu/` | Picture processing unit: registers and frame timing (`mod.rs`), the per-dot fetch and pixel pipeline (`pipeline.rs`) |
| `src/render/` | The 256×240 frame buffer and the NES palette |
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