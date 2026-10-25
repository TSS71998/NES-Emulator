//! Background: every 8 dots the PPU fetches a nametable byte, an attribute
//! byte and the two pattern bytes of the next tile, then loads them into
//! 16-bit shift registers that shift once per dot. The pixel at screen x
//! comes out on dot x + 1; fine X picks which bit of the shifters is used.
//!
//! Sprites: during dots 1-256 the PPU finds the (up to 8) sprites on the
//! next line, and during dots 257-320 loads them into its 8 sprite units:
//! each has an attribute latch, an X counter and two 8-bit pattern shift
//! registers. Empty slots fetch tile $FF, so the pattern table is read 8
//! times per line either way. That is where MMC3 sees A12 go high when
//! sprites use $1000. On the next line, each dot every unit's counter counts
//! down, and once it reaches 0 the unit's shift registers output a pixel
//! and shift. The counters keep running with rendering off; the shift
//! registers don't, so turning rendering off and on mid-line can draw stale
//! sprite data.

use super::{NesPPU, palette_index};
use crate::region::Region;
use crate::state::{StateReader, StateWriter};

#[derive(Default)]
pub(super) struct Background {
    next_tile: u8,
    next_palette: u8,
    next_low: u8,
    next_high: u8,
    pattern_low: u16,
    pattern_high: u16,
    attribute_low: u16,
    attribute_high: u16,
    /// The current tile's palette, which the attribute shifters shift in.
    attribute_latch: u8,
    /// A tile's high pattern byte was fetched, so the shifters take the new
    /// tile on the next dot. (If rendering was off for that fetch, they
    /// don't.)
    load_pending: bool,
}

/// One of the 8 sprite output units.
#[derive(Clone, Copy, Default)]
struct SpriteUnit {
    low: u8,
    high: u8,
    attr: u8,
    /// Dots until the sprite starts: its X position, counted down.
    counter: u8,
}

pub(super) struct Sprites {
    secondary: [u8; 32],
    /// Whether the first sprite found by the evaluation (on dot 66) was in
    /// range: "sprite 0" for the next line.
    sprite_zero_found: bool,
    /// Whether unit 0 holds that sprite 0, for this line's sprite 0 hits.
    sprite_zero_loaded: bool,
    /// The byte last read from OAM or secondary OAM; $2004 returns it while
    /// the PPU is rendering.
    pub(super) buffer: u8,
    /// The secondary OAM ("OAM2") address, 0-31. When it wraps (secondary
    /// OAM full) `secondary_full` is raised, and freezes it at 0 until the
    /// reset signal: rendering on during dot 63, 255 or 339 resets both on
    /// the next dot.
    secondary_addr: u8,
    secondary_full: bool,
    secondary_reset: bool,
    /// The sprite whose unit is being loaded: its Y and tile.
    fetch_y: u8,
    fetch_tile: u8,
    /// Evaluation state: whether the sprite just read is being copied,
    /// whether all 64 sprites have been looked at, and the 3 extra reads
    /// after an overflow.
    copying: bool,
    done: bool,
    overflow_reads: u8,
    units: [SpriteUnit; 8],
    /// Bit n set: unit n may still have pattern bits to shift out. A unit
    /// whose shift registers are empty can't draw, and shifting it changes
    /// nothing, so those units are skipped (its counter still runs).
    drawing: u8,
}

impl Default for Sprites {
    fn default() -> Self {
        Sprites {
            secondary: [0xFF; 32],
            sprite_zero_found: false,
            sprite_zero_loaded: false,
            buffer: 0xFF,
            secondary_addr: 0,
            secondary_full: false,
            secondary_reset: false,
            fetch_y: 0xFF,
            fetch_tile: 0xFF,
            copying: false,
            done: false,
            overflow_reads: 0,
            units: [SpriteUnit::default(); 8],
            drawing: 0,
        }
    }
}

impl Sprites {
    /// Updates unit `slot`'s bit in `drawing` from its shift registers.
    fn update_drawing(&mut self, slot: usize) {
        let unit = &self.units[slot];
        if unit.low | unit.high != 0 {
            self.drawing |= 1 << slot;
        } else {
            self.drawing &= !(1 << slot);
        }
    }

    /// Moves the secondary OAM address on, unless it's frozen.
    fn increment_secondary(&mut self) {
        if !self.secondary_full {
            self.secondary_addr += 1;
            if self.secondary_addr == 0x20 {
                self.secondary_addr = 0;
                self.secondary_full = true;
            }
        }
    }
}

impl NesPPU {
    pub(super) fn render_dot(&mut self) {
        let dot = self.dot;
        let visible = self.scanline < 240;
        // Shifting and pixel output follow the rendering flag; the memory
        // fetches and sprite evaluation follow it a dot later. So right
        // after rendering comes back on the shifters move for a dot before a
        // fetch can reload them.
        let rendering = self.rendering_enabled();
        let fetching = self.fetching;

        if rendering {
            if let Some(row) = self.oam_corruption.take() {
                self.corrupt_oam(row);
            }
        }

        if rendering
            && !visible
            && (1..=8).contains(&dot)
            && self.oam_addr >= 8
            && self.region != Region::Pal
        {
            // The 2C02 bug: when rendering starts, the OAM row (8 bytes)
            // OAMADDR points to is copied over row 0, so if OAMADDR is 8 or
            // more, sprites 0 and 1 are overwritten. Reliable on the common
            // 2C02 revisions (C, E, G, H) and the Dendy; the PAL 2C07
            // doesn't do it.
            let i = dot as usize - 1;
            self.oam_data[i] = self.oam_data[(self.oam_addr & 0xF8) as usize + i];
        }

        // the shifters move on dots 2-257 and 322-337
        if rendering && ((2..=257).contains(&dot) || (322..=337).contains(&dot)) {
            self.shift_background();
        }
        if (1..=256).contains(&dot) {
            if visible {
                if rendering {
                    self.output_pixel();
                } else {
                    self.output_backdrop();
                }
            }
            // the sprite counters run even with rendering off
            self.clock_sprite_units(rendering);
        }
        if dot == 339 && !rendering {
            // with rendering off on dot 339, every sprite starts at X = 0
            for unit in self.sprites.units.iter_mut() {
                unit.counter = 0;
            }
        }

        if self.bg.load_pending {
            self.load_shifters();
        }
        if fetching {
            if visible && (1..=256).contains(&dot) {
                self.evaluate_sprites();
            }
            if dot >= 321 {
                self.sprites.buffer = self.sprites.secondary[self.sprites.secondary_addr as usize];
            }
            match dot {
                1..=256 | 321..=336 => self.fetch_background(),
                257..=320 => {
                    if dot == 257 {
                        self.sprites.sprite_zero_loaded = self.sprites.sprite_zero_found;
                    }
                    // OAMADDR is held at 0 while sprites are fetched
                    self.oam_addr = 0;
                    self.fetch_sprites();
                }
                // two unused nametable fetches end the line (MMC5 counts them)
                337 | 339 => self.bus_latch(self.nametable_addr()),
                338 | 340 => {
                    let addr = self.nametable_addr();
                    self.bus_read(addr);
                }
                _ => {}
            }
        }

        let sprites = &mut self.sprites;
        if sprites.secondary_reset {
            sprites.secondary_reset = false;
            sprites.secondary_addr = 0;
            sprites.secondary_full = false;
        }
        if fetching && matches!(dot, 63 | 255 | 339) {
            sprites.secondary_reset = true;
        }

        // the scroll updates need rendering on for a dot already: turned on
        // just at dot 256, it doesn't move to the next row
        if rendering && fetching {
            if dot == 256 {
                self.increment_y();
            } else if dot == 257 {
                self.copy_horizontal();
            }
            if self.scanline == self.prerender_line && (280..=304).contains(&dot) {
                self.copy_vertical();
            }
        }
    }

