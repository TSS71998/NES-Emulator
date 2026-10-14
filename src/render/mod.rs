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
                let px = if flip_horizontal { tile_x + 7 - x } else { tile_x + x };
                let py = if flip_vertical { tile_y + height - 1 - y } else { tile_y + y };
                frame.set_pixel(px, py, rgb);
            }
        }
    }
}
