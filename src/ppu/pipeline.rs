//! Background: every 8 dots the PPU fetches a nametable byte, an attribute
//! byte and the two pattern bytes of the next tile, then loads them into
//! 16-bit shift registers that shift once per dot. The pixel at screen x
//! comes out on dot x + 1; fine X picks which bit of the shifters is used.
//!
//! Sprites: at dot 257 the PPU finds the (up to 8) sprites on the next line,
//! and during dots 257-320 fetches their pattern rows. Empty slots fetch
//! tile $FF, so the pattern table is read 8 times per line either way.
//! That is where MMC3 sees A12 go high when sprites use $1000.

use super::{NesPPU, palette_index};
use crate::region::Region;
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
}

#[derive(Clone, Copy, Default)]
struct SpritePixel {
    value: u8,
    palette: u8,
    behind_background: bool,
    sprite_zero: bool,
}

pub(super) struct Sprites {
    secondary: [u8; 32],
    count: usize,
    sprite_zero_found: bool,
    /// The byte last read from OAM or secondary OAM; $2004 returns it while
    /// the PPU is rendering.
    pub(super) buffer: u8,
    /// Evaluation state: the next secondary OAM byte to write, the sprite
    /// (n) and byte (m) OAMADDR is on, whether the sprite just read is being
    /// copied, whether all 64 sprites have been looked at, and the 3 extra
    /// reads after an overflow.
    secondary_addr: u8,
    copying: bool,
    done: bool,
    overflow_reads: u8,
    line: [SpritePixel; 256],
    next_line: [SpritePixel; 256],
}

impl Default for Sprites {
    fn default() -> Self {
        Sprites {
            secondary: [0xFF; 32],
            count: 0,
            sprite_zero_found: false,
            buffer: 0xFF,
            secondary_addr: 0,
            copying: false,
            done: false,
            overflow_reads: 0,
            line: [SpritePixel::default(); 256],
            next_line: [SpritePixel::default(); 256],
        }
    }
}

impl NesPPU {
    pub(super) fn render_dot(&mut self) {
        let dot = self.dot;
        let visible = self.scanline < 240;

        if !self.rendering_enabled() {
            if visible && (1..=256).contains(&dot) {
                self.output_backdrop();
            }
            return;
        }

        if !visible && (1..=8).contains(&dot) && self.oam_addr >= 8 && self.region != Region::Pal {
            // The 2C02 bug: when rendering starts, the OAM row (8 bytes)
            // OAMADDR points to is copied over row 0, so if OAMADDR is 8 or
            // more, sprites 0 and 1 are overwritten. Reliable on the common
            // 2C02 revisions (C, E, G, H) and the Dendy; the PAL 2C07
            // doesn't do it.
            let i = dot as usize - 1;
            self.oam_data[i] = self.oam_data[(self.oam_addr & 0xF8) as usize + i];
        }

        // the shifters move on dots 2-257 and 322-337
        if (2..=257).contains(&dot) || (322..=337).contains(&dot) {
            self.shift_background();
        }
        if visible && (1..=256).contains(&dot) {
            self.output_pixel();
        }

        if visible && (1..=256).contains(&dot) {
            self.evaluate_sprites();
        }

        if dot == 321 {
            self.sprites.buffer = self.sprites.secondary[0];
        }

        match dot {
            1..=256 | 321..=336 => self.fetch_background(),
            257..=320 => {
                if dot == 257 {
                    self.load_shifters();
                    self.copy_horizontal();
                    if !visible {
                        self.sprites.count = 0;
                    }
                }
                // OAMADDR is held at 0 while sprites are fetched
                self.oam_addr = 0;
                self.fetch_sprites();
            }
            337 => self.load_shifters(),
            338 | 340 => {
                // two unused nametable fetches end the line (MMC5 counts them)
                let addr = 0x2000 | (self.v & 0x0FFF);
                self.bus_address(addr);
            }
            _ => {}
        }
        if dot == 256 {
            self.increment_y();
        }
        if self.scanline == self.prerender_line && (280..=304).contains(&dot) {
            self.copy_vertical();
        }
    }

    fn shift_background(&mut self) {
        let bg = &mut self.bg;
        bg.pattern_low <<= 1;
        bg.pattern_high <<= 1;
        bg.attribute_low <<= 1;
        bg.attribute_high <<= 1;
    }