    /// Rendering was turned off on a rendering line. Secondary OAM's
    /// address at that moment picks an OAM row (8 bytes) to corrupt when
    /// rendering next runs; during evaluation (dots 65-256) it's rounded up
    /// to a whole sprite.
    pub(super) fn seed_oam_corruption(&mut self) {
        if self.oam_corruption.is_some() {
            return;
        }
        let mut row = self.sprites.secondary_addr;
        if (65..=256).contains(&self.dot) && row & 3 != 0 {
            row = ((row & !3) + 4) & 0x1F;
        }
        self.oam_corruption = Some(row);
    }

    /// OAM corruption: on the first rendering dot after rendering was
    /// turned off mid-line, OAM row 0 is copied over the seeded row, and
    /// secondary OAM's byte 0 over the seeded byte (AccuracyCoin "OAM
    /// Corruption").
    fn corrupt_oam(&mut self, row: u8) {
        let row = row as usize;
        for i in 0..8 {
            self.oam_data[row * 8 + i] = self.oam_data[i];
        }
        self.sprites.secondary[row] = self.sprites.secondary[0];
    }

    /// What comes in at the bottom: 0 for the low pattern plane, 1 for the
    /// high one, and the attribute latch's bits for the palette. Normally
    /// it's overwritten by the next tile before it's drawn, but a shifter
    /// that isn't reloaded (rendering turned off and on at the right time)
    /// draws it (AccuracyCoin "BG Serial In").
    fn shift_background(&mut self) {
        let bg = &mut self.bg;
        bg.pattern_low <<= 1;
        bg.pattern_high = bg.pattern_high << 1 | 1;
        bg.attribute_low = bg.attribute_low << 1 | (bg.attribute_latch & 1) as u16;
        bg.attribute_high = bg.attribute_high << 1 | (bg.attribute_latch >> 1) as u16;
    }

    /// Moves the fetched tile into the low byte of the shifters.
    fn load_shifters(&mut self) {
        let bg = &mut self.bg;
        bg.load_pending = false;
        bg.pattern_low = (bg.pattern_low & 0xFF00) | bg.next_low as u16;
        bg.pattern_high = (bg.pattern_high & 0xFF00) | bg.next_high as u16;
        let spread = |bit: u8| if bit != 0 { 0xFF } else { 0x00 };
        bg.attribute_low = (bg.attribute_low & 0xFF00) | spread(bg.next_palette & 1);
        bg.attribute_high = (bg.attribute_high & 0xFF00) | spread(bg.next_palette & 2);
        bg.attribute_latch = bg.next_palette;
    }

    /// One step of the 8-dot tile fetch: four 2-dot memory accesses (see
    /// `bus_latch`/`bus_read`), nametable read on dot 2, attribute on 4,
    /// pattern low on 6, pattern high on 8, after which coarse X moves on.
    /// The shifters take the new tile on the next dot (9, 17, ...).
    fn fetch_background(&mut self) {
        match self.dot & 7 {
            1 => self.bus_latch(self.nametable_addr()),
            2 => {
                let addr = self.nametable_addr();
                self.bg.next_tile = self.bus_read(addr);
            }
            3 => self.bus_latch(self.attribute_addr()),
            4 => {
                let addr = self.attribute_addr();
                let shift = ((self.v >> 4) & 4) | (self.v & 2);
                self.bg.next_palette = (self.bus_read(addr) >> shift) & 0b11;
            }
            5 => self.bus_latch(self.background_pattern_addr()),
            6 => {
                let addr = self.background_pattern_addr();
                self.bg.next_low = self.bus_read(addr);
            }
            7 => self.bus_latch(self.background_pattern_addr() + 8),
            _ => {
                let addr = self.background_pattern_addr() + 8;
                self.bg.next_high = self.bus_read(addr);
                self.bg.load_pending = true;
                self.increment_coarse_x();
            }
        }
    }

    fn nametable_addr(&self) -> u16 {
        0x2000 | (self.v & 0x0FFF)
    }

    fn attribute_addr(&self) -> u16 {
        let v = self.v;
        0x23C0 | (v & 0x0C00) | ((v >> 4) & 0x38) | ((v >> 2) & 0x07)
    }

    fn background_pattern_addr(&self) -> u16 {
        let fine_y = (self.v >> 12) & 0b111;
        self.ctrl.bknd_pattern_addr() + self.bg.next_tile as u16 * 16 + fine_y
    }

    /// The first dot of a memory access (ALE): `addr` goes on the bus and
    /// its low byte into the octal latch. If the $2007 state machine reads
    /// on this dot, the two collide: the latch takes the data still on the
    /// low address lines from the last read, and the state machine reads
    /// from the new high byte and that latch, feeding the result back into
    /// the latch (AccuracyCoin "ALE + Read").
    pub(super) fn bus_latch(&mut self, addr: u16) {
        self.bus_used = true;
        if self.data_read {
            self.octal_latch = self.addr_bus as u8;
            self.addr_bus = addr & 0x3FFF;
            let data = self.memory_read((self.addr_bus & 0x3F00) | self.octal_latch as u16);
            self.addr_bus = (self.addr_bus & 0x3F00) | data as u16;
            self.internal_data_buf = data;
            self.octal_latch = data;
        } else {
            self.addr_bus = addr & 0x3FFF;
            self.octal_latch = addr as u8;
        }
    }

