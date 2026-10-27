use super::{Chr, Mapper, bank_offset};
use crate::apu::{DUTY_TABLE, LENGTH_TABLE};
use crate::cartridge::{Mirroring, Rom};
use crate::state::{StateReader, StateWriter};
use std::cell::Cell;

const PRG_BANK: usize = 0x2000;
/// No address seen yet (for the scanline detector).
const NO_ADDRESS: u16 = 0xFFFF;

/// Which kind of pattern fetch the PPU is doing, as the MMC5 works it out
/// from the order of the fetches.
#[derive(Clone, Copy, PartialEq)]
enum Fetch {
    Background,
    Sprite,
}

/// Mapper 5: Nintendo MMC5 (ExROM boards: Castlevania III, Just Breed,
/// Laser Invasion, Metal Slader Glory, Uncharted Waters, the Koei games).
///
/// The biggest of the Nintendo mappers:
///
/// - PRG in 8, 16 or 32 KiB banks ($5100 mode, $5113-$5117), with up to
///   64 KiB of PRG-RAM that can be banked into $6000-$DFFF. Writes need
///   $5102 = 2 and $5103 = 1.
/// - CHR in 1, 2, 4 or 8 KiB banks ($5101 mode), with two sets of
///   registers: $5120-$5127 for sprites and $5128-$512B for the background
///   when sprites are 8x16, so the two can use different tiles.
/// - 1 KiB of ExRAM inside the chip ($5C00-$5FFF, mode $5104): an extra
///   nametable, or "extended attributes" (a CHR bank and palette for every
///   background tile), or plain RAM.
/// - Nametable mapping per nametable ($5105): either half of the
///   console's VRAM, ExRAM, or a fill pattern ($5106 tile, $5107 palette).
/// - A scanline IRQ ($5203 line, $5204 enable/status) and an 8x8 multiplier
///   ($5205/$5206).
/// - Two more pulse channels and an 8-bit PCM channel ($5000-$5015).
///
/// The MMC5 sees no PPU timing signals, only what's on the PPU bus, so it
/// works out where the PPU is from the fetches: three reads in a row of
/// the same nametable address (the two dummy fetches at the end of a line
/// and the first fetch of the next) mean a new scanline. It then counts
/// pattern fetches to tell sprites (the 16 after the 64 background ones)
/// from background tiles. When the PPU stops reading for 3 CPU cycles
/// (vblank, or rendering off) it's out of the frame.
///
/// Not emulated: the vertical split screen ($5200-$5202; only Uchuu
/// Keibitai SDF uses it) and the PCM channel's read mode.
pub struct Mmc5 {
    prg_rom: Vec<u8>,
    prg_ram: Vec<u8>,
    chr: Chr,
    has_battery: bool,
    exram: Vec<u8>,

    prg_mode: u8,
    chr_mode: u8,
    ram_protect: [u8; 2],
    exram_mode: u8,
    nametables: u8,
    fill_tile: u8,
    fill_attribute: u8,
    /// $5113-$5117
    prg_banks: [u8; 5],
    /// $5120-$5127 and $5128-$512B, with $5130's bits on top.
    chr_a: [u16; 8],
    chr_b: [u16; 4],
    chr_upper: u8,
    /// Whether $5128-$512B were written last (they're used outside the
    /// frame then).
    last_set_b: bool,

    irq_target: u8,
    irq_enabled: bool,
    /// Reading $5204 clears it, and reads only have `&self`.
    irq_pending: Cell<bool>,
    multiplicand: u8,
    multiplier: u8,

    // What the MMC5 knows about the PPU
    large_sprites: bool,
    rendering_enabled: bool,
    in_frame: bool,
    scanline: u8,
    last_address: u16,
    match_count: u8,
    new_line: bool,
    ppu_reading: bool,
    idle_cycles: u8,
    /// The last read was a nametable fetch, so a nametable address next is
    /// the attribute fetch.
    after_nametable_fetch: bool,
    attribute_fetch: bool,
    pattern_reads: u8,
    fetch: Fetch,
    /// The ExRAM byte for the background tile being fetched (extended
    /// attribute mode).
    ext_byte: u8,

    audio: Mmc5Audio,
}

