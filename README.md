# NES Emulator

A Nintendo Entertainment System (NES) emulator written in Rust, using SDL2 for video, audio and input. It runs games on the most common cartridge boards, including *Super Mario Bros.*, *Tetris* and *Final Fantasy*, with full sound and battery saves.

This project was built by following [Writing NES Emulator in Rust](https://bugzmanov.github.io/nes_ebook/) by bugzmanov, then extended with an APU, more accurate timing and NES 2.0 ROM support.

## Features

- **Plays most of the NES library**: 32 mappers (see [Mappers](#mappers-and-roms)), from NROM, MMC1 and MMC3 (*Super Mario Bros. 1–3*, *Zelda*, *Final Fantasy*, *Mega Man*) to MMC5 (*Castlevania III*), MMC2 (*Punch-Out!!*), Konami's VRC chips and Sunsoft's FME-7, with the Famicom cartridges' extra sound channels.
- **Accurate**: a cycle-accurate CPU and a dot-accurate PPU and APU that pass blargg's hardware test ROMs and all 146 of AccuracyCoin's tests (see [Technical](#technical)), so games that rely on precise timing work.
- **Full sound**: all five APU channels through the NES's own mixer and output filters, with dynamic rate control to keep audio smooth.
- **NTSC, PAL and Dendy**: the region is picked from the ROM header or file name, or set with `--region`.
- **Battery saves**: games with battery-backed RAM (like *Final Fantasy* and *Zelda*) save to a `.sav` file next to the ROM, every few seconds while the RAM changes and when you quit.
- **Two players, keyboard or gamepad**: each player has their own keys, and any gamepad SDL supports (Xbox, PlayStation, Switch and most others) works too, plugged in before or while a game runs.
- **Reset button**: `Ctrl+R` resets the console. It behaves like a front-loading NES by default, or like a top-loading NES or a Famicom with `--console` (Japanese ROMs pick Famicom automatically).
- **ROM formats**: iNES, NES 2.0 and archaic iNES headers.
- **TV-style picture**: the 8-pixel border a TV hides is cropped on every side, showing 240×224.

## Technical

### CPU (6502)

- All official and unofficial opcodes, including the unstable `SHX`/`SHY`/`SHA`/`TAS` stores (whose value loses its `& (H + 1)` when DMA halts the cycle before the write). Cycle counts match the `nestest` reference log, including page-crossing and branch penalties.
- Every memory access takes its own cycle, and the PPU and APU run up to that point first, so a register read or write lands on the exact PPU dot it would on hardware.
- The 6502's dummy accesses happen on the bus too: indexed reads that cross a page first read the wrong address, indexed stores always read before writing, read-modify-write instructions write the old value back before the new one, one-byte instructions read the next byte, indexed zero-page modes read the unindexed address, stack pulls and `JSR` read the stack, `RTS` reads its return address, and taken branches read the next opcode. Games that rely on these side effects on `$2002`, `$2007`, `$4015` or `$4016` behave as on hardware.
- NMI and IRQ are polled on an instruction's second-to-last cycle (so an IRQ waiting on `CLI` runs after the next instruction), with the edge cases: an NMI arriving during BRK or an IRQ takes over its vector, a handler's first instruction always runs before another interrupt, and a taken branch polls only before its second cycle (and, if it crosses a page, before its fourth), so an IRQ arriving during its third cycle, even in a DMA that stalls it, waits one instruction.
- Reset works as on a front-loading NES: A, X, Y and RAM are kept, S drops by 3, the APU is silenced and its frame counter restarted, and the PPU is reset too (see below).

### PPU

- Dot-accurate: each of the 341 dots per line does what the real PPU does on it. Background tiles are fetched every 8 dots into shift registers; sprites for the next line are evaluated on dots 1–256 (secondary OAM cleared on dots 1–64, then one OAM byte read every other dot) and loaded into the 8 sprite units on dots 257–320; one pixel comes out per dot.
- Memory accesses take two dots, as on the chip: the address goes out and its low byte into the board's octal latch on one dot, and the read happens on the next from the high byte and that latch, while the low address lines carry the data. A `$2007` read starts the PPU's data state machine, which latches v 4 dots later and fills the read buffer 2 dots after that; with rendering on, the buffer gets whatever rendering reads on that dot, or collides with it. A second `$2006` write reaches v 4 dots after the write, so a fetch in between reads from a "hybrid" address.
- Rendering on and off: shifting, pixel output and the scroll updates follow PPUMASK a dot after it's written, and the fetches and sprite evaluation a dot later still. The background shift registers shift in 1s (high plane) and the attribute latch, and only reload when a tile's last fetch happened, so turning rendering off and on draws stale or shifted-in data.
- Scrolling uses the real internal registers (loopy `v`, `t`, `x` and `w`), so mid-line changes to scroll, CHR banks, palettes or PPUMASK show up on the dot they happen.
- Sprites: 8×8 and 8×16, 8 per line with the overflow flag set on the exact dot (including its hardware bug), priority, left-column clipping, and sprite-0 hit on the exact pixel. Evaluation starts wherever OAMADDR points, and the first sprite it finds acts as sprite 0. With OAMADDR not a multiple of 4, sprites overlap and realign exactly as on hardware (a Y or X byte out of range realigns OAMADDR to a sprite boundary). `$2004` reads during rendering return the byte the PPU is working on (sampled a dot later than other registers), and writes skip to the start of the next sprite instead of writing.
- Sprite units: each of the 8 has an X counter, an attribute latch and two pattern shift registers. The counters run even with rendering off but the shift registers don't, and turning rendering off over dot 339 starts every sprite at X = 0. Secondary OAM has its real address register: the sprite fetch reads Y, tile, attribute and X through it, and once it wraps it stays frozen at 0 until rendering is on for its reset on dot 63, 255 or 339.
- OAM corruption: turning rendering off on a rendering line copies OAM row 0 over the row picked by secondary OAM's address at that moment, the next time rendering runs. And if OAMADDR is 8 or more when rendering starts, the OAM row it points to is copied over sprites 0 and 1, as on the 2C02 and Dendy (not PAL). `--no-oam-corruption` turns off the first kind (see [Test ROMs](#test-roms)).
- Colours: the palette is decoded from a model of the PPU's composite signal. On PAL and Dendy, whose PPUs mirror the colour phases on alternate lines, the PAL TV's colour reference turns every hue by 15°, so they get their own palette. Greyscale and colour emphasis work too (greyscale also applies to palette reads through `$2007`): each PPUMASK tint bit attenuates part of the colour signal, as on the real chip, with red and green swapped on PAL and Dendy.
- Vblank and NMI timing down to the dot, including the `$2002` read race and the odd-frame dot skip. A `$2002` read takes the vblank flag at the start of the CPU's read cycle but the sprite flags at its end, about 2 dots later, as the real CPU's M2 clock does, and they pass through a latch first: a sprite 0 hit shows 2 dots after its pixel, an overflow 1 dot after it's found.
- Open bus: an I/O latch that every register write fills, which write-only registers and undefined bits read back, and whose bits fade after about 600 ms.
- After reset, writes to `$2000`, `$2001`, `$2005` and `$2006` are ignored until the end of the next vblank, as on a front-loading NES.

### APU

- Both pulse channels, triangle, noise and DMC, the frame counter (envelopes, sweeps, length counters) with its frame interrupt, the DMC interrupt, the nonlinear mixer and the NES's output filters.
- Timing is to the CPU cycle: `$4017` writes land 3 or 4 cycles later, a `$4015` read clears the frame interrupt flag 1 or 2 cycles later, length counter writes interact correctly with clocks on the same cycle, a `$4010` rate written on the cycle the DMC timer reloads takes effect at once, and the power-on state matches hardware.
- The controllers' strobe line follows `$4016` writes only when a get cycle turns into a put cycle, so of two writes on back-to-back cycles the controllers may see only the second.
- Audio sync: dynamic rate control keeps about 25 ms of audio buffered, which avoids crackles from the sound card's clock drifting against the system clock.

### DMA and regions
- OAM DMA and DMC sample fetches halt the CPU on its next read cycle (repeating that read, side effects and all), then take turns on alternating get/put cycles, so OAM DMA takes 513 or 514 cycles and a DMC fetch 1–4.
- Turning the DMC off takes 2 or 3 cycles, and a fetch that starts in that window is cut short after its halt cycle (an explicit abort); a 1-byte sample can do the same to itself (an implicit abort).
- The APU and controller registers decode only the low 5 bits of the address and only answer while the CPU's address is in `$4000`–`$401F`. A DMA that halts the CPU there reads them through every `$20` bytes it touches (clearing the frame interrupt, clocking a controller), colliding with RAM, the PPU or the cartridge where those answer too.
- Two data buses: the CPU's internal bus, which `$4015` and the controllers' floating bits read from, and the external one that open bus reads see. A `$4015` read only reaches the internal one, and a DMC fetch only the external one.
- Clocks are derived exactly from each console's master crystal:

  | | NTSC | PAL | Dendy |
  |---|---|---|---|
  | CPU clock | 1.789773 MHz | 1.662607 MHz | 1.773448 MHz |
  | PPU dots per CPU cycle | 3 | 3.2 | 3 |
  | Scanlines per frame (vblank from line) | 262 (241) | 312 (241) | 312 (291) |
  | Odd-frame dot skip | yes | no | no |
  | APU noise/DMC/frame sequencer tables | NTSC | PAL | NTSC |
  | Frame rate | 60.0988 | 50.0070 | 50.0070 |

### Mappers and ROMs

| Mapper | Board | Games |
|--------|-------|-------|
| 0 | NROM | *Super Mario Bros.*, *Donkey Kong*, *Balloon Fight* |
| 1 | MMC1 (SxROM) | *The Legend of Zelda*, *Metroid*, *Final Fantasy*, *Mega Man 2* |
| 2 | UxROM | *Mega Man*, *Castlevania*, *Contra*, *DuckTales* |
| 3 | CNROM | *Gradius*, *Arkanoid*, *Paperboy* |
| 4 | MMC3 (TxROM) | *Super Mario Bros. 2* and *3*, *Mega Man 3–6*, *Kirby's Adventure* |
| 5 | MMC5 (ExROM) | *Castlevania III*, *Just Breed*, *Laser Invasion*, *Uncharted Waters* |
| 7 | AxROM | *Battletoads*, *Marble Madness*, *Wizards & Warriors* |
| 9 | MMC2 (PNROM) | *Punch-Out!!* |
| 10 | MMC4 (FxROM) | *Fire Emblem*, *Famicom Wars* |
| 11 | Color Dreams | *Crystal Mines*, *Spiritual Warfare* |
| 13 | CPROM | *Videomation* |
| 21, 22, 23, 25 | Konami VRC2/VRC4 | *Contra* (Japan), *Gradius II*, *Ganbare Goemon*, *Tiny Toon Adventures* |
| 24, 26 | Konami VRC6 | *Akumajou Densetsu*, *Madara*, *Esper Dream 2* |
| 34 | BNROM, NINA-001 | *Deadly Towers*, *Impossible Mission II* |
| 66 | GxROM | *Super Mario Bros. + Duck Hunt*, *Dragon Power* |
| 69 | Sunsoft FME-7/5B | *Batman: Return of the Joker*, *Gimmick!*, *Hebereke* |
| 70, 152 | Bandai | *Family Trainer*, *Arkanoid II* |
| 71 | Camerica | *Micro Machines*, *Fire Hawk* |
| 78 | Irem/Jaleco | *Holy Diver*, *Cosmo Carrier* |
| 79 | NINA-03/06 | *Krazy Kreatures*, *Tiles of Fate* |
| 87 | Jaleco/Konami | *City Connection*, *The Goonies* (Japan) |
| 94 | UN1ROM | *Senjou no Ookami* |
| 118 | TxSROM (MMC3) | *Armadillo*, *Goal! Two* |
| 119 | TQROM (MMC3) | *Pin Bot*, *High Speed* |
| 140 | Jaleco JF-11/14 | *Bio Senshi Dan* |
| 180 | UNROM (Crazy Climber) | *Crazy Climber* |
| 206 | Namco 108 (DxROM) | *Gauntlet*, *Karnov*, *Babel no Tou* |

- Cartridges see every address the PPU puts on its bus, with the dot it happened on, and can raise the CPU's IRQ line. MMC3 counts rising edges of address line A12 with the chip's filter for short dips, exactly as the hardware does.
- MMC5 works out where the PPU is the way the chip does, from the order of the PPU's fetches, for its scanline IRQ and for 8x16 sprites with their own CHR banks. Its ExRAM, fill mode and extended attributes (a CHR bank and palette per background tile) are emulated; its vertical split screen (used only by *Uchuu Keibitai SDF*) isn't.
- MMC2/MMC4 switch CHR banks when the PPU reads the marker tiles, the read after the marker getting the new bank, as on the chips.
- Expansion sound from VRC6 (two pulses and a sawtooth), Sunsoft 5B (three square waves with noise and envelope) and MMC5 (two pulses and PCM) is mixed in with the APU's, as a Famicom does. VRC4, VRC6 and FME-7 IRQs count CPU cycles (with the VRC's scanline prescaler).
- MMC1 ignores a write on the cycle right after another, as the real chip does, so read-modify-write instructions on its registers work.
- NES 2.0 headers: 12-bit mapper numbers, submappers, RAM sizes, CPU/PPU timing and console type.

### Consoles and controllers

- `--console nes|toploader|famicom` picks the console model. Only the front-loading NES (the default) resets the PPU with the reset button. The controller ports' unconnected bits read as open bus where each model leaves them floating, and on the Famicom, `M` speaks into controller 2's microphone (bit 2 of `$4016`). Nothing in a ROM header says Famicom, so ROMs tagged "(J)" or "(Japan)" pick it from the file name.
- Each controller combines its keys with any number of gamepads. New gamepads go to the player with fewer, and pressing opposite directions together (which a real d-pad can't do, and some games misbehave on) cancels them out.

### Test ROMs

These pass in full:

- **CPU**: blargg's `instr_test-v5`, `nes_instr_test`, `instr_timing`, `instr_misc`, `cpu_interrupts_v2`, `cpu_dummy_reads`, `cpu_dummy_writes`, `cpu_exec_space` and `cpu_reset`
- **PPU**: `ppu_vbl_nmi`, `sprite_hit_tests`, `sprite_overflow_tests` (5.Emulator only with `--no-oam-corruption`, see below), `ppu_open_bus`, `ppu_read_buffer`, `oam_read` and `oam_stress`
- **APU**: `apu_test`, the 2005 APU tests, `pal_apu_tests` and `apu_reset`
- **DMA**: `sprdma_and_dmc_dma` and `dmc_dma_during_read4`
- **Mappers**: `mmc3_test_2` tests 1–5 (and 6 for boards marked as MMC3A in a NES 2.0 header)

[AccuracyCoin](https://github.com/100thCoin/AccuracyCoin), a newer and much stricter collection of 146 tests, passes all 146. Its PPU tests follow the model in [TriCNES](https://github.com/100thCoin/TriCNES), the emulator by AccuracyCoin's author.

The two disagree on one point: blargg's `sprite_overflow_tests/5.Emulator` (test 3) turns rendering off mid-line at a dot that, by AccuracyCoin's hardware measurements, corrupts the OAM row holding sprites it then counts. It passes with `--no-oam-corruption`, which is also Mesen's default.

## Limitations

- **Unsupported mappers.** The emulator exits with a message for mappers it doesn't have (see [Mappers](#mappers-and-roms)), such as Namco 163, VRC7 and the Bandai and Taito boards. No Famicom Disk System.
- **MMC1 variants.** SOROM/SXROM boards with 16–32 KiB of banked PRG-RAM are treated as having one 8 KiB bank.
- **PAL colors.** The PAL palette comes from the signal model, not from measurements of a real PAL console, so it may not match one exactly.
- **One CPU/PPU clock alignment.** A console powers up in one of four alignments between its CPU and PPU clocks, which shift some PPU effects by a dot (which row OAM corruption hits, when a `$2001` write takes effect). The emulator always uses the same one.
- **Other input devices.** Only standard controllers (and the Famicom microphone) are emulated: no Zapper, Power Pad or Famicom expansion-port devices.

## Building and running

You need:

- **Rust 1.85 or newer** (the crate uses the 2024 edition; `rust-version` in `Cargo.toml` says so, so Cargo gives a clear error on older compilers).
- **SDL2 development libraries:**
  - Linux: `sudo apt install libsdl2-dev`
  - macOS: `brew install sdl2`
  - Windows: see the [rust-sdl2 setup guide](https://github.com/Rust-SDL2/rust-sdl2#windows-msvc). It explains where to put `SDL2.lib` and `SDL2.dll`.

Then from the project folder:

```sh
cargo run --release -- ff.nes          # roms/ff.nes
cargo run --release -- path/to/game.nes
cargo run --release -- --region pal "Game (Europe).nes"
cargo run --release -- --console famicom game.nes
```

Pass the ROM to play as an argument. A bare file name is looked up in the current folder, then in `roms/`. With no argument it loads `Alter_Ego.nes`.

`--release` gives the fastest build (about 4 ms per frame, a quarter of the 16.7 ms a frame lasts). A plain `cargo run` also keeps up: `Cargo.toml` builds the dev profile at `opt-level = 1`, which is still debuggable but runs at about 5 ms per frame instead of 30 ms. Without it, the emulator falls behind real time and the audio crackles.

## Launcher

`launcher/` is a game library window in the style of DuckStation's game list. It finds your games and plays the one you pick in its own window, using the emulator as a library (`src/nes.rs`). The standalone emulator works the same with or without it.

```sh
cargo run -p nes_launcher --release      # from the project folder
```

- **Library.** Add folders (subfolders are included) or single `.nes` files, with the toolbar buttons or by dropping them onto the window. No-Intro style names are tidied up: `Legend of Zelda, The (USA) (Rev 1).nes` is listed as *The Legend of Zelda*, with `USA` and `Rev 1` as tags.
- **Game list.** Shows region, mapper, size, last played and total time played. Click a column header to sort, type in the search box (`Ctrl+F`) to filter, and use the arrow keys and `Enter` to pick and play.
- **Favorites.** Click the ☆ next to a game (or the button under its title, or press X on a gamepad) to make it a favorite, and **★ Favorites** in the toolbar to show only those. Favorites have a ★ on their covers too. With *Last played* sorted, recently played games come first.
- **Titles and covers.** **Get titles and covers** in Settings downloads No-Intro's list of NES games (about 3 MB, from the [libretro database](https://github.com/libretro/libretro-database)) and names your games from it. Games are recognized by a checksum of the ROM, so `smb.nes` is listed as *Super Mario Bros.* with its `World` tag, and the list's name can also give the region and console when the file name doesn't. Box art for the games it finds comes from [libretro-thumbnails](https://github.com/libretro-thumbnails/Nintendo_-_Nintendo_Entertainment_System), and games you add later get theirs too. **☰ List** and **🖼 Covers** in the toolbar switch between the list and a grid of covers (arrow keys and `Enter` work there too), and the selected game's cover is shown in the details panel. The list and covers are kept in the data folder (below), as `game-list.txt` and `covers/`, so they're only downloaded once.
- **Details panel.** Shows what the ROM header says and the region and console the emulator will pick. Games using a mapper the emulator doesn't support yet are marked and can't be started.
- **Per-game settings.** Region, console and OAM row corruption: the same choices as the emulator's `--region`, `--console` and `--no-oam-corruption`.
- **Controls.** **🎮 Controls** in the toolbar sets the keys and gamepad buttons for each player's NES buttons (and the Famicom microphone): click a box, then press the key or gamepad button for it (`Backspace` clears it, `Esc` stops). A key can be bound to more than one button, and a button can have several gamepad buttons (the defaults give A and B two each, so the face buttons work by position). `Esc`, `Tab`, `F5`, `F8`, `F9` and `F11` stay the launcher's own keys.
- **Gamepad.** The launcher works from a gamepad too. In the library the d-pad or left stick moves the selection (hold it to keep going), LB/RB move a page, Y switches between the list and covers, X makes the game a favorite, and A or Start plays. In a game, Guide or Back+Start opens the pause menu, where the d-pad moves between the buttons, A presses one and B resumes.
- **Play.** Double-click a game, press `Enter` or click Play, and the game fills the window. The keys and gamepads start out the same as the standalone emulator's (see [Controls](#controls)). `F11` switches fullscreen on and off, and `Ctrl+R` resets. Battery saves go in the `.sav` file next to the ROM, and closing the window during a game saves it and its play time first.
- **Pause menu.** `Esc` pauses the game and opens a menu over it: Resume (or `Esc` again), Reset, Fullscreen, the screen settings, the save states, and Quit to library. Time spent paused doesn't count as play time.
- **Save states.** Each game has 4 slots, shown in the pause menu with the picture and time of each save. Saving or loading there carries on playing; `F5` saves and `F8` loads the last slot used, without opening the menu. A state carries on exactly where it was saved (picture and sound), and only loads into the game, region and console it was made with. States are kept in the launcher's data folder (below), in `states/`.
- **Fast forward.** Hold `Tab` to run the game faster: 4× normal speed, or 2×, 3×, 8× or as fast as the computer can, set in the screen settings. The sound is off while it runs fast, and it stops when you let go of `Tab`, pause, or leave the window.
- **Screen settings** (in the pause menu and in Settings). *Whole pixels* draws the game at the biggest whole multiple of its size that fits, so every pixel is the same size; *Fill the window* makes it as big as fits. *TV pixel shape* draws the pixels 8:7 wide, as a TV did. The game can also pause by itself when the window loses focus.
- **Sound.** The volume slider and *Mute* are with the screen settings (in the pause menu and in Settings), and `F9` mutes and unmutes during a game. Both are remembered.

The emulator runs on its own thread, at the console's frame rate, and SDL plays the sound and reads the gamepads, so the launcher needs the SDL2 libraries too. The launcher can't tell the left and right `Shift` and `Ctrl` keys apart, so with the default keys either one works as player 2's Start and Select.

On Linux, including WSL, the **Add folder…** and **Add games…** dialogs come from the desktop's portal service or, failing that, `zenity`. WSL has neither until you install zenity (`sudo apt install zenity`). You can also type or paste a folder's path in the box below **Add folder…** (on the empty library screen, or in Settings). Under WSL, Windows paths like `C:\Users\you\ROMs` are turned into `/mnt/c/Users/you/ROMs`.

WSL shows Linux windows on the Windows desktop through WSLg, which keeps a window's frame around it in fullscreen ([microsoft/wslg#530](https://github.com/microsoft/wslg/issues/530)), so the launcher takes the frame off while it's fullscreen. If the frame still shows, start the launcher with `env -u WAYLAND_DISPLAY cargo run -p nes_launcher --release`: that runs it through WSLg's X11 support, where removing the frame is known to work.

The launcher's settings and play times are saved in the platform's app data folder (`%APPDATA%\NES Launcher\data` on Windows, `~/.local/share/neslauncher` on Linux, `~/Library/Application Support/NES-Launcher` on macOS).

 ## Controls 
| NES button | Player 1 keys | Player 2 keys | Gamepad |
|------------|---------------|---------------|---------|
| D-pad      | Arrow keys    | `I` `J` `K` `L` | D-pad or left stick |
| A          | `A`           | `G`           | Right or top face button (Xbox B/Y) |
| B          | `S`           | `F`           | Bottom or left face button (Xbox A/X) |
| Start      | `Enter`       | Right `Shift` | Start |
| Select     | `Space`       | Right `Ctrl`  | Back / Select |

The face buttons go by position, as on an NES pad, where B is to the left of A. The first gamepad plugged in is player 1 and the second player 2.

| Console | Key |
|---------|-----|
| Reset | `Ctrl+R` |
| Famicom microphone (with `--console famicom`) | `M` (hold) |
| Quit | `Esc` |

## Tests

```sh
cargo test
```

The tests cover CPU instructions, PPU registers, VRAM mirroring, scrolling, sprites and frame timing, APU channels and sample generation, and iNES / NES 2.0 header parsing, and bank switching for each mapper.

## Project layout

| File | Purpose |
|------|---------|
| `src/main.rs` | SDL window, command-line options and the main loop |
| `src/lib.rs`, `src/nes.rs` | The emulator as a library. `Nes` loads a game, runs it one frame at a time, and saves and loads states, for frontends like the launcher |
| `src/state.rs` | The save state format: each part of the console writes its state in a fixed order and reads it back (`save_state`/`load_state`) |
| `src/input.rs` | Keyboard and gamepad input for both controllers |
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
| `src/region.rs` | NTSC, PAL and Dendy clocks and timing tables, and the console models |
| `src/trace.rs` | CPU trace output in `nestest` log format |
| `launcher/` | The game library window, which also plays the games (see [Launcher](#launcher)). `gamelist.rs` downloads the game list and covers |

## Acknowledgements and references

- [Writing NES Emulator in Rust](https://bugzmanov.github.io/nes_ebook/) by bugzmanov is the tutorial this emulator is built on. It covers the CPU, bus, cartridge, PPU, rendering and joypad.
- [TetaNES](https://github.com/lukexor/tetanes/tree/main) by lukexor is a full-featured NES emulator in Rust, used as a reference.
- [NESdev Wiki](https://www.nesdev.org/wiki/Nesdev_Wiki) is the main hardware reference for CPU, PPU and APU behaviour and timing, and for the iNES and NES 2.0 file formats.