    /// The second dot of a memory access: reads from the high byte of
    /// `addr`, worked out again now, and the low byte latched a dot ago, so
    /// if v changed in between ($2006) the read is from a "hybrid" address.
    /// A $2007 read on the same dot gets the same byte.
    pub(super) fn bus_read(&mut self, addr: u16) -> u8 {
        self.bus_used = true;
        let addr = (addr & 0x3F00) | self.octal_latch as u16;
        self.bus_address(addr);
        let data = self.memory_read(addr);
        self.addr_bus = (addr & 0x3F00) | data as u16;
        if self.data_read {
            self.internal_data_buf = data;
        }
        data
    }

    pub(super) fn increment_coarse_x(&mut self) {
        if self.v & 0x001F == 31 {
            self.v &= !0x001F;
            self.v ^= 0x0400; // next horizontal nametable
        } else {
            self.v += 1;
        }
    }

    pub(super) fn increment_y(&mut self) {
        if self.v & 0x7000 != 0x7000 {
            self.v += 0x1000; // fine Y
            return;
        }
        self.v &= !0x7000;
        let mut coarse_y = (self.v & 0x03E0) >> 5;
        if coarse_y == 29 {
            coarse_y = 0;
            self.v ^= 0x0800; // next vertical nametable
        } else if coarse_y == 31 {
            coarse_y = 0; // rows 30-31 are attribute data; wraps without switching
        } else {
            coarse_y += 1;
        }
        self.v = (self.v & !0x03E0) | (coarse_y << 5);
    }

    fn copy_horizontal(&mut self) {
        self.v = (self.v & !0x041F) | (self.t & 0x041F);
    }

    fn copy_vertical(&mut self) {
        self.v = (self.v & !0x7BE0) | (self.t & 0x7BE0);
    }

    /// One dot of sprite evaluation (dots 1-256 of a visible line), which
    /// finds the first 8 sprites in OAM that cover the next line.
    ///
    /// Dots 1-64 fill secondary OAM with $FF. From dot 65 the PPU reads
    /// OAM[OAMADDR] on odd dots, and on even dots copies the byte into
    /// secondary OAM and moves OAMADDR on. It starts at whatever OAMADDR is,
    /// and the moves are plain additions to the 8-bit address, so with
    /// OAMADDR not a multiple of 4 ("misaligned") sprites overlap, as on
    /// hardware (https://github.com/100thCoin/AccuracyCoin, "Misaligned OAM
    /// Behavior"):
    ///
    /// - a Y byte in range starts a copy: OAMADDR + 1 for each of the 4 bytes.
    ///   The X byte gets the same in-range test as Y; if it fails, OAMADDR is
    ///   realigned (+1 then & $FC).
    /// - a Y byte not in range skips to the next sprite: + 4, then & $FC.
    /// - the first sprite found (on dot 66) is "sprite 0" for the next line,
    ///   whichever OAM entry it is.
    ///
    /// Once 8 sprites are found it keeps looking for a 9th to set the
    /// overflow flag, but with the hardware bug: after each sprite not in
    /// range the sprite and byte counters both move on (n + 1, m + 1, no
    /// carry), so it compares tiles, attributes or X positions against the
    /// line. Evaluation stops when OAMADDR wraps past the last sprite.
    fn evaluate_sprites(&mut self) {
        let dot = self.dot;
        let height = self.ctrl.sprite_size() as u16;
        let line = self.scanline;
        let sprites = &mut self.sprites;

        if dot <= 64 {
            // odd dots read $FF, even dots write it to secondary OAM
            if dot & 1 == 1 {
                sprites.buffer = 0xFF;
                if dot == 1 {
                    sprites.secondary_addr = 0;
                }
            } else {
                sprites.secondary[sprites.secondary_addr as usize] = 0xFF;
                sprites.increment_secondary();
            }
            return;
        }

        if dot == 65 {
            sprites.copying = false;
            sprites.done = false;
            sprites.overflow_reads = 0;
            sprites.sprite_zero_found = false;
        }

        if dot & 1 == 1 {
            sprites.buffer = self.oam_data[self.oam_addr as usize];
            return;
        }

        let in_range = |y: u8| line >= y as u16 && line < y as u16 + height;
        let value = sprites.buffer;
        let mut addr = self.oam_addr as u16;
        let mut overflow = false;

        if sprites.done {
            // all 64 looked at: OAMADDR keeps moving, and the even dots read
            // secondary OAM instead of writing it
            addr = (addr + 4) & !3;
            sprites.buffer = sprites.secondary[sprites.secondary_addr as usize];
        } else if !sprites.secondary_full {
            sprites.secondary[sprites.secondary_addr as usize] = value;
            if !sprites.copying {
                if in_range(value) {
                    sprites.copying = true;
                    sprites.sprite_zero_found |= dot == 66;
                    sprites.increment_secondary();
                    addr += 1;
                } else {
                    addr = (addr + 4) & !3;
                }
            } else {
                sprites.increment_secondary();
                if sprites.secondary_addr & 3 == 0 {
                    sprites.copying = false;
                    addr = if in_range(value) {
                        addr + 1
                    } else {
                        (addr + 1) & !3
                    };
                } else {
                    addr += 1;
                }
            }
        } else {
            sprites.buffer = sprites.secondary[sprites.secondary_addr as usize];
            if sprites.overflow_reads > 0 {
                addr += 1;
                sprites.overflow_reads -= 1;
                if sprites.overflow_reads == 0 {
                    sprites.done = true;
                    addr &= !3;
                }
            } else if in_range(value) {
                overflow = true;
                sprites.overflow_reads = 3;
                addr += 1;
            } else {
                addr = ((addr & !3) + 4) | ((addr + 1) & 3);
            }
        }

        if addr > 0xFF {
            sprites.done = true;
        }
        self.oam_addr = addr as u8;

        if overflow {
            self.status.set_sprite_overflow(true);
        }
    }