impl Mmc5 {
    pub fn new(rom: Rom) -> Self {
        let ram_size = match rom.prg_ram_size + rom.prg_nvram_size {
            0 => 0x10000,
            size => size.next_power_of_two().max(0x2000),
        };
        Mmc5 {
            prg_ram: vec![0; ram_size],
            chr: Chr::new(&rom),
            has_battery: rom.has_battery,
            prg_rom: rom.prg_rom,
            exram: vec![0; 0x400],
            prg_mode: 3,
            chr_mode: 0,
            ram_protect: [1, 2],
            exram_mode: 3,
            nametables: 0,
            fill_tile: 0,
            fill_attribute: 0,
            prg_banks: [0, 0xFF, 0xFF, 0xFF, 0xFF],
            chr_a: [0; 8],
            chr_b: [0; 4],
            chr_upper: 0,
            last_set_b: false,
            irq_target: 0,
            irq_enabled: false,
            irq_pending: Cell::new(false),
            multiplicand: 0xFF,
            multiplier: 0xFF,
            large_sprites: false,
            rendering_enabled: false,
            in_frame: false,
            scanline: 0,
            last_address: NO_ADDRESS,
            match_count: 0,
            new_line: false,
            ppu_reading: false,
            idle_cycles: 0,
            after_nametable_fetch: false,
            attribute_fetch: false,
            pattern_reads: 0,
            fetch: Fetch::Background,
            ext_byte: 0,
            audio: Mmc5Audio::default(),
        }
    }

    /// Where a CPU address in $6000-$FFFF goes: (is it RAM, offset).
    fn prg_target(&self, addr: u16) -> (bool, usize) {
        let a = addr as usize;
        if addr < 0x8000 {
            let bank = (self.prg_banks[0] & 7) as usize;
            return (true, bank_offset(bank, PRG_BANK, self.prg_ram.len()) + (a & 0x1FFF));
        }
        let slot = (a - 0x8000) / PRG_BANK;
        // (register in prg_banks, bank size in 8 KiB units)
        let (register, size) = match (self.prg_mode, slot) {
            (0, _) => (4, 4),
            (1, 0 | 1) | (2, 0 | 1) => (2, 2),
            (1, _) => (4, 2),
            (2, 2) => (3, 1),
            (2, _) => (4, 1),
            (_, slot) => (1 + slot, 1),
        };
        let value = self.prg_banks[register];
        let rom = register == 4 || value & 0x80 != 0;
        let within = (a >> 13) & (size - 1);
        let bank = ((value & 0x7F) as usize & !(size - 1)) | within;
        if rom {
            (false, bank_offset(bank, PRG_BANK, self.prg_rom.len()) + (a & 0x1FFF))
        } else {
            (true, bank_offset(bank & 7, PRG_BANK, self.prg_ram.len()) + (a & 0x1FFF))
        }
    }

    fn ram_writable(&self) -> bool {
        self.ram_protect[0] & 3 == 2 && self.ram_protect[1] & 3 == 1
    }

    /// Extended attributes are on while rendering is (as $2001 says): the
    /// first two tiles of a frame are fetched on the pre-render line,
    /// before the frame has been detected.
    fn ext_attributes(&self) -> bool {
        self.exram_mode == 1 && self.rendering_enabled
    }

    /// Which 1 KiB CHR bank a pattern address is in, from register set A
    /// or B.
    fn chr_bank(&self, addr: usize) -> usize {
        let slot = addr / 0x400;
        let use_b = if self.in_frame && self.rendering_enabled {
            self.large_sprites && self.fetch == Fetch::Background
        } else {
            self.last_set_b
        };
        if use_b {
            let b = &self.chr_b;
            let s = slot & 3;
            match self.chr_mode {
                0 => b[3] as usize * 8 + slot,
                1 => b[3] as usize * 4 + s,
                2 => b[(s & 2) + 1] as usize * 2 + (s & 1),
                _ => b[s] as usize,
            }
        } else {
            let a = &self.chr_a;
            match self.chr_mode {
                0 => a[7] as usize * 8 + slot,
                1 => a[(slot & 4) + 3] as usize * 4 + (slot & 3),
                2 => a[(slot & 6) + 1] as usize * 2 + (slot & 1),
                _ => a[slot] as usize,
            }
        }
    }

