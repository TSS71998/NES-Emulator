pub mod frame;
pub mod palette;

use crate::ppu::NesPPU;
use frame::Frame;

fn bg_palette(
    ppu: &NesPPU,
    attribute_table: &[u8],
    tile_column: usize,
    tile_row: usize,
) -> [u8; 4] {
    let attr_table_idx = tile_row / 4 * 8 + tile_column / 4;
    let attr_byte = attribute_table[attr_table_idx];

    let palette_idx = match (tile_column % 4 / 2, tile_row % 4 / 2) {
        (0, 0) => attr_byte & 0b11,
        (1, 0) => (attr_byte >> 2) & 0b11,
        (0, 1) => (attr_byte >> 4) & 0b11,
        (1, 1) => (attr_byte >> 6) & 0b11,
        (_, _) => panic!("should not happen"),
    };

    let palette_start: usize = 1 + (palette_idx as usize) * 4;
    [
        ppu.palette_table[0],
        ppu.palette_table[palette_start],
        ppu.palette_table[palette_start + 1],
        ppu.palette_table[palette_start + 2],
    ]
}

fn sprite_palette(ppu: &NesPPU, palette_idx: u8) -> [u8; 4] {
    let start = 0x11 + (palette_idx * 4) as usize;
    [
        0,
        ppu.palette_table[start],
        ppu.palette_table[start + 1],
        ppu.palette_table[start + 2],
    ]
}

fn render_background(ppu: &NesPPU, frame: &mut Frame) {
    for screen_y in 0..240 {
        let scroll = ppu.scanline_scroll[screen_y];
        for screen_x in 0..256 {
            let (nt_start, tile_column, tile_row, value) = ppu.bg_pixel(screen_x, screen_y, scroll);
            let attribute_table = &ppu.vram[nt_start + 0x3c0..nt_start + 0x400];
            let palette = bg_palette(ppu, attribute_table, tile_column, tile_row);

            frame.set_pixel(
                screen_x,
                screen_y,
                palette::SYSTEM_PALETTE[palette[value as usize] as usize],
            );
        }
    }
}

pub fn render(ppu: &NesPPU, frame: &mut Frame) {
    render_background(ppu, frame);

    let height = ppu.ctrl.sprite_size() as usize;

    for i in (0..ppu.oam_data.len()).step_by(4).rev() {
        let tile_idx = ppu.oam_data[i + 1];
        let tile_x = ppu.oam_data[i + 3] as usize;
        let tile_y = ppu.oam_data[i] as usize + 1;

        let attr = ppu.oam_data[i + 2];
        let flip_vertical = attr >> 7 & 1 == 1;
        let flip_horizontal = attr >> 6 & 1 == 1;

        let palette_idx = attr & 0b11;
        let sprite_palette = sprite_palette(ppu, palette_idx);

        for y in 0..height {
            let row_addr = ppu.sprite_row_addr(tile_idx, y);
            let mut low_plane = ppu.chr_read(row_addr);
            let mut high_plane = ppu.chr_read(row_addr + 8);

            for x in (0..=7).rev() {
                let value = (1 & high_plane) << 1 | (1 & low_plane);
                low_plane >>= 1;
                high_plane >>= 1;
                if value == 0 {
                    continue;
                }
                let rgb = palette::SYSTEM_PALETTE[sprite_palette[value as usize] as usize];
                let px = if flip_horizontal {
                    tile_x + 7 - x
                } else {
                    tile_x + x
                };
                let py = if flip_vertical {
                    tile_y + height - 1 - y
                } else {
                    tile_y + y
                };
                frame.set_pixel(px, py, rgb);
            }
        }
    }
}

pub fn render_scanline(ppu: &NesPPU, line: usize, scroll: (u8, u8, u16), frame: &mut Frame) {
    let mut bg_values = [0u8; 256];
    render_background_line(ppu, line, scroll, frame, &mut bg_values);
    if ppu.mask.show_sprites() {
        render_sprites_line(ppu, line, frame, &bg_values);
    }
}