    /// Dots 257-320: 8 dots per sprite unit, reading its secondary OAM
    /// entry through the secondary OAM address, one byte a dot (the address
    /// moves on dots 1-3 and 8 of the 8): Y, tile, attribute and X, latched
    /// on dots 2-5. Meanwhile the bus does two garbage nametable fetches and
    /// the two pattern fetches (read on dots 6 and 8). A sprite whose Y isn't
    /// in range of the line (including empty slots, Y = $FF) still fetches
    /// its pattern but loads 0s. The pre-render line doesn't evaluate
    /// sprites, so it loads whatever secondary OAM was left with, against
    /// line 261 & $FF. If the address is frozen (secondary OAM was full and
    /// rendering was off for the reset on dot 255) every byte is entry 0's
    /// (AccuracyCoin "Frozen OAM2 Increment").
    fn fetch_sprites(&mut self) {
        let offset = self.dot - 257;
        let slot = (offset / 8) as usize;
        let latched = self.sprites.buffer;
        self.sprites.buffer = self.sprites.secondary[self.sprites.secondary_addr as usize];
        match offset & 7 {
            0 => {
                self.bus_latch(self.nametable_addr());
                self.sprites.increment_secondary();
            }
            1 => {
                self.sprites.fetch_y = latched;
                let addr = self.nametable_addr();
                self.bus_read(addr);
                self.sprites.increment_secondary();
            }
            2 => {
                self.sprites.fetch_tile = latched;
                self.bus_latch(self.nametable_addr());
                self.sprites.increment_secondary();
            }
            3 => {
                self.sprites.units[slot].attr = latched;
                let addr = self.nametable_addr();
                self.bus_read(addr);
            }
            4 => {
                self.sprites.units[slot].counter = latched;
                self.bus_latch(self.sprite_fetch_addr(slot));
            }
            5 => {
                let addr = self.sprite_fetch_addr(slot);
                let byte = self.bus_read(addr);
                self.sprites.units[slot].low = self.sprite_pattern(slot, byte);
                self.sprites.update_drawing(slot);
            }
            6 => self.bus_latch(self.sprite_fetch_addr(slot) + 8),
            _ => {
                let addr = self.sprite_fetch_addr(slot) + 8;
                let byte = self.bus_read(addr);
                self.sprites.units[slot].high = self.sprite_pattern(slot, byte);
                self.sprites.update_drawing(slot);
                self.sprites.increment_secondary();
            }
        }
    }

    /// The row of the sprite being fetched on this line, if it's in range.
    fn sprite_row(&self) -> Option<u16> {
        let y = self.sprites.fetch_y as u16;
        let line = self.scanline & 0xFF;
        let height = self.ctrl.sprite_size() as u16;
        (line >= y && line - y < height).then(|| line - y)
    }

    fn sprite_fetch_addr(&self, slot: usize) -> u16 {
        let attr = self.sprites.units[slot].attr;
        let height = self.ctrl.sprite_size() as u16;
        let y = self.sprites.fetch_y as u16;
        let mut row = (self.scanline & 0xFF).wrapping_sub(y) & (height - 1);
        if attr & 0x80 != 0 {
            row = height - 1 - row;
        }
        self.sprite_row_addr(self.sprites.fetch_tile, row as usize)
    }

    /// One of a sprite's pattern bytes as loaded into its unit: flipped for
    /// horizontal flip, or 0 if the sprite isn't on this line.
    fn sprite_pattern(&self, slot: usize, byte: u8) -> u8 {
        if self.sprite_row().is_none() {
            0
        } else if self.sprites.units[slot].attr & 0x40 != 0 {
            byte.reverse_bits()
        } else {
            byte
        }
    }

    /// Dots 1-256: each unit counts down to its sprite's X position, then
    /// shifts its pattern out (only while rendering).
    fn clock_sprite_units(&mut self, rendering: bool) {
        let sprites = &mut self.sprites;
        for (i, unit) in sprites.units.iter_mut().enumerate() {
            if unit.counter > 0 {
                unit.counter -= 1;
            } else if rendering && sprites.drawing & (1 << i) != 0 {
                unit.low <<= 1;
                unit.high <<= 1;
                if unit.low | unit.high == 0 {
                    sprites.drawing &= !(1 << i);
                }
            }
        }
    }

    /// The first opaque pixel among the sprite units that have started:
    /// (unit, value, attributes).
    fn sprite_pixel(&self) -> Option<(usize, u8, u8)> {
        if self.sprites.drawing == 0 {
            return None;
        }
        self.sprites.units.iter().enumerate().find_map(|(i, unit)| {
            let value = (unit.high >> 7) << 1 | unit.low >> 7;
            (unit.counter == 0 && value != 0).then_some((i, value, unit.attr))
        })
    }

    /// Mixes the background and sprite pixel for this dot, sets sprite 0
    /// hit, and writes the colour to the frame.
    fn output_pixel(&mut self) {
        let x = (self.dot - 1) as usize;
        let mut bg_value = 0;
        let mut bg_palette = 0;
        if self.mask.show_background() && (x >= 8 || self.mask.leftmost_8pxl_background()) {
            let bit = 15 - self.fine_x;
            let bg = &self.bg;
            bg_value = (((bg.pattern_high >> bit) & 1) << 1 | ((bg.pattern_low >> bit) & 1)) as u8;
            bg_palette =
                (((bg.attribute_high >> bit) & 1) << 1 | ((bg.attribute_low >> bit) & 1)) as u8;
        }

        let mut sprite = None;
        if self.mask.show_sprites() && (x >= 8 || self.mask.leftmost_8pxl_sprite()) {
            sprite = self.sprite_pixel();
        }

        if let Some((unit, _, _)) = sprite {
            if unit == 0 && self.sprites.sprite_zero_loaded && bg_value != 0 && x != 255 {
                self.status.set_sprite_zero_hit(true);
            }
        }

        let palette_addr = match (bg_value, sprite) {
            (0, None) => 0,
            (0, Some((_, s, attr))) => 0x10 | (attr & 3) << 2 | s,
            (b, None) => bg_palette << 2 | b,
            (b, Some((_, s, attr))) => {
                if attr & 0x20 != 0 {
                    bg_palette << 2 | b
                } else {
                    0x10 | (attr & 3) << 2 | s
                }
            }
        };
        let colour = self.palette_table[palette_index(palette_addr as u16)];
        self.put_pixel(x, colour);
    }

    /// With rendering off the screen shows the backdrop colour, or, if v
    /// points into palette RAM, the colour there.
    fn output_backdrop(&mut self) {
        let x = (self.dot - 1) as usize;
        let v = self.v & 0x3FFF;
        let colour = if v >= 0x3F00 {
            self.palette_table[palette_index(v)]
        } else {
            self.palette_table[0]
        };
        self.put_pixel(x, colour);
    }

    /// Writes a palette colour to the frame, with PPUMASK's greyscale and
    /// emphasis applied.
    fn put_pixel(&mut self, x: usize, color: u8) {
        let rgb = self.palette[self.emphasis_offset | (color & self.greyscale_mask) as usize];
        let i = (self.scanline as usize * 256 + x) * 3;
        self.frame.data[i] = rgb.0;
        self.frame.data[i + 1] = rgb.1;
        self.frame.data[i + 2] = rgb.2;
    }
}