    fn chr_offset(&self, addr: u16) -> usize {
        let addr = addr as usize & 0x1FFF;
        if self.ext_attributes() && self.fetch == Fetch::Background {
            let bank = (self.ext_byte & 0x3F) as usize | ((self.chr_upper as usize & 3) << 6);
            return bank_offset(bank, 0x1000, self.chr.len()) + (addr & 0x0FFF);
        }
        bank_offset(self.chr_bank(addr), 0x400, self.chr.len()) + (addr & 0x3FF)
    }

    fn nametable_source(&self, addr: u16) -> u8 {
        let quadrant = (addr >> 10) & 3;
        (self.nametables >> (quadrant * 2)) & 3
    }

    /// Out of the frame: the PPU stopped rendering.
    fn leave_frame(&mut self) {
        self.in_frame = false;
        self.last_address = NO_ADDRESS;
        self.match_count = 0;
        self.new_line = false;
    }

    /// The scanline detector found the start of a line.
    fn start_line(&mut self) {
        if self.in_frame {
            self.scanline = self.scanline.wrapping_add(1);
            if self.scanline == self.irq_target && self.irq_target != 0 {
                self.irq_pending.set(true);
            }
        } else {
            self.in_frame = true;
            self.scanline = 0;
            self.irq_pending.set(false);
        }
        self.pattern_reads = 0;
    }
}