fn render_background_line(
    ppu: &NesPPU,
    line: usize,
    scroll: (u8, u8, u16),
    frame: &mut Frame,
    bg_values: &mut [u8; 256],
) {
    let backdrop = palette::SYSTEM_PALETTE[ppu.palette_table[0] as usize];
    for screen_x in 0..256 {
        let hidden =
            !ppu.mask.show_background() || (screen_x < 8 && !ppu.mask.leftmost_8pxl_background());
        if hidden {
            frame.set_pixel(screen_x, line, backdrop);
            continue;
        }

        let (nt_start, tile_column, tile_row, value) = ppu.bg_pixel(screen_x, line, scroll);
        let attribute_table = &ppu.vram[nt_start + 0x3C0..nt_start + 0x400];
        let palette = bg_palette(ppu, attribute_table, tile_column, tile_row);
        bg_values[screen_x] = value;

        frame.set_pixel(
            screen_x,
            line,
            palette::SYSTEM_PALETTE[palette[value as usize] as usize],
        );
    }
}

fn render_sprites_line(ppu: &NesPPU, line: usize, frame: &mut Frame, bg_values: &[u8; 256]) {
    let height = ppu.ctrl.sprite_size() as usize;
    // The lowest-numbered sprite with an opaque pixel wins that pixel, and
    // only then does its priority bit decide whether the background covers
    // it. (Games use this to hide sprites behind other sprites' "masks".)
    let mut claimed = [false; 256];

    for i in (0..ppu.oam_data.len()).step_by(4) {
        // sprites are displayed one line below their OAM y
        let tile_y = ppu.oam_data[i] as usize + 1;
        if line < tile_y || line >= tile_y + height {
            continue;
        }
        let tile_idx = ppu.oam_data[i + 1];
        let attr = ppu.oam_data[i + 2];
        let tile_x = ppu.oam_data[i + 3] as usize;
        let flip_vertical = attr >> 7 & 1 == 1;
        let flip_horizontal = attr >> 6 & 1 == 1;
        let behind_background = attr >> 5 & 1 == 1;
        let sprite_palette = sprite_palette(ppu, attr & 0b11);

        let row = line - tile_y;
        let row = if flip_vertical { height - 1 - row } else { row };
        let row_addr = ppu.sprite_row_addr(tile_idx, row);
        let mut low_plane = ppu.chr_read(row_addr);
        let mut high_plane = ppu.chr_read(row_addr + 8);

        for x in (0..=7).rev() {
            let value = (1 & high_plane) << 1 | (1 & low_plane);
            low_plane >>= 1;
            high_plane >>= 1;
            if value == 0 {
                continue;
            }
            let px = if flip_horizontal {
                tile_x + 7 - x
            } else {
                tile_x + x
            };
            if px >= 256 || claimed[px] || (px < 8 && !ppu.mask.leftmost_8pxl_sprite()) {
                continue;
            }
            claimed[px] = true;
            if behind_background && bg_values[px] != 0 {
                continue;
            }
            let rgb = palette::SYSTEM_PALETTE[sprite_palette[value as usize] as usize];
            frame.set_pixel(px, line, rgb);
        }
    }
}

#[cfg(test)]
mod test {
    use super::*;
    use crate::ppu::PPU;

    const RED: u8 = 0x16;
    const GREEN: u8 = 0x2A;
    const BLUE: u8 = 0x12;

    fn test_ppu() -> NesPPU {
        use crate::cartridge::Mirroring;
        use crate::mapper::{Mapper, Nrom};

        let mut cart = Nrom::from_parts(vec![0; 0x4000], vec![], Mirroring::Vertical);
        for row in 0..8 {
            cart.ppu_write(0x10 + row, 0xFF);
        }

        let mut ppu = NesPPU::new(std::rc::Rc::new(std::cell::RefCell::new(cart)));
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

    fn pixel(ppu: &mut NesPPU, x: usize, line: usize) -> (u8, u8, u8) {
        let mut frame = Frame::new();
        render_scanline(ppu, line, (0, 0, 0x2000), &mut frame);
        let i = (line * 256 + x) * 3;
        (frame.data[i], frame.data[i + 1], frame.data[i + 2])
    }

    fn colour(index: u8) -> (u8, u8, u8) {
        palette::SYSTEM_PALETTE[index as usize]
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
}
