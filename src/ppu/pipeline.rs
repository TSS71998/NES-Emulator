//! Background: every 8 dots the PPU fetches a nametable byte, an attribute
//! byte and the two pattern bytes of the next tile, then loads them into
//! 16-bit shift registers that shift once per dot. The pixel at screen x
//! comes out on dot x + 1; fine X picks which bit of the shifters is used.
//!
//! Sprites: at dot 257 the PPU finds the (up to 8) sprites on the next line,
//! and during dots 257-320 fetches their pattern rows. Empty slots fetch
//! tile $FF, so the pattern table is read 8 times per line either way.
//! That is where MMC3 sees A12 go high when sprites use $1000.

use super::{palette_index, NesPPU};
use crate::render::palette::SYSTEM_PALETTE;

#[derive(Default)]
pub(super) struct Background {
    next_tile: u8,
    next_palette: u8,
    next_low: u8,
    next_high: u8,
    pattern_low: u16,
    pattern_high: u16,
    attribute_low: u16,
    attribute_high: u16
}

#[derive(Clone, Copy, Default)]
struct SpritePixel {
    value: u8,
    palette: u8,
    behind_background: bool,
    sprite_zero: bool
}

pub(super) struct Sprites {
    /// Sprites found for the next line: (OAM index, y, tile, attributes, x)
    found: [(u8, u8, u8, u8, u8); 8],
    count: usize,
    line: [SpritePixel; 256],
    next_line: [SpritePixel; 256]
}

impl Default for Sprites {
    fn default() -> Self {
        Sprites {
            found: [(0, 0, 0, 0, 0); 8],
            count: 0,
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

        // the shifters move on dots 2-257 and 322-337
        if (2..=257).contains(&dot) || (322..=337).contains(&dot) {
            self.shift_background();
        }
        if visible && (1..=256).contains(&dot) {
            self.output_pixel();
        }

        match dot {
            1..=256 | 321..=336 => self.fetch_background(),
            257..=320 => {
                if dot == 257 {
                    self.load_shifters();
                    self.copy_horizontal();
                    self.evaluate_sprites();
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
        if self.scanline == self.region.prerender_scanline() && (280..=304).contains(&dot) {
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
        self.vram[super::mirror_nametable(self.mirroring(), addr) as usize]
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

    /// Finds the first 8 sprites in OAM that cover the next line, and sets
    /// the overflow flag if there are more, including the hardware's bug
    /// (after 8 sprites it checks the wrong byte of each following entry).
    fn evaluate_sprites(&mut self) {
        let sprites = &mut self.sprites;
        sprites.count = 0;
        if self.scanline >= 240 {
            return; // the pre-render line fetches but never shows sprites
        }
        let height = self.ctrl.sprite_size() as u16;
        let line = self.scanline;
        let in_range = |y: u8| line >= y as u16 && line < y as u16 + height;

        let mut n = 0;
        while n < 64 && sprites.count < 8 {
            let entry = &self.oam_data[n * 4..n * 4 + 4];
            if in_range(entry[0]) {
                sprites.found[sprites.count] = (n as u8, entry[0], entry[1], entry[2], entry[3]);
                sprites.count += 1;
            }
            n += 1;
        }
        let mut m = 0;
        while n < 64 {
            if in_range(self.oam_data[n * 4 + m]) {
                self.status.set_sprite_overflow(true);
                break;
            }
            n += 1;
            m = (m + 1) & 3;
        }
    }

    /// Dots 257-320: 8 dots per sprite slot, two garbage nametable fetches
    /// then the two pattern bytes (read on dots 262 and 264 for the first
    /// slot). The pixels go into `next_line`
    fn fetch_sprites(&mut self) {
        let offset = self.dot - 257;
        let slot = (offset / 8) as usize;
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
        let (_, y, tile, attr, _) = self.sprites.found[slot];
        let height = self.ctrl.sprite_size() as u16;
        let mut row = self.scanline - y as u16;
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
        let (index, _, _, attr, x) = self.sprites.found[slot];
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
                    sprite_zero: index == 0,
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
            bg_palette = (((bg.attribute_high >> bit) & 1) << 1 | ((bg.attribute_low >> bit) & 1)) as u8;
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

    fn put_pixel(&mut self, x: usize, colour: u8) {
        let colour = if self.mask.is_greyscale() { colour & 0x30 } else { colour };
        let rgb = SYSTEM_PALETTE[(colour & 0x3F) as usize];
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
        (ppu.frame.data[i], ppu.frame.data[i + 1], ppu.frame.data[i + 2])
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