impl Mapper for Mmc5 {
    fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.prg_ram);
        self.chr.save_state(w);
        w.bytes(&self.exram);
        w.u8(self.prg_mode);
        w.u8(self.chr_mode);
        w.bytes(&self.ram_protect);
        w.u8(self.exram_mode);
        w.u8(self.nametables);
        w.u8(self.fill_tile);
        w.u8(self.fill_attribute);
        w.bytes(&self.prg_banks);
        for bank in self.chr_a.iter().chain(&self.chr_b) {
            w.u16(*bank);
        }
        w.u8(self.chr_upper);
        w.bool(self.last_set_b);
        w.u8(self.irq_target);
        w.bool(self.irq_enabled);
        w.bool(self.irq_pending.get());
        w.u8(self.multiplicand);
        w.u8(self.multiplier);
        w.bool(self.large_sprites);
        w.bool(self.rendering_enabled);
        w.bool(self.in_frame);
        w.u8(self.scanline);
        w.u16(self.last_address);
        w.u8(self.match_count);
        w.bool(self.new_line);
        w.bool(self.ppu_reading);
        w.u8(self.idle_cycles);
        w.bool(self.after_nametable_fetch);
        w.bool(self.attribute_fetch);
        w.u8(self.pattern_reads);
        w.bool(self.fetch == Fetch::Sprite);
        w.u8(self.ext_byte);
        self.audio.save_state(w);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        r.bytes_into(&mut self.prg_ram);
        self.chr.load_state(r);
        r.bytes_into(&mut self.exram);
        self.prg_mode = r.u8() & 3;
        self.chr_mode = r.u8() & 3;
        r.bytes_into(&mut self.ram_protect);
        self.exram_mode = r.u8() & 3;
        self.nametables = r.u8();
        self.fill_tile = r.u8();
        self.fill_attribute = r.u8() & 3;
        r.bytes_into(&mut self.prg_banks);
        for bank in self.chr_a.iter_mut().chain(&mut self.chr_b) {
            *bank = r.u16() & 0x3FF;
        }
        self.chr_upper = r.u8() & 3;
        self.last_set_b = r.bool();
        self.irq_target = r.u8();
        self.irq_enabled = r.bool();
        self.irq_pending.set(r.bool());
        self.multiplicand = r.u8();
        self.multiplier = r.u8();
        self.large_sprites = r.bool();
        self.rendering_enabled = r.bool();
        self.in_frame = r.bool();
        self.scanline = r.u8();
        self.last_address = r.u16();
        self.match_count = r.u8().min(2);
        self.new_line = r.bool();
        self.ppu_reading = r.bool();
        self.idle_cycles = r.u8().min(3);
        self.after_nametable_fetch = r.bool();
        self.attribute_fetch = r.bool();
        self.pattern_reads = r.u8();
        self.fetch = if r.bool() { Fetch::Sprite } else { Fetch::Background };
        self.ext_byte = r.u8();
        self.audio.load_state(r);
    }

    fn cpu_read(&self, addr: u16) -> Option<u8> {
        match addr {
            0x5010 => Some(0), // PCM status: no read-mode IRQ
            0x5015 => Some(self.audio.status()),
            0x5204 => {
                let status = (self.irq_pending.get() as u8) << 7 | (self.in_frame as u8) << 6;
                self.irq_pending.set(false);
                Some(status)
            }
            0x5205 => Some((self.multiplicand as u16 * self.multiplier as u16) as u8),
            0x5206 => Some(((self.multiplicand as u16 * self.multiplier as u16) >> 8) as u8),
            0x5C00..=0x5FFF if self.exram_mode >= 2 => Some(self.exram[addr as usize & 0x3FF]),
            0x6000..=0xFFFF => {
                let (ram, offset) = self.prg_target(addr);
                Some(if ram { self.prg_ram[offset] } else { self.prg_rom[offset] })
            }
            _ => None,
        }
    }

    fn cpu_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x5000..=0x5015 => self.audio.write(addr, data),
            0x5100 => self.prg_mode = data & 3,
            0x5101 => self.chr_mode = data & 3,
            0x5102 | 0x5103 => self.ram_protect[addr as usize - 0x5102] = data & 3,
            0x5104 => self.exram_mode = data & 3,
            0x5105 => self.nametables = data,
            0x5106 => self.fill_tile = data,
            0x5107 => self.fill_attribute = data & 3,
            0x5113..=0x5117 => self.prg_banks[addr as usize - 0x5113] = data,
            0x5120..=0x5127 => {
                self.chr_a[addr as usize - 0x5120] = data as u16 | (self.chr_upper as u16) << 8;
                self.last_set_b = false;
            }
            0x5128..=0x512B => {
                self.chr_b[addr as usize - 0x5128] = data as u16 | (self.chr_upper as u16) << 8;
                self.last_set_b = true;
            }
            0x5130 => self.chr_upper = data & 3,
            0x5203 => self.irq_target = data,
            0x5204 => self.irq_enabled = data & 0x80 != 0,
            0x5205 => self.multiplicand = data,
            0x5206 => self.multiplier = data,
            0x5C00..=0x5FFF if self.exram_mode != 3 => self.exram[addr as usize & 0x3FF] = data,
            0x6000..=0xFFFF => {
                let (ram, offset) = self.prg_target(addr);
                if ram && self.ram_writable() {
                    self.prg_ram[offset] = data;
                }
            }
            _ => {}
        }
    }

    fn ppu_read(&self, addr: u16) -> u8 {
        self.chr.read(self.chr_offset(addr))
    }

    fn ppu_write(&mut self, addr: u16, data: u8) {
        let offset = self.chr_offset(addr);
        self.chr.write(offset, data);
    }

    fn mirroring(&self) -> Mirroring {
        // what the CIRAM nametables show; see nametable_pages
        Mirroring::Vertical
    }

    fn nametable_pages(&self) -> [u8; 4] {
        std::array::from_fn(|n| (self.nametables >> (n * 2)) & 1)
    }

    fn maps_nametables(&self) -> bool {
        true
    }

    fn nametable_read(&self, addr: u16) -> Option<u8> {
        if self.attribute_fetch && self.ext_attributes() {
            return Some((self.ext_byte >> 6) * 0x55);
        }
        match self.nametable_source(addr) {
            0 | 1 => None,
            2 if self.exram_mode <= 1 => Some(self.exram[addr as usize & 0x3FF]),
            2 => Some(0),
            _ if addr & 0x3FF >= 0x3C0 => Some(self.fill_attribute * 0x55),
            _ => Some(self.fill_tile),
        }
    }

    fn nametable_write(&mut self, addr: u16, data: u8) -> bool {
        match self.nametable_source(addr) {
            0 | 1 => false,
            2 => {
                if self.exram_mode <= 1 {
                    self.exram[addr as usize & 0x3FF] = data;
                }
                true
            }
            _ => true,
        }
    }

    fn watches_ppu_bus(&self) -> bool {
        true
    }

    fn ppu_address(&mut self, addr: u16, _ppu_cycle: u64) {
        self.ppu_reading = true;
        if self.new_line {
            self.new_line = false;
            self.start_line();
        }
        let nametable = (0x2000..0x3000).contains(&addr);
        if nametable && addr == self.last_address {
            self.match_count += 1;
            if self.match_count == 2 {
                self.new_line = true;
            }
        } else {
            self.match_count = 0;
        }
        self.last_address = addr;

        if nametable {
            self.attribute_fetch = self.after_nametable_fetch;
            self.after_nametable_fetch = !self.attribute_fetch;
            if !self.attribute_fetch && self.exram_mode == 1 {
                self.ext_byte = self.exram[addr as usize & 0x3FF];
            }
        } else if addr < 0x2000 {
            self.after_nametable_fetch = false;
            self.attribute_fetch = false;
            self.fetch = if self.in_frame && (64..80).contains(&self.pattern_reads) {
                Fetch::Sprite
            } else {
                Fetch::Background
            };
            self.pattern_reads = self.pattern_reads.saturating_add(1);
        }
    }

    fn watches_ppu_registers(&self) -> bool {
        true
    }

    fn ppu_register_write(&mut self, addr: u16, data: u8) {
        match addr {
            0x2000 => self.large_sprites = data & 0x20 != 0,
            0x2001 => {
                self.rendering_enabled = data & 0x18 != 0;
                if !self.rendering_enabled {
                    self.leave_frame();
                }
            }
            _ => {}
        }
    }

    fn irq_pending(&self) -> bool {
        self.irq_enabled && self.irq_pending.get()
    }

    fn clocked(&self) -> bool {
        true
    }

    fn cpu_clock(&mut self) {
        if self.ppu_reading {
            self.idle_cycles = 0;
        } else if self.idle_cycles < 3 {
            self.idle_cycles += 1;
            if self.idle_cycles == 3 {
                self.leave_frame();
            }
        }
        self.ppu_reading = false;
        self.audio.clock();
    }

    fn audio_output(&self) -> f32 {
        self.audio.output()
    }

    fn battery_ram(&self) -> Option<&[u8]> {
        self.has_battery.then_some(&self.prg_ram[..])
    }

    fn load_battery_ram(&mut self, data: &[u8]) {
        let n = data.len().min(self.prg_ram.len());
        self.prg_ram[..n].copy_from_slice(&data[..n]);
    }
}