/// Save states (see `state.rs`).
impl Background {
    pub(super) fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.next_tile);
        w.u8(self.next_palette);
        w.u8(self.next_low);
        w.u8(self.next_high);
        w.u16(self.pattern_low);
        w.u16(self.pattern_high);
        w.u16(self.attribute_low);
        w.u16(self.attribute_high);
        w.u8(self.attribute_latch);
        w.bool(self.load_pending);
    }

    pub(super) fn load_state(&mut self, r: &mut StateReader) {
        self.next_tile = r.u8();
        self.next_palette = r.u8();
        self.next_low = r.u8();
        self.next_high = r.u8();
        self.pattern_low = r.u16();
        self.pattern_high = r.u16();
        self.attribute_low = r.u16();
        self.attribute_high = r.u16();
        self.attribute_latch = r.u8();
        self.load_pending = r.bool();
    }
}

impl Sprites {
    pub(super) fn save_state(&self, w: &mut StateWriter) {
        w.bytes(&self.secondary);
        w.bool(self.sprite_zero_found);
        w.bool(self.sprite_zero_loaded);
        w.u8(self.buffer);
        w.u8(self.secondary_addr);
        w.bool(self.secondary_full);
        w.bool(self.secondary_reset);
        w.u8(self.fetch_y);
        w.u8(self.fetch_tile);
        w.bool(self.copying);
        w.bool(self.done);
        w.u8(self.overflow_reads);
        for unit in &self.units {
            w.u8(unit.low);
            w.u8(unit.high);
            w.u8(unit.attr);
            w.u8(unit.counter);
        }
        w.u8(self.drawing);
    }

