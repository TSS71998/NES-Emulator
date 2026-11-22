# NES-Emulator: technical details

How the emulator works, how accurate it is and what it doesn't do yet. For an overview, see the [README](../README.md).

## Contents

- [CPU (6502)](#cpu-6502)
- [PPU](#ppu)
- [APU](#apu)
- [DMA and regions](#dma-and-regions)
- [Mappers and ROMs](#mappers-and-roms)
- [Consoles and controllers](#consoles-and-controllers)
- [Test ROMs](#test-roms)
- [Limitations](#limitations)
- [Project layout](#project-layout)

## CPU (6502)

- All official and unofficial opcodes, including the unstable `SHX`/`SHY`/`SHA`/`TAS` stores (whose value loses its `& (H + 1)` when DMA halts the cycle before the write). Cycle counts match the `nestest` reference log, including page-crossing and branch penalties.
- Every memory access takes its own cycle, and the PPU and APU run up to that point first, so a register read or write lands on the exact PPU dot it would on hardware.
- The 6502's dummy accesses happen on the bus too: indexed reads that cross a page first read the wrong address, indexed stores always read before writing, read-modify-write instructions write the old value back before the new one, one-byte instructions read the next byte, indexed zero-page modes read the unindexed address, stack pulls and `JSR` read the stack, `RTS` reads its return address, and taken branches read the next opcode. Games that rely on these side effects on `$2002`, `$2007`, `$4015` or `$4016` behave as on hardware.
- NMI and IRQ are polled on an instruction's second-to-last cycle (so an IRQ waiting on `CLI` runs after the next instruction), with the edge cases: an NMI arriving during BRK or an IRQ takes over its vector, a handler's first instruction always runs before another interrupt, and a taken branch polls only before its second cycle (and, if it crosses a page, before its fourth), so an IRQ arriving during its third cycle, even in a DMA that stalls it, waits one instruction.
- Reset works as on a front-loading NES: A, X, Y and RAM are kept, S drops by 3, the APU is silenced and its frame counter restarted, and the PPU is reset too (see below).

## PPU

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

## APU

- Both pulse channels, triangle, noise and DMC, the frame counter (envelopes, sweeps, length counters) with its frame interrupt, the DMC interrupt, the nonlinear mixer and the NES's output filters.
- Timing is to the CPU cycle: `$4017` writes land 3 or 4 cycles later, a `$4015` read clears the frame interrupt flag 1 or 2 cycles later, length counter writes interact correctly with clocks on the same cycle, a `$4010` rate written on the cycle the DMC timer reloads takes effect at once, and the power-on state matches hardware.
- The controllers' strobe line follows `$4016` writes only when a get cycle turns into a put cycle, so of two writes on back-to-back cycles the controllers may see only the second.
- Audio sync: dynamic rate control keeps about 25 ms of audio buffered, which avoids crackles from the sound card's clock drifting against the system clock.

## DMA and regions
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

## Mappers and ROMs

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

## Consoles and controllers

- `--console nes|toploader|famicom` picks the console model. Only the front-loading NES (the default) resets the PPU with the reset button. The controller ports' unconnected bits read as open bus where each model leaves them floating, and on the Famicom, `M` speaks into controller 2's microphone (bit 2 of `$4016`). Nothing in a ROM header says Famicom, so ROMs tagged "(J)" or "(Japan)" pick it from the file name.
- Each controller combines its keys with any number of gamepads. New gamepads go to the player with fewer, and pressing opposite directions together (which a real d-pad can't do, and some games misbehave on) cancels them out.

## Test ROMs

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

## Project layout

| File | Purpose |
|------|---------|
| `src/main.rs` | SDL window, command-line options and the main loop |
| `src/lib.rs`, `src/nes.rs` | The emulator as a library. `Nes` loads a game, runs it one frame at a time, and saves and loads states, for frontends like the launcher and the web version |
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
| `launcher/` | The game library window, which also plays the games (see [LAUNCHER.md](LAUNCHER.md)). `gamelist.rs` downloads the game list and covers |
| `web/` | The browser version: `src/lib.rs` exports the emulator to WebAssembly, `www/` is the page, and `build.sh` builds both into `web/dist` (see the [README](../README.md#play-it-in-your-browser)) |