/// CPU cycles between the MMC5's envelope and length counter clocks (it
/// has its own 240 Hz timer instead of the APU's frame counter).
const FRAME_PERIOD: u16 = 7457;

/// The MMC5's sound: two pulse channels like the APU's (without sweep) and
/// a raw 8-bit PCM channel.
#[derive(Default)]
struct Mmc5Audio {
    pulses: [Mmc5Pulse; 2],
    pcm: u8,
    pcm_read_mode: bool,
    frame_timer: u16,
    odd_cycle: bool,
}

#[derive(Default)]
struct Mmc5Pulse {
    enabled: bool,
    duty: u8,
    duty_pos: u8,
    period: u16,
    timer: u16,
    length: u8,
    halt: bool,
    constant: bool,
    volume: u8,
    envelope_start: bool,
    envelope_divider: u8,
    decay: u8,
}

impl Mmc5Audio {
    fn write(&mut self, addr: u16, data: u8) {
        match addr {
            0x5000..=0x5007 => {
                let pulse = &mut self.pulses[(addr as usize - 0x5000) / 4];
                pulse.write(addr & 3, data);
            }
            0x5010 => self.pcm_read_mode = data & 1 != 0,
            0x5011 => {
                if !self.pcm_read_mode && data != 0 {
                    self.pcm = data;
                }
            }
            0x5015 => {
                for (i, pulse) in self.pulses.iter_mut().enumerate() {
                    pulse.enabled = data & (1 << i) != 0;
                    if !pulse.enabled {
                        pulse.length = 0;
                    }
                }
            }
            _ => {}
        }
    }

    fn status(&self) -> u8 {
        (self.pulses[0].length > 0) as u8 | ((self.pulses[1].length > 0) as u8) << 1
    }