    pub(super) fn load_state(&mut self, r: &mut StateReader) {
        r.bytes_into(&mut self.secondary);
        self.sprite_zero_found = r.bool();
        self.sprite_zero_loaded = r.bool();
        self.buffer = r.u8();
        self.secondary_addr = r.u8() & 0x1F;
        self.secondary_full = r.bool();
        self.secondary_reset = r.bool();
        self.fetch_y = r.u8();
        self.fetch_tile = r.u8();
        self.copying = r.bool();
        self.done = r.bool();
        self.overflow_reads = r.u8();
        for unit in &mut self.units {
            unit.low = r.u8();
            unit.high = r.u8();
            unit.attr = r.u8();
            unit.counter = r.u8();
        }
        self.drawing = r.u8();
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::ppu::PPU;
    use crate::render::palette::{NTSC_PALETTE, PAL_PALETTE, SYSTEM_PALETTE};

    const RED: u8 = 0x16;
    const GREEN: u8 = 0x2A;
    const BLUE: u8 = 0x12;

    /// CHR-RAM tile 1 is solid colour 1; everything else is blank.
    fn test_ppu() -> NesPPU {
        use crate::cartridge::Mirroring;
        use crate::mapper::{Mapper, Nrom};

        let mut cart = Nrom::from_parts(vec![0; 0x4000], vec![], Mirroring::Vertical);
        for row in 0..8 {
            cart.ppu_write(0x10 + row, 0xFF);
        }

        let mut ppu = NesPPU::new(std::rc::Rc::new(std::cell::RefCell::new(cart)));
        ppu.oam_data = [0xFF; 256]; // all sprites below the screen
        ppu.palette_table[0x00] = 0x0F; // backdrop: black
        ppu.palette_table[0x01] = GREEN; // background colour 1
        ppu.palette_table[0x11] = RED; // sprite palette 0, colour 1
        ppu.palette_table[0x15] = BLUE; // sprite palette 1, colour 1
        ppu.write_to_mask(0b0001_1110); // background + sprites, no left clipping
        ppu
    }

    fn sprite(ppu: &mut NesPPU, n: usize, x: u8, y: u8, attr: u8) {
        ppu.oam_data[n * 4..n * 4 + 4].copy_from_slice(&[y, 1, attr, x]);
    }

    fn run_frame(ppu: &mut NesPPU) {
        while !ppu.tick(1) {}
    }

    /// Runs two frames (the first one after a change can start with old
    /// scroll values, as the pre-render line loads them) and reads a pixel.
    fn pixel(ppu: &mut NesPPU, x: usize, y: usize) -> (u8, u8, u8) {
        run_frame(ppu);
        run_frame(ppu);
        let i = (y * 256 + x) * 3;
        (
            ppu.frame.data[i],
            ppu.frame.data[i + 1],
            ppu.frame.data[i + 2],
        )
    }

    fn colour(index: u8) -> (u8, u8, u8) {
        SYSTEM_PALETTE[index as usize]
    }

    #[test]
    fn test_rendering_disabled_shows_backdrop() {
        let mut ppu = test_ppu();
        sprite(&mut ppu, 0, 16, 9, 0);
        ppu.write_to_mask(0);
        assert_eq!(pixel(&mut ppu, 20, 10), colour(0x0F));
    }

    #[test]
    fn test_left_column_clipping() {
        let mut ppu = test_ppu();
        sprite(&mut ppu, 0, 0, 9, 0);
        assert_eq!(pixel(&mut ppu, 3, 10), colour(RED));
        ppu.write_to_mask(0b0001_1000); // clip both in the left 8 pixels
        assert_eq!(pixel(&mut ppu, 3, 10), colour(0x0F));
    }

    #[test]
    fn test_colour_emphasis() {
        let mut ppu = test_ppu();
        ppu.vram[12 * 32 + 12] = 1; // opaque background tile at x 96-103, y 96-103
        sprite(&mut ppu, 0, 20, 9, 0);
        ppu.write_to_mask(0b0011_1110); // emphasise red
        assert_eq!(
            pixel(&mut ppu, 100, 100),
            NTSC_PALETTE[1 << 6 | GREEN as usize]
        );
        assert_eq!(pixel(&mut ppu, 22, 10), NTSC_PALETTE[1 << 6 | RED as usize]);
        ppu.write_to_mask(0b1111_1111); // all three, greyscale on
        assert_eq!(
            pixel(&mut ppu, 100, 100),
            NTSC_PALETTE[7 << 6 | (GREEN & 0x30) as usize]
        );
        // with rendering off the backdrop is tinted too
        ppu.write_to_mask(0b1000_0000);
        assert_eq!(pixel(&mut ppu, 100, 100), NTSC_PALETTE[4 << 6 | 0x0F]);
    }

    #[test]
    fn test_pal_swaps_red_and_green_emphasis() {
        let mut ppu = test_ppu();
        ppu.vram[12 * 32 + 12] = 1; // opaque background tile at x 96-103, y 96-103
        ppu.set_region(Region::Pal);
        ppu.write_to_mask(0b0011_1110); // bit 5: green on PAL
        assert_eq!(
            pixel(&mut ppu, 100, 100),
            PAL_PALETTE[2 << 6 | GREEN as usize]
        );
        ppu.write_to_mask(0b0101_1110); // bit 6: red on PAL
        assert_eq!(
            pixel(&mut ppu, 100, 100),
            PAL_PALETTE[1 << 6 | GREEN as usize]
        );
    }

    #[test]
    fn test_lower_oam_index_wins_even_when_behind_background() {
        let mut ppu = test_ppu();
        ppu.vram[32 + 2] = 1; // opaque background tile at x 16-23, y 8-15
        sprite(&mut ppu, 0, 16, 9, 0b0010_0000); // behind background, red
        sprite(&mut ppu, 1, 16, 9, 0b0000_0001); // in front, blue
        // sprite 0 wins the pixel, then hides behind the background
        assert_eq!(pixel(&mut ppu, 20, 10), colour(GREEN));
        // where the background is transparent, sprite 0 shows
        ppu.vram[32 + 2] = 0;
        assert_eq!(pixel(&mut ppu, 20, 10), colour(RED));
    }

    #[test]
    fn test_only_eight_sprites_per_line_and_overflow() {
        let mut ppu = test_ppu();
        for n in 0..9 {
            sprite(&mut ppu, n, n as u8 * 10, 49, 0);
        }
        run_frame(&mut ppu);
        assert_eq!(pixel(&mut ppu, 72, 50), colour(RED)); // 8th sprite
        assert_eq!(pixel(&mut ppu, 82, 50), colour(0x0F)); // 9th is dropped
        // the overflow flag is set mid-frame and cleared at the pre-render line
        while ppu.status.snapshot() & 0x20 == 0 {
            ppu.tick(1);
        }
        assert_eq!(ppu.scanline, 49);
    }

    /// Runs until the PPU has just emulated `dot` of `line`.
    fn run_to(ppu: &mut NesPPU, line: u16, dot: u16) {
        while (ppu.scanline, ppu.dot) != (line, dot) {
            ppu.tick(1);
        }
    }

    fn overflow(ppu: &NesPPU) -> bool {
        ppu.status.snapshot() & 0x20 != 0
    }

    #[test]
    fn test_overflow_is_set_on_the_dot_the_ninth_sprite_is_checked() {
        let mut ppu = test_ppu();
        for n in 0..9 {
            sprite(&mut ppu, n, n as u8 * 10, 49, 0);
        }
        run_frame(&mut ppu);
        // 8 sprites copied at 8 dots each (65-128), then the 9th sprite's Y
        // is read on dot 129 and found in range on dot 130
        run_to(&mut ppu, 49, 129);
        assert!(!overflow(&ppu));
        ppu.tick(1);
        assert!(overflow(&ppu));
    }

    #[test]
    fn test_overflow_bug_checks_the_wrong_byte() {
        // after 8 sprites, a sprite not in range makes the PPU check byte 1
        // (the tile) of the next one, then byte 2 of the one after...
        let mut ppu = test_ppu();
        for n in 0..8 {
            sprite(&mut ppu, n, n as u8 * 10, 49, 0);
        }
        ppu.oam_data[8 * 4..8 * 4 + 4].copy_from_slice(&[200, 1, 0, 0]); // not in range
        ppu.oam_data[9 * 4..9 * 4 + 4].copy_from_slice(&[200, 45, 0, 0]); // tile 45 "is in range"
        run_frame(&mut ppu);
        run_to(&mut ppu, 49, 256);
        assert!(overflow(&ppu), "a false overflow");

        // ...so a real 9th sprite there is missed
        let mut ppu = test_ppu();
        for n in 0..8 {
            sprite(&mut ppu, n, n as u8 * 10, 49, 0);
        }
        ppu.oam_data[8 * 4..8 * 4 + 4].copy_from_slice(&[200, 1, 0, 0]);
        ppu.oam_data[9 * 4..9 * 4 + 4].copy_from_slice(&[49, 1, 0, 90]); // in range, but tile 1 is checked
        run_frame(&mut ppu);
        run_to(&mut ppu, 49, 256);
        assert!(!overflow(&ppu), "the 10th sprite is missed");
    }

    #[test]
    fn test_evaluation_starts_at_oamaddr() {
        let mut ppu = test_ppu();
        sprite(&mut ppu, 0, 10, 49, 0);
        sprite(&mut ppu, 1, 40, 49, 0);
        run_frame(&mut ppu);
        run_to(&mut ppu, 49, 64);
        ppu.oam_addr = 4; // as if the CPU wrote $2003 during the line
        run_to(&mut ppu, 51, 0);
        let at = |x: usize| {
            let i = (50 * 256 + x) * 3;
            (
                ppu.frame.data[i],
                ppu.frame.data[i + 1],
                ppu.frame.data[i + 2],
            )
        };
        assert_eq!(at(12), colour(0x0F), "sprite 0 is skipped");
        assert_eq!(at(42), colour(RED), "sprite 1 is found");
    }

    /// Writes sprites as raw bytes starting at OAM address `start`, so they
    /// can be misaligned.
    fn raw_sprites(ppu: &mut NesPPU, start: usize, bytes: &[u8]) {
        ppu.oam_data = [0xFF; 256];
        ppu.oam_data[start..start + bytes.len()].copy_from_slice(bytes);
    }

    /// Starts evaluation for line 49 at OAMADDR `start`, and returns the 8
    /// sprites it put in secondary OAM and whether it set the overflow flag.
    fn evaluate_from(ppu: &mut NesPPU, start: u8) -> ([u8; 32], bool) {
        run_frame(ppu);
        run_to(ppu, 49, 64);
        ppu.status.set_sprite_overflow(false);
        ppu.oam_addr = start;
        run_to(ppu, 49, 256);
        (ppu.sprites.secondary, overflow(ppu))
    }

    #[test]
    fn test_misaligned_oam_realigns_when_y_is_out_of_range() {
        // OAMADDR 1: sprites at 1, 5, ... $15 overlap by a byte (X stays in
        // range, so it stays misaligned), then $19 reads Y $80 (not in
        // range): the skip is +4 then & $FC, so the next Y is read at $1C.
        let mut ppu = test_ppu();
        let mut bytes = vec![];
        let mut expected = vec![];
        for i in 0..6 {
            bytes.extend_from_slice(&[49, 0x10 + i, 0, 49]);
            expected.extend_from_slice(&[49, 0x10 + i, 0, 49]);
        }
        bytes.extend_from_slice(&[0x80, 0xFF, 0xFF]); // $19-$1B
        for j in 0..3 {
            bytes.extend_from_slice(&[49, 0x20 + j, 0, 49]); // $1C, $20, $24
        }
        expected.extend_from_slice(&[49, 0x20, 0, 49, 49, 0x21, 0, 49]);
        raw_sprites(&mut ppu, 1, &bytes);
        let (secondary, overflow) = evaluate_from(&mut ppu, 1);
        assert_eq!(secondary[..], expected[..]);
        assert!(overflow, "the 9th sprite, at $24");
    }

    #[test]
    fn test_misaligned_oam_realigns_when_x_is_out_of_range() {
        // An X byte out of range moves OAMADDR + 1 then & $FC: the X at $1C
        // leads back to $1C, which is read as the next Y ($80, skipped), and
        // evaluation goes on aligned from $20.
        let mut ppu = test_ppu();
        let mut bytes = vec![];
        let mut expected = vec![];
        for i in 0..6 {
            bytes.extend_from_slice(&[49, 0x10 + i, 0, 49]);
            expected.extend_from_slice(&[49, 0x10 + i, 0, 49]);
        }
        bytes.extend_from_slice(&[49, 0x16, 0, 0x80]); // $19; X $80 at $1C
        expected.extend_from_slice(&[49, 0x16, 0, 0x80]);
        bytes.extend_from_slice(&[0xFF, 0xFF, 0xFF]); // $1D-$1F
        for j in 0..3 {
            bytes.extend_from_slice(&[49, 0x20 + j, 0, 49]); // $20, $24, $28
        }
        expected.extend_from_slice(&[49, 0x20, 0, 49]);
        raw_sprites(&mut ppu, 1, &bytes);
        let (secondary, overflow) = evaluate_from(&mut ppu, 1);
        assert_eq!(secondary[..], expected[..]);
        assert!(overflow, "the 9th sprite, at $24");
    }

    #[test]
    fn test_first_sprite_found_is_sprite_zero() {
        // with OAMADDR at sprite 1 when evaluation starts, sprite 1 is the
        // one that sets the sprite 0 hit flag
        let mut ppu = test_ppu();
        ppu.vram[6 * 32 + 5] = 1; // background tile at x 40-47, y 48-55
        sprite(&mut ppu, 1, 40, 49, 0);
        run_frame(&mut ppu);
        run_to(&mut ppu, 49, 64);
        ppu.oam_addr = 4;
        run_to(&mut ppu, 51, 0);
        assert!(ppu.status.snapshot() & 0x40 != 0);
    }

    #[test]
    fn test_oam_data_during_rendering() {
        let mut ppu = test_ppu();
        sprite(&mut ppu, 0, 10, 49, 0);
        run_frame(&mut ppu);
        // dots 1-64 clear secondary OAM, and $2004 reads $FF
        run_to(&mut ppu, 49, 30);
        assert_eq!(ppu.read_oam_data(), 0xFF);
        // dots 257-320 read secondary OAM: slot 0 is sprite 0 (X = 10)
        run_to(&mut ppu, 49, 260);
        assert_eq!(ppu.read_oam_data(), 10);
        // a write doesn't reach OAM but moves OAMADDR to the next sprite
        run_to(&mut ppu, 49, 330);
        ppu.write_to_oam_data(0x77);
        assert_eq!(ppu.oam_addr, 4);
        assert_eq!(ppu.oam_data[0], 49);
        ppu.oam_addr = 0x05;
        ppu.write_to_oam_data(0x77);
        assert_eq!(ppu.oam_addr, 0x08);
        // in vblank OAM is free again
        run_to(&mut ppu, 245, 0);
        ppu.oam_addr = 0;
        assert_eq!(ppu.read_oam_data(), 49);
        ppu.write_to_oam_data(0x77);
        assert_eq!(ppu.oam_data[0], 0x77);
    }

    fn numbered_oam(ppu: &mut NesPPU) {
        for i in 0..256 {
            ppu.oam_data[i] = i as u8;
        }
    }

    #[test]
    fn test_oamaddr_of_8_or_more_when_rendering_starts_copies_a_row() {
        let mut ppu = test_ppu();
        run_frame(&mut ppu);
        numbered_oam(&mut ppu);
        run_to(&mut ppu, 245, 0);
        ppu.oam_addr = 0x2B; // left there by the CPU during vblank
        let prerender = ppu.prerender_line;
        run_to(&mut ppu, prerender, 9);
        let expected: Vec<u8> = (0x28..0x30).collect();
        assert_eq!(
            ppu.oam_data[0..8],
            expected[..],
            "bytes $28-$2F copied to 0-7"
        );
        assert_eq!(ppu.oam_data[8], 8, "nothing else changes");

        // the PAL PPU doesn't have the bug
        let mut ppu = test_ppu();
        ppu.set_region(Region::Pal);
        run_frame(&mut ppu);
        numbered_oam(&mut ppu);
        run_to(&mut ppu, 245, 0);
        ppu.oam_addr = 0x2B;
        let prerender = ppu.prerender_line;
        run_to(&mut ppu, prerender, 9);
        assert_eq!(ppu.oam_data[0], 0);
    }

    #[test]
    fn test_horizontal_scroll_with_fine_x() {
        let mut ppu = test_ppu();
        ppu.vram[2] = 1; // tile at x 16-23 of row 0
        ppu.write_to_scroll(5); // scroll right 5 pixels
        ppu.write_to_scroll(0);
        run_frame(&mut ppu);
        assert_eq!(pixel(&mut ppu, 10, 0), colour(0x0F));
        assert_eq!(pixel(&mut ppu, 11, 0), colour(GREEN));
        assert_eq!(pixel(&mut ppu, 18, 0), colour(GREEN));
        assert_eq!(pixel(&mut ppu, 19, 0), colour(0x0F));
    }

    #[test]
    fn test_vertical_scroll_wraps_into_next_nametable() {
        let mut ppu = test_ppu(); // vertical mirroring: $2800 shows $2000
        ppu.vram[0] = 1; // top-left tile of nametable 0
        ppu.write_to_scroll(0);
        ppu.write_to_scroll(236); // the tile row at y 0 shows at screen y 4
        assert_eq!(pixel(&mut ppu, 0, 3), colour(0x0F));
        assert_eq!(pixel(&mut ppu, 0, 4), colour(GREEN));
        assert_eq!(pixel(&mut ppu, 0, 11), colour(GREEN));
    }

    #[test]
    fn test_sprite_zero_hit_is_set_at_the_overlapping_dot() {
        let mut ppu = test_ppu();
        ppu.vram[5 * 32 + 10] = 1; // background tile at x 80-87, y 40-47
        sprite(&mut ppu, 0, 84, 43, 0); // sprite 0 at x 84, y 44
        run_frame(&mut ppu);
        while ppu.status.snapshot() & 0x40 == 0 {
            ppu.tick(1);
        }
        assert_eq!((ppu.scanline, ppu.dot), (44, 85)); // pixel 84 is output at dot 85
    }

    #[test]
    fn test_sprite_counters_run_while_rendering_is_off() {
        let mut ppu = test_ppu();
        ppu.sprites.units[0] = SpriteUnit {
            low: 0x80,
            high: 0x00,
            attr: 0,
            counter: 2,
        };
        ppu.sprites.drawing = 0b1;
        ppu.clock_sprite_units(false);
        ppu.clock_sprite_units(false);
        assert_eq!(ppu.sprites.units[0].counter, 0, "counted down anyway");
        ppu.clock_sprite_units(false);
        assert_eq!(ppu.sprites.units[0].low, 0x80, "but doesn't shift");
        assert_eq!(ppu.sprite_pixel(), Some((0, 1, 0)));
        ppu.clock_sprite_units(true);
        assert_eq!(ppu.sprites.units[0].low, 0x00);
    }

    #[test]
    fn test_rendering_off_on_dot_339_starts_every_sprite_at_x_0() {
        let mut ppu = test_ppu();
        run_frame(&mut ppu);
        run_to(&mut ppu, 10, 330);
        for unit in ppu.sprites.units.iter_mut() {
            unit.counter = 50;
        }
        ppu.write_to_mask(0);
        run_to(&mut ppu, 10, 340);
        assert!(ppu.sprites.units.iter().all(|u| u.counter == 0));
    }

    #[test]
    fn test_background_shifters_shift_in_ones_and_the_attribute_latch() {
        let mut ppu = test_ppu();
        ppu.bg.attribute_latch = 0b10;
        for _ in 0..3 {
            ppu.shift_background();
        }
        assert_eq!(ppu.bg.pattern_low, 0);
        assert_eq!(ppu.bg.pattern_high, 0b111, "the high plane shifts in 1s");
        assert_eq!(ppu.bg.attribute_low, 0);
        assert_eq!(ppu.bg.attribute_high, 0b111);
    }

    #[test]
    fn test_a_fetch_reads_from_the_latched_low_byte() {
        // the low byte is latched on the first dot of a fetch; the high
        // byte is worked out again on the second (a "hybrid" address if v
        // changed in between)
        let mut ppu = test_ppu();
        let hybrid = ppu.mirror_vram_addr(0x2C45) as usize;
        ppu.vram[hybrid] = 0x77;
        ppu.bus_latch(0x2345);
        assert_eq!(ppu.bus_read(0x2C00), 0x77);
    }

    #[test]
    fn test_ppu_data_read_during_rendering_gets_the_byte_rendering_reads() {
        let mut ppu = test_ppu();
        let addr = ppu.mirror_vram_addr(0x2123) as usize;
        ppu.vram[addr] = 0x5A;
        ppu.bus_latch(0x2123);
        ppu.data_read = true; // the $2007 state machine reads on this dot
        ppu.bus_read(0x2123);
        assert_eq!(ppu.internal_data_buf, 0x5A);
    }

    #[test]
    fn test_frozen_secondary_oam_address_loads_entry_0_into_every_unit() {
        let mut ppu = test_ppu();
        ppu.sprites.secondary = [0xFF; 32];
        ppu.sprites.secondary[..4].copy_from_slice(&[0xC1, 0x24, 0x00, 0x10]);
        ppu.sprites.secondary_addr = 0;
        ppu.sprites.secondary_full = true; // not reset on dot 255
        ppu.scanline = 196;
        for dot in 257..=320 {
            ppu.dot = dot;
            ppu.fetch_sprites();
        }
        for unit in ppu.sprites.units {
            assert_eq!((unit.attr, unit.counter), (0xC1, 0xC1));
        }

        // normally the address walks through all 8 entries
        ppu.sprites.secondary_full = false;
        for dot in 257..=320 {
            ppu.dot = dot;
            ppu.fetch_sprites();
        }
        assert_eq!(ppu.sprites.units[0].counter, 0x10);
        assert_eq!(ppu.sprites.units[1].counter, 0xFF);
        assert!(
            ppu.sprites.secondary_full,
            "wrapped at the end of the fetch"
        );
    }

    /// Turns rendering off around dot 8 of line 5, on again for a frame, and
    /// returns OAM.
    fn turn_rendering_off_mid_line(ppu: &mut NesPPU) -> [u8; 256] {
        run_frame(ppu);
        numbered_oam(ppu);
        run_to(ppu, 5, 6);
        ppu.write_to_mask(0);
        run_to(ppu, 20, 0);
        ppu.write_to_mask(0b0001_1110);
        run_frame(ppu);
        ppu.oam_data
    }

    #[test]
    fn test_turning_rendering_off_mid_line_corrupts_an_oam_row() {
        // secondary OAM's address (3 after dot 6 of the clear) picks the row
        // that row 0 is copied over once rendering is back on
        let oam = turn_rendering_off_mid_line(&mut test_ppu());
        assert_eq!(oam[24..32], [0, 1, 2, 3, 4, 5, 6, 7]);
        assert_eq!(oam[8..24], (8..24).collect::<Vec<u8>>()[..], "only one row");
        assert_eq!(oam[32], 32);

        let mut ppu = test_ppu();
        ppu.set_oam_row_corruption(false);
        let oam = turn_rendering_off_mid_line(&mut ppu);
        assert_eq!(oam[24..32], [24, 25, 26, 27, 28, 29, 30, 31]);
    }

    #[test]
    fn test_2002_sees_sprite_flags_late() {
        let mut ppu = test_ppu();
        ppu.write_to_mask(0);
        ppu.tick(5);
        // a sprite 0 hit shows 2 dots after the dot it happens on, an
        // overflow 1 dot after (set here as if on the next dot)
        ppu.status.set_sprite_zero_hit(true);
        ppu.status.set_sprite_overflow(true);
        ppu.tick(1);
        assert_eq!(ppu.late_sprite_flags(), 0x00);
        ppu.tick(1);
        assert_eq!(ppu.late_sprite_flags(), 0x20);
        ppu.tick(1);
        assert_eq!(ppu.late_sprite_flags(), 0x60);
    }
}
