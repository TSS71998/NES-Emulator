pub mod frame;
pub mod palette;

use crate::ppu::NesPPU;
use frame::Frame;

fn bg_palette(ppu: &NesPPU, attribute_table:&[u8], tile_column: usize, tile_row: usize) -> [u8; 4] {
    let attr_table_idx = tile_row / 4 * 8 + tile_column / 4;
    let attr_byte = attribute_table[attr_table_idx];

    let palette_idx = match (tile_column % 4 / 2, tile_row % 4 / 2) {
        (0,0) => attr_byte & 0b11,
        (1,0) => (attr_byte >> 2) & 0b11,
        (0,1) => (attr_byte >> 4) & 0b11,
        (1,1) => (attr_byte >> 6) & 0b11,
        (_,_) => panic!("should not happen")
    };

    let palette_start: usize = 1 + (palette_idx as usize) * 4;
    [
        ppu.palette_table[0],
        ppu.palette_table[palette_start],
        ppu.palette_table[palette_start+1],
        ppu.palette_table[palette_start+2]
    ]
}

fn sprite_palette(ppu: &NesPPU, palette_idx: u8) -> [u8; 4] {
    let start = 0x11 + (palette_idx * 4) as usize;
    [
        0,
        ppu.palette_table[start],
        ppu.palette_table[start+1],
        ppu.palette_table[start+2],
    ]
}

fn render_background(ppu: &NesPPU, frame: &mut Frame) {
    for screen_y in 0..240 {
        let scroll = ppu.scanline_scroll[screen_y];
        for screen_x in 0..256 {
            let (nt_start, tile_column, tile_row, value) = ppu.bg_pixel(screen_x, screen_y, scroll);
            let attribute_table = &ppu.vram[nt_start + 0x3c0..nt_start + 0x400];
            let palette = bg_palette(ppu, attribute_table, tile_column, tile_row);

            frame.set_pixel(screen_x, screen_y, palette::SYSTEM_PALLETE[palette[value as usize] as usize]);
        }
    }
}

pub fn render(ppu: &NesPPU, frame: &mut Frame) {
    render_background(ppu, frame);

    for i in (0..ppu.oam_data.len()).step_by(4).rev() {
        let tile_idx = ppu.oam_data[i + 1] as u16;
        let tile_x = ppu.oam_data[i + 3] as usize;
        // sprites are displayed one line below their OAM y
        let tile_y = ppu.oam_data[i] as usize + 1;

        let filp_vertical = if ppu.oam_data[i + 2] >> 7 & 1 == 1 {
            true
        } else {
            false
        };

        let filp_horizontal = if ppu.oam_data[i + 2] >> 6 & 1 == 1 {
            true
        } else {
            false
        };

        let palette_idx = ppu.oam_data[i + 2] & 0b11;
        let sprite_palette = sprite_palette(ppu, palette_idx);
        let bank: u16 = ppu.ctrl.sprt_pattern_addr();

        let tile = &ppu.chr_rom[(bank + tile_idx * 16) as usize..=(bank + tile_idx * 16 + 15) as usize];

        for y in 0..=7 {
            let mut upper = tile[y];
            let mut lower = tile[y + 8];

            for x in (0..=7).rev() {
                let value = (1 & lower) << 1 | (1 & upper);
                upper = upper >> 1;
                lower = lower >> 1;
                let rgb = match value {
                    0 => continue,
                    1 => palette::SYSTEM_PALLETE[sprite_palette[1] as usize],
                    2 => palette::SYSTEM_PALLETE[sprite_palette[2] as usize],
                    3 => palette::SYSTEM_PALLETE[sprite_palette[3] as usize],
                    _ => panic!("can't be"),
                };
                match (filp_horizontal, filp_vertical) {
                    (false, false) => frame.set_pixel(tile_x + x, tile_y + y, rgb),
                    (true, false) => frame.set_pixel(tile_x + 7 - x, tile_y + y, rgb),
                    (false, true) => frame.set_pixel(tile_x + x, tile_y + 7 - y, rgb),
                    (true, true) => frame.set_pixel(tile_x + 7 - x, tile_y + 7 - y, rgb),
                }
            }
        }
    }
}