    fn clock(&mut self) {
        self.odd_cycle = !self.odd_cycle;
        if self.odd_cycle {
            for pulse in &mut self.pulses {
                pulse.clock_timer();
            }
        }
        self.frame_timer += 1;
        if self.frame_timer >= FRAME_PERIOD {
            self.frame_timer = 0;
            for pulse in &mut self.pulses {
                pulse.clock_envelope();
                if pulse.length > 0 && !pulse.halt {
                    pulse.length -= 1;
                }
            }
        }
    }

    fn output(&self) -> f32 {
        let pulses = (self.pulses[0].output() + self.pulses[1].output()) as f32;
        let pulse_level = if pulses == 0.0 {
            0.0
        } else {
            95.88 / (8128.0 / pulses + 100.0)
        };
        pulse_level + self.pcm as f32 * 0.0016
    }

    fn save_state(&self, w: &mut StateWriter) {
        for p in &self.pulses {
            w.bool(p.enabled);
            w.u8(p.duty);
            w.u8(p.duty_pos);
            w.u16(p.period);
            w.u16(p.timer);
            w.u8(p.length);
            w.bool(p.halt);
            w.bool(p.constant);
            w.u8(p.volume);
            w.bool(p.envelope_start);
            w.u8(p.envelope_divider);
            w.u8(p.decay);
        }
        w.u8(self.pcm);
        w.bool(self.pcm_read_mode);
        w.u16(self.frame_timer);
        w.bool(self.odd_cycle);
    }

    fn load_state(&mut self, r: &mut StateReader) {
        for p in &mut self.pulses {
            p.enabled = r.bool();
            p.duty = r.u8() & 3;
            p.duty_pos = r.u8() & 7;
            p.period = r.u16() & 0x7FF;
            p.timer = r.u16() & 0x7FF;
            p.length = r.u8();
            p.halt = r.bool();
            p.constant = r.bool();
            p.volume = r.u8() & 0x0F;
            p.envelope_start = r.bool();
            p.envelope_divider = r.u8() & 0x0F;
            p.decay = r.u8() & 0x0F;
        }
        self.pcm = r.u8();
        self.pcm_read_mode = r.bool();
        self.frame_timer = r.u16().min(FRAME_PERIOD);
        self.odd_cycle = r.bool();
    }
}

impl Mmc5Pulse {
    fn write(&mut self, register: u16, data: u8) {
        match register {
            0 => {
                self.duty = data >> 6;
                self.halt = data & 0x20 != 0;
                self.constant = data & 0x10 != 0;
                self.volume = data & 0x0F;
            }
            1 => {} // no sweep unit
            2 => self.period = (self.period & 0x700) | data as u16,
            _ => {
                self.period = (self.period & 0xFF) | ((data as u16 & 7) << 8);
                if self.enabled {
                    self.length = LENGTH_TABLE[(data >> 3) as usize];
                }
                self.duty_pos = 0;
                self.envelope_start = true;
            }
        }
    }

    fn clock_timer(&mut self) {
        if self.timer == 0 {
            self.timer = self.period;
            self.duty_pos = (self.duty_pos + 1) & 7;
        } else {
            self.timer -= 1;
        }
    }

    fn clock_envelope(&mut self) {
        if self.envelope_start {
            self.envelope_start = false;
            self.decay = 15;
            self.envelope_divider = self.volume;
        } else if self.envelope_divider == 0 {
            self.envelope_divider = self.volume;
            if self.decay > 0 {
                self.decay -= 1;
            } else if self.halt {
                self.decay = 15;
            }
        } else {
            self.envelope_divider -= 1;
        }
    }