    /// Moves the fetched tile into the low byte of the shifters.
    fn load_shifters(&mut self) {
        let bg = &mut self.bg;
        bg.pattern_low = (bg.pattern_low & 0xFF00) | bg.next_low as u16;
        bg.pattern_high = (bg.pattern_high & 0xFF00) | bg.next_high as u16;
        let spread = |bit: u8| if bit != 0 { 0xFF } else { 0x00 };
        bg.attribute_low = (bg.attribute_low & 0xFF00) | spread(bg.next_palette & 1);
        bg.attribute_high = (bg.attribute_high & 0xFF00) | spread(bg.next_palette & 2);
    }

    /// One step of the 8-dot tile fetch. Each fetch takes two dots and the
    /// byte is read on the second: nametable on dot 2, attribute on 4,
    /// pattern low on 6, pattern high on 8, after which coarse X moves on.
    /// The shifters take the new tile on the next dot (9, 17, ...).
    fn fetch_background(&mut self) {
        match self.dot & 7 {
            1 => {
                if self.dot >= 9 {
                    self.load_shifters();
                }
            }
            2 => {
                let addr = 0x2000 | (self.v & 0x0FFF);
                self.bus_address(addr);
                self.bg.next_tile = self.read_nametable(addr);
            }
            4 => {
                let v = self.v;
                let addr = 0x23C0 | (v & 0x0C00) | ((v >> 4) & 0x38) | ((v >> 2) & 0x07);
                self.bus_address(addr);
                let shift = ((v >> 4) & 4) | (v & 2);
                self.bg.next_palette = (self.read_nametable(addr) >> shift) & 0b11;
            }
            6 => {
                let addr = self.background_pattern_addr();
                self.bus_address(addr);
                self.bg.next_low = self.chr_read(addr);
            }
            0 => {
                let addr = self.background_pattern_addr() + 8;
                self.bus_address(addr);
                self.bg.next_high = self.chr_read(addr);
                self.increment_coarse_x();
            }
            _ => {}
        }
    }

    fn background_pattern_addr(&self) -> u16 {
        let fine_y = (self.v >> 12) & 0b111;
        self.ctrl.bknd_pattern_addr() + self.bg.next_tile as u16 * 16 + fine_y
    }

    fn read_nametable(&self, addr: u16) -> u8 {
        self.vram[self.mirror_vram_addr(addr) as usize]
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
            sprites.buffer = 0xFF;
            sprites.secondary[(dot as usize - 1) >> 1] = 0xFF;
            return;
        }

