<p align="center">
  <img src="docs/logo.svg" alt="NES Emulator" width="560">
</p>

<p align="center">
  <b>A cycle-accurate Nintendo Entertainment System emulator, written in Rust.</b>
</p>

<p align="center">
  <img src="https://img.shields.io/badge/made%20with-Rust-b7410e" alt="Made with Rust">
  <img src="https://img.shields.io/badge/runs%20on-Windows%20%7C%20macOS%20%7C%20Linux%20%7C%20Web-3a4a8a" alt="Runs on Windows, macOS, Linux and the web">
  <img src="https://img.shields.io/badge/mappers-32-555555" alt="32 mappers">
  <img src="https://img.shields.io/badge/AccuracyCoin-146%2F146-2e8b57" alt="AccuracyCoin: 146 of 146 tests pass">
</p>

<p align="center">
  <a href="https://tss71998.github.io/NES-Emulator/"><b>▶ Play Moonlight Sorrow in your browser</b></a>
</p>

## About

**NES-Emulator** plays NES games on your computer, or in a web browser. It
runs the most common cartridge boards, including *Super Mario Bros.*,
*Tetris*, *The Legend of Zelda* and *Final Fantasy*, with full sound and
battery saves.

It started from bugzmanov's tutorial
[Writing NES Emulator in Rust](https://bugzmanov.github.io/nes_ebook/), and
grew an APU, timing accurate to the cycle and NES 2.0 ROM support. It's also
what I test my NES game, [Moonlight Sorrow](https://github.com/TSS71998/moonlight),
on.

## Play it in your browser

The emulator compiles to WebAssembly, and this repo's
[GitHub Pages site](https://tss71998.github.io/NES-Emulator/) runs
*Moonlight Sorrow* in it. Press play, then use the arrow keys, `A` to jump
and `S` to attack (or a gamepad). You can also drop any other `.nes` file on
the screen to play it.

## Features

- **Plays most of the NES library**: 32 mappers, from NROM, MMC1 and MMC3
  (*Super Mario Bros. 1–3*, *Zelda*, *Mega Man*) to MMC5 (*Castlevania III*),
  MMC2 (*Punch-Out!!*), Konami's VRC chips and Sunsoft's FME-7, with the
  Famicom cartridges' extra sound channels.
- **Accurate**: the CPU is accurate to the cycle and the PPU and APU to the
  dot, passing blargg's hardware test ROMs and all 146 of AccuracyCoin's
  tests, so games that rely on precise timing work.
- **Full sound**: all five sound channels through the NES's own mixer and
  output filters, kept smooth with dynamic rate control.
- **NTSC, PAL and Dendy**, picked from the ROM, plus the front-loading NES,
  top-loading NES and Famicom.
- **Battery saves** for games like *Final Fantasy* and *Zelda*.
- **Two players**, on the keyboard or any gamepad (Xbox, PlayStation,
  Switch and most others).
- **A game library**: the launcher lists your games with their box art and
  adds save states, fast forward and per-game settings.
- **Runs in the browser**, compiled to WebAssembly.

## Controls

| NES button | Player 1 | Player 2 | Gamepad |
|---|---|---|---|
| D-pad | Arrow keys | `I` `J` `K` `L` | D-pad or left stick |
| A | `A` | `G` | Right or top face button (Xbox B/Y) |
| B | `S` | `F` | Bottom or left face button (Xbox A/X) |
| Start | `Enter` | Right `Shift` | Start |
| Select | `Space` | Right `Ctrl` | Back / Select |

| Console | Key |
|---|---|
| Reset | `Ctrl+R` |
| Famicom microphone (with `--console famicom`) | `M` (hold) |
| Quit (desktop) | `Esc` |

The face buttons go by position, as on an NES pad, where B is to the left of
A. The first gamepad plugged in is player 1 and the second player 2. The
browser version uses the same keys.

## Build and run

You need:

1. **Rust 1.85 or newer**.
2. **The SDL2 development libraries**:
   - Linux: `sudo apt install libsdl2-dev`
   - macOS: `brew install sdl2`
   - Windows: see the [rust-sdl2 setup guide](https://github.com/Rust-SDL2/rust-sdl2#windows-msvc)
     for where to put `SDL2.lib` and `SDL2.dll`.

Then, from this folder:

```sh
cargo run --release -- path/to/game.nes
cargo run --release -- --region pal "Game (Europe).nes"
cargo run --release -- --console famicom game.nes
```

A bare file name is looked up in the current folder, then in `roms/`. With
no ROM it loads `Alter_Ego.nes`. A release build takes about 4 ms per frame;
a plain `cargo run` keeps up too (at about 5 ms).

### The launcher

```sh
cargo run -p nes_launcher --release
```

A game library in the style of DuckStation's: add your ROM folders, browse
them as a list or by box art, and play. It adds save states, fast forward,
favorites, remappable controls, per-game settings and play time tracking.
The full guide is in [docs/LAUNCHER.md](docs/LAUNCHER.md).

### The browser version

The web page is in [`web/`](web): the emulator built for WebAssembly
(`web/src/lib.rs`, no extra tools needed) and a small page that draws the
picture, plays the sound and reads the keyboard and gamepads. To build and
try it yourself:

```sh
rustup target add wasm32-unknown-unknown   # once
sh web/build.sh                            # uses roms/moonlight.nes
python3 -m http.server -d web/dist         # then open http://localhost:8000
```

Each push to `main` rebuilds the page and publishes it on GitHub Pages
(see [`.github/workflows/pages.yml`](.github/workflows/pages.yml)).

## Tests

```sh
cargo test
```

The tests cover the CPU's instructions, the PPU's registers, scrolling,
sprites and timing, the APU's channels, ROM header parsing, bank switching
for each mapper, and save states.

## Accuracy

The emulator passes, in full, blargg's CPU, PPU, APU, DMA and MMC3 test
ROMs, and all 146 tests of [AccuracyCoin](https://github.com/100thCoin/AccuracyCoin),
a much stricter newer collection. What's emulated, down to the dot, and what
isn't yet (a few mappers, the Famicom Disk System, the Zapper) is in
[docs/TECHNICAL.md](docs/TECHNICAL.md).

## Made with

- **Code**: [Rust](https://www.rust-lang.org)
- **Desktop video, sound and input**: [SDL2](https://www.libsdl.org) through
  [rust-sdl2](https://github.com/Rust-SDL2/rust-sdl2)
- **Launcher**: [egui](https://github.com/emilk/egui), with game names from
  the [libretro database](https://github.com/libretro/libretro-database) and
  box art from [libretro-thumbnails](https://github.com/libretro-thumbnails/Nintendo_-_Nintendo_Entertainment_System)
- **Browser version**: WebAssembly, with the Web Audio and Gamepad APIs

## References

- [Writing NES Emulator in Rust](https://bugzmanov.github.io/nes_ebook/) by
  bugzmanov, the tutorial this emulator is built on.
- [TetaNES](https://github.com/lukexor/tetanes/tree/main) by lukexor, a
  full-featured NES emulator in Rust, used as a reference.
- [NESdev Wiki](https://www.nesdev.org/wiki/Nesdev_Wiki), the main hardware
  reference for the CPU, PPU, APU and ROM formats.
- [TriCNES](https://github.com/100thCoin/TriCNES), the emulator by
  AccuracyCoin's author, whose PPU model the PPU follows.

## For developers

How the emulator works (the CPU, PPU, APU, DMA, each mapper, the test ROMs
it passes, its limitations and the project layout) is in
[docs/TECHNICAL.md](docs/TECHNICAL.md).

---

<p align="center">Created by <a href="https://github.com/TSS71998">TSS71998</a></p>