    fn output(&self) -> u8 {
        if self.length == 0 || DUTY_TABLE[self.duty as usize][self.duty_pos as usize] == 0 {
            0
        } else if self.constant {
            self.volume
        } else {
            self.decay
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::mapper::test_util::banked_rom;

    fn mmc5() -> Mmc5 {
        Mmc5::new(banked_rom(5, 32, 32, true))
    }

    #[test]
    fn test_prg_modes() {
        let mut m = mmc5();
        assert_eq!(m.cpu_read(0xFFFF), Some(0x9F), "$5117 starts at the last bank");
        m.cpu_write(0x5114, 0x80 | 6); // 8 KiB ROM bank 6 = 16 KiB bank 3
        m.cpu_write(0x5115, 0x80 | 9);
        m.cpu_write(0x5116, 0x80 | 20);
        assert_eq!(m.cpu_read(0x8000), Some(3));
        assert_eq!(m.cpu_read(0xA000), Some(4));
        assert_eq!(m.cpu_read(0xC000), Some(10));
        m.cpu_write(0x5100, 1); // 16 KiB: $5115 (low bit ignored), $5117
        assert_eq!(m.cpu_read(0x8000), Some(4));
        assert_eq!(m.cpu_read(0xBFFF), Some(0x84));
        assert_eq!(m.cpu_read(0xC000), Some(31));
        m.cpu_write(0x5100, 0); // 32 KiB: $5117
        m.cpu_write(0x5117, 0x84);
        assert_eq!(m.cpu_read(0x8000), Some(2));
        assert_eq!(m.cpu_read(0xFFFF), Some(0x83));
    }

    #[test]
    fn test_prg_ram_banks_and_protection() {
        let mut m = mmc5();
        m.cpu_write(0x5113, 2);
        m.cpu_write(0x6000, 0x11);
        assert_eq!(m.cpu_read(0x6000), Some(0), "protected at power-on");
        m.cpu_write(0x5102, 2);
        m.cpu_write(0x5103, 1);
        m.cpu_write(0x6000, 0x11);
        m.cpu_write(0x5114, 2); // RAM bank 2 at $8000 (mode 3)
        assert_eq!(m.cpu_read(0x8000), Some(0x11));
        m.cpu_write(0x8001, 0x22);
        assert_eq!(m.cpu_read(0x6001), Some(0x22));
        assert_eq!(m.battery_ram().unwrap()[0x4001], 0x22);
    }

    #[test]
    fn test_chr_modes_and_sets() {
        let mut m = mmc5();
        m.cpu_write(0x5101, 3);
        m.cpu_write(0x5123, 77);
        assert_eq!(m.ppu_read(0x0C00), 77);
        m.cpu_write(0x5101, 1); // 4 KiB: $5123 at $0000, $5127 at $1000
        m.cpu_write(0x5127, 5);
        assert_eq!(m.ppu_read(0x0400), 53, "bank 77 * 4 + 1, of 256");
        assert_eq!(m.ppu_read(0x1800), 22);
        m.cpu_write(0x5130, 1);
        m.cpu_write(0x512B, 3); // set B, bank $103, written last: used out of the frame
        assert_eq!(m.ppu_read(0x0000), ((0x103 * 4) & 0xFF) as u8);
    }

    #[test]
    fn test_multiplier_and_exram() {
        let mut m = mmc5();
        m.cpu_write(0x5205, 200);
        m.cpu_write(0x5206, 100);
        assert_eq!(m.cpu_read(0x5205), Some((20000 & 0xFF) as u8));
        assert_eq!(m.cpu_read(0x5206), Some((20000 >> 8) as u8));
        m.cpu_write(0x5104, 2);
        m.cpu_write(0x5C10, 0x42);
        assert_eq!(m.cpu_read(0x5C10), Some(0x42));
        m.cpu_write(0x5104, 0);
        assert_eq!(m.cpu_read(0x5C10), None, "write-only while it's a nametable");
        m.cpu_write(0x5105, 0b11_10_01_00); // CIRAM 0, CIRAM 1, ExRAM, fill
        assert_eq!(m.nametable_pages()[..2], [0, 1]);
        assert_eq!(m.nametable_read(0x2810), Some(0x42));
        m.cpu_write(0x5106, 0x99);
        m.cpu_write(0x5107, 2);
        assert_eq!(m.nametable_read(0x2C00), Some(0x99));
        assert_eq!(m.nametable_read(0x2FC0), Some(0xAA));
        assert_eq!(m.nametable_read(0x2000), None);
    }

    /// Feeds the fetches of one rendered line, as the PPU does them: tiles
    /// 2-33 (0 and 1 came at the end of the line before), the sprites, then
    /// tiles 0 and 1 of the next line and two dummy fetches of tile 2.
    fn render_line(m: &mut Mmc5) {
        for tile in 0..32 {
            m.ppu_address(0x2002 + tile, 0);
            m.ppu_address(0x23C0, 0);
            m.ppu_address(0x0000, 0);
            m.ppu_address(0x0008, 0);
            m.cpu_clock();
            m.cpu_clock();
        }
        for _ in 0..8 {
            m.ppu_address(0x2000, 0);
            m.ppu_address(0x2000, 0);
            m.ppu_address(0x1000, 0);
            m.ppu_address(0x1008, 0);
            m.cpu_clock();
        }
        for tile in 0..2 {
            m.ppu_address(0x2000 + tile, 0);
            m.ppu_address(0x23C0, 0);
            m.ppu_address(0x0000, 0);
            m.ppu_address(0x0008, 0);
        }
        m.ppu_address(0x2002, 0);
        m.ppu_address(0x2002, 0);
        m.cpu_clock();
    }

    #[test]
    fn test_scanline_irq() {
        let mut m = mmc5();
        m.ppu_register_write(0x2001, 0x18);
        m.cpu_write(0x5203, 3);
        m.cpu_write(0x5204, 0x80);
        render_line(&mut m); // pre-render line
        for line in 0..3 {
            render_line(&mut m);
            assert!(m.in_frame);
            assert!(!m.irq_pending(), "line {line}");
        }
        render_line(&mut m);
        assert!(m.irq_pending());
        assert_eq!(m.cpu_read(0x5204), Some(0xC0));
        assert!(!m.irq_pending(), "reading $5204 acknowledges");
        for _ in 0..3 {
            m.cpu_clock();
        }
        assert!(!m.in_frame, "3 CPU cycles without PPU reads: vblank");
    }

    #[test]
    fn test_8x16_sprites_use_set_a() {
        let mut m = mmc5();
        m.cpu_write(0x5101, 0); // 8 KiB banks
        m.cpu_write(0x5127, 1); // sprites: bank 1
        m.cpu_write(0x512B, 2); // background: bank 2
        m.ppu_register_write(0x2000, 0x20);
        m.ppu_register_write(0x2001, 0x18);
        render_line(&mut m);
        // the first fetch of the next line is the third read of tile 2:
        // a new line, so background fetches come first
        m.ppu_address(0x2002, 0);
        m.ppu_address(0x23C0, 0);
        m.ppu_address(0x0000, 0);
        assert_eq!(m.ppu_read(0x0000), 16, "background: set B");
        for _ in 0..63 {
            m.ppu_address(0x0000, 0);
        }
        m.ppu_address(0x1000, 0);
        assert_eq!(m.ppu_read(0x1000), 12, "sprite: set A");
    }

    #[test]
    fn test_extended_attributes() {
        let mut m = mmc5();
        m.cpu_write(0x5104, 1);
        m.cpu_write(0x5C05, 0b10_000011); // palette 2, 4 KiB bank 3
        m.ppu_register_write(0x2001, 0x18);
        m.ppu_address(0x2005, 0);
        m.ppu_address(0x23C1, 0);
        assert_eq!(m.nametable_read(0x23C1), Some(0xAA));
        m.ppu_address(0x0010, 0);
        assert_eq!(m.ppu_read(0x0010), 12, "4 KiB bank 3: 1 KiB tag 12");
    }

    #[test]
    fn test_audio_pulse_and_pcm() {
        let mut m = mmc5();
        m.cpu_write(0x5015, 1);
        m.cpu_write(0x5000, 0b1011_1111); // 50%, constant volume 15
        m.cpu_write(0x5002, 0x20);
        m.cpu_write(0x5003, 0x08);
        assert_eq!(m.cpu_read(0x5015), Some(1));
        let mut peak: f32 = 0.0;
        for _ in 0..1000 {
            m.cpu_clock();
            peak = peak.max(m.audio_output());
        }
        assert!((peak - 0.1494).abs() < 0.01, "{peak}");
        m.cpu_write(0x5015, 0);
        m.cpu_write(0x5011, 100);
        assert!((m.audio_output() - 0.16).abs() < 0.01);
    }
}