        if dot == 65 {
            sprites.secondary_addr = 0;
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
            addr += 4;
            if sprites.secondary_addr >= 0x20 {
                sprites.buffer = sprites.secondary[(sprites.secondary_addr & 0x1F) as usize];
            }
        } else if sprites.secondary_addr < 0x20 { 
            sprites.secondary[sprites.secondary_addr as usize] = value;
            if !sprites.copying {
                if in_range(value) {
                    sprites.copying = true;
                    sprites.sprite_zero_found |= dot == 66;
                    sprites.secondary_addr += 1;
                    addr += 1;
                } else {
                    addr = (addr + 4) & !3;
                }
            } else {
                sprites.secondary_addr += 1;
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
            sprites.buffer = sprites.secondary[(sprites.secondary_addr & 0x1F) as usize];
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

        if dot == 256 {
            sprites.count = (sprites.secondary_addr >> 2) as usize;
        }
        if overflow {
            self.status.set_sprite_overflow(true);
        }
    }

    /// Dots 257-320: 8 dots per sprite slot, two garbage nametable fetches
    /// then the two pattern bytes (read on dots 262 and 264 for the first
    /// slot). The pixels go into `next_line`
    fn fetch_sprites(&mut self) {
        let offset = self.dot - 257;
        let slot = (offset / 8) as usize;
        self.sprites.buffer = self.sprites.secondary[slot * 4 + (offset as usize & 7).min(3)];
        match offset & 7 {
            1 | 3 => {
                let addr = 0x2000 | (self.v & 0x0FFF);
                self.bus_address(addr);
            }
            5 => {
                let addr = self.sprite_fetch_addr(slot);
                self.bus_address(addr);
            }
            7 => {
                let addr = self.sprite_fetch_addr(slot) + 8;
                self.bus_address(addr);
                self.load_sprite(slot);
            }
            _ => {}
        }
        if self.dot == 320 {
            std::mem::swap(&mut self.sprites.line, &mut self.sprites.next_line);
            self.sprites.next_line = [SpritePixel::default(); 256];
        }
    }

    fn sprite_fetch_addr(&self, slot: usize) -> u16 {
        if slot >= self.sprites.count {
            // empty slots fetch tile $FF
            return self.sprite_row_addr(0xFF, 0);
        }
        let sprite = &self.sprites.secondary[slot * 4..slot * 4 + 4];
        let (y, tile, attr) = (sprite[0], sprite[1], sprite[2]);
        let height = self.ctrl.sprite_size() as u16;
        let mut row = self.scanline.wrapping_sub(y as u16) & (height - 1);
        if attr & 0x80 != 0 {
            row = height - 1 - row;
        }
        self.sprite_row_addr(tile, row as usize)
    }

    /// Reads a sprite's pattern row and draws it into `next_line` wherever no
    /// earlier sprite has an opaque pixel.
    fn load_sprite(&mut self, slot: usize) {
        if slot >= self.sprites.count {
            return;
        }
        let addr = self.sprite_fetch_addr(slot);
        let (attr, x) = (
            self.sprites.secondary[slot * 4 + 2],
            self.sprites.secondary[slot * 4 + 3],
        );
        let sprite_zero = slot == 0 && self.sprites.sprite_zero_found;
        let mut low = self.chr_read(addr);
        let mut high = self.chr_read(addr + 8);
        if attr & 0x40 != 0 {
            low = low.reverse_bits();
            high = high.reverse_bits();
        }
        for col in 0..8 {
            let px = x as usize + col;
            if px > 255 {
                break;
            }
            let value = ((high >> (7 - col)) & 1) << 1 | ((low >> (7 - col)) & 1);
            let pixel = &mut self.sprites.next_line[px];
            if value != 0 && pixel.value == 0 {
                *pixel = SpritePixel {
                    value,
                    palette: attr & 0b11,
                    behind_background: attr & 0x20 != 0,
                    sprite_zero,
                };
            }
        }
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

        let mut sprite = SpritePixel::default();
        if self.mask.show_sprites() && (x >= 8 || self.mask.leftmost_8pxl_sprite()) {
            sprite = self.sprites.line[x];
        }

        if sprite.sprite_zero && sprite.value != 0 && bg_value != 0 && x != 255 {
            self.status.set_sprite_zero_hit(true);
        }

        let palette_addr = match (bg_value, sprite.value) {
            (0, 0) => 0,
            (0, s) => 0x10 | sprite.palette << 2 | s,
            (b, 0) => bg_palette << 2 | b,
            (b, s) => {
                if sprite.behind_background {
                    bg_palette << 2 | b
                } else {
                    0x10 | sprite.palette << 2 | s
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

    fn put_pixel(&mut self, x: usize, color: u8) {
        let color = if self.mask.is_greyscale() {
            color & 0x30
        } else {
            color
        };
        let emphasis = self
            .mask
            .emphasise(matches!(self.region, Region::Pal | Region::Dendy));
        let rgb = self.palette[emphasis << 6 | (color & 0x3F) as usize];
        let i = (self.scanline as usize * 256 + x) * 3;
        self.frame.data[i] = rgb.0;
        self.frame.data[i + 1] = rgb.1;
        self.frame.data[i + 2] = rgb.2;
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::ppu::PPU;
    use crate::render::palette::{SYSTEM_PALETTE, NTSC_PALETTE, PAL_PALETTE};

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
        assert_eq!(
            pixel(&mut ppu, 22, 10),
            NTSC_PALETTE[1 << 6 | RED as usize]
        );
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
        ppu.vram[1 * 32 + 2] = 1; // opaque background tile at x 16-23, y 8-15
        sprite(&mut ppu, 0, 16, 9, 0b0010_0000); // behind background, red
        sprite(&mut ppu, 1, 16, 9, 0b0000_0001); // in front, blue
        // sprite 0 wins the pixel, then hides behind the background
        assert_eq!(pixel(&mut ppu, 20, 10), colour(GREEN));
        // where the background is transparent, sprite 0 shows
        ppu.vram[1 * 32 + 2] = 0;
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
}
