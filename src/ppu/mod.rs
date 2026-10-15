use crate::cartridge::Mirroring;
use crate::mapper::SharedMapper;
use crate::render;
use crate::render::frame::Frame;
use registers::addr::AddrRegister;
use registers::control::ControlRegister;
use registers::mask::MaskRegister;
use registers::scroll::ScrollRegister;
use registers::status::StatusRegister;

pub mod registers;

pub struct NesPPU {
    mapper: SharedMapper,
    pub palette_table: [u8; 32],
    pub vram: [u8; 4096],

    pub addr: AddrRegister,
    pub ctrl: ControlRegister,
    pub mask: MaskRegister,
    pub status: StatusRegister,
    pub scroll: ScrollRegister,

    pub oam_data: [u8; 256],
    pub oam_addr: u8,

    write_latch: bool,
    odd_frame: bool,
    render_y_bump: u16,

    internal_data_buf: u8,
    pub scanlines: u16,
    cycles: usize,
    pub nmi_pending: bool,
    // (scroll_x, scroll_y, nametable_addr) in effect for each visible scanline,
    // so mid-frame scroll splits (e.g. SMB status bar) render correctly
    pub scanline_scroll: [(u8, u8, u16); 240],
    pub frame: Frame
}

pub trait PPU {
    fn write_to_ppu_addr(&mut self, value: u8);
    fn write_to_ctrl(&mut self, value: u8);
    fn write_to_mask(&mut self, value: u8);
    fn write_to_oam_addr(&mut self, value: u8);
    fn write_to_oam_data(&mut self, value: u8);
    fn write_to_scroll(&mut self, value: u8);
    fn write_to_data(&mut self, value: u8);
    fn write_oam_dma(&mut self, value: &[u8; 256]);
    fn read_data(&mut self) -> u8;
    fn read_status(&mut self) -> u8;
    fn read_oam_data(&self) -> u8;
}

impl NesPPU {
    pub fn new(mapper: SharedMapper) -> Self {
        NesPPU {
            mapper,
            vram: [0; 4096],
            oam_data: [0; 64 * 4],
            oam_addr: 0,
            palette_table: [0; 32],
            internal_data_buf: 0,
            cycles: 0,
            scanlines: 0,
            nmi_pending: false,
            scanline_scroll: [(0, 0, 0x2000); 240],
            frame: Frame::new(),
            write_latch: false,
            odd_frame: false,
            render_y_bump: 0,

            addr: AddrRegister::new(),
            ctrl: ControlRegister::new(),
            mask: MaskRegister::new(),
            scroll: ScrollRegister::new(),
            status: StatusRegister::new(),
        }
    }

    #[cfg(test)]
    pub fn with_mirroring(mirroring: Mirroring) -> Self {
        use crate::mapper::Nrom;
        use std::cell::RefCell;
        use std::rc::Rc;
        let cart = Nrom::from_parts(vec![0; 0x4000], vec![0; 0x2000], mirroring);
        NesPPU::new(Rc::new(RefCell::new(cart)))
    }    

    #[cfg(test)]
    pub fn new_empty_rom() -> Self {
        NesPPU::with_mirroring(Mirroring::Horizontal)
    }

    pub fn chr_read(&self, addr:u16) -> u8 {
        self.mapper.borrow().ppu_read(addr)
    }

    pub fn mirroring(&self) -> Mirroring {
        self.mapper.borrow().mirroring()
    }

    /// Pattern-table address of the low bitplane for `row` of a sprite's tile
    /// (the high plane is 8 bytes later). `row` is counted from the top of the
    /// sprite after any vertical flip, and may be 0-15 for 8x16 sprites.
    pub fn sprite_row_addr(&self, tile_idx: u8, row: usize) -> u16 {
        let row = row as u16;
        if self.ctrl.sprite_size() == 16 {
            // 8x16: bit 0 of the tile index picks the pattern table, and the
            // bottom half of the sprite is the next tile
            let bank = (tile_idx as u16 & 1) * 0x1000;
            let tile = (tile_idx as u16 & 0xFE) + row / 8;
            bank + tile * 16 + row % 8
        } else {
            self.ctrl.sprt_pattern_addr() + tile_idx as u16 * 16 + row
        }
    }

    fn is_rendering(&self) -> bool {
        (self.mask.show_background() || self.mask.show_sprites())
            && (self.scanlines < 240 || self.scanlines == 261)
    }

    fn increment_vram_addr(&mut self) {
        if self.is_rendering() {
            self.render_y_bump += 1;
        } else {
            self.addr.increment(self.ctrl.vram_addr_increment());
        }
    }

    fn current_scroll(&self) -> (u8, u8, u16) {
        let mut nametable = self.ctrl.nametable_addr();
        let mut y = self.scroll.scroll_y as u16 + self.render_y_bump;

        while y >= 240 {
            y -= 240;
            nametable ^= 0x800;
        }
        (self.scroll.scroll_x, y as u8, nametable)
    }

    /// Maps a nametable address ($2000-$3EFF) to an index into `vram`
    pub fn mirror_vram_addr(&self, addr: u16) -> u16 {
        let vram_index = (addr & 0x2FFF) - 0x2000;
        let name_table = vram_index / 0x400;
        let offset = vram_index & 0x3FF;
        match (self.mirroring(), name_table) {
            (Mirroring::Vertical, 2) | (Mirroring::Vertical, 3) => vram_index - 0x800,
            (Mirroring::Horizontal, 1) | (Mirroring::Horizontal, 2) => vram_index - 0x400,
            (Mirroring::Horizontal, 3) => vram_index - 0x800,
            (Mirroring::SingleScreenLower, _) => offset,
            (Mirroring::SingleScreenUpper, _) => 0x400 + offset,
            _ => vram_index,
        }
    }

    pub fn tick(&mut self, cycles: u8) -> bool {
        self.cycles += cycles as usize;
        let rendering = self.mask.show_background() || self.mask.show_sprites();
        let line_len = if self.scanlines == 261 && self.odd_frame && rendering {
            340
        } else {
            341
        };

        if self.cycles >= line_len {
            if self.is_sprite_0_hit() {
                self.status.set_sprite_zero_hit(true);
            }

            let line = self.scanlines as usize;
            if line < 240 {
                let scroll = self.current_scroll();
                self.scanline_scroll[line] = scroll;
                self.render_scanline(line, scroll);
            }

            if rendering && (line < 240 || line == 261) {
                self.mapper.borrow_mut().scanline();
            }

            self.cycles = self.cycles - line_len;
            self.scanlines += 1;

            if self.scanlines == 241 {
                self.status.set_vblank_status(true);
                self.status.set_sprite_zero_hit(false);
                if self.ctrl.generate_vblank_nmi() {
                    self.nmi_pending = true;
                }
            }

            if self.scanlines >= 262 {
                self.scanlines = 0;
                self.odd_frame = !self.odd_frame;
                self.nmi_pending = false;
                self.render_y_bump = 0;
                self.status.set_sprite_zero_hit(false);
                self.status.reset_vblank_status();
                return true;
            }
        }
        return false;
    }

    fn render_scanline(&mut self, line: usize, scroll: (u8, u8, u16)) {
        let mut frame = std::mem::take(&mut self.frame);
        render::render_scanline(self, line, scroll, &mut frame);
        self.frame = frame;
    }

    /// Returns the logical nametable's offset into vram, the tile column/row and the
    /// 2-bit pixel value of the background at a screen position for the given
    /// (scroll_x, scroll_y, nametable_addr).
    pub fn bg_pixel(
        &self,
        screen_x: usize,
        screen_y: usize,
        scroll: (u8, u8, u16),
    ) -> (usize, usize, usize, u8) {
        let (scroll_x, scroll_y, nametable_addr) = scroll;
        let base_nt = ((nametable_addr - 0x2000) / 0x400) as usize;

        // position in the 512x480 space made of the 4 logical nametables
        let world_x = screen_x + scroll_x as usize + (base_nt & 1) * 256;
        let world_y = screen_y + scroll_y as usize + (base_nt >> 1) * 240;
        let nt_x = (world_x / 256) % 2;
        let nt_y = (world_y / 240) % 2;
        let x = world_x % 256;
        let y = world_y % 240;
        let tile_column = x / 8;
        let tile_row = y / 8;

        // mirror_vram_addr maps the logical nametable onto physical vram
        let nt_start = self.mirror_vram_addr(0x2000 + ((nt_y * 2 + nt_x) * 0x400) as u16) as usize;
        let tile_idx = self.vram[nt_start + tile_row * 32 + tile_column] as usize;
        let row_addr = self.ctrl.bknd_pattern_addr() + (tile_idx * 16 + y % 8) as u16;

        let shift = 7 - (x % 8);
        let low_plane = (self.chr_read(row_addr) >> shift) & 1;
        let high_plane = (self.chr_read(row_addr + 8) >> shift) & 1;
        (nt_start, tile_column, tile_row, high_plane << 1 | low_plane)
    }

    // Sprite 0 hit is set on the first pixel where an opaque sprite 0 pixel
    // overlaps an opaque background pixel, checked once per visible scanline.
    fn is_sprite_0_hit(&self) -> bool {
        let line = self.scanlines as usize;
        if line >= 240 || !self.mask.show_background() || !self.mask.show_sprites() {
            return false;
        }

        // sprites are displayed one line below their OAM y
        let sprite_y = self.oam_data[0] as usize + 1;
        let height = self.ctrl.sprite_size() as usize;
        if line < sprite_y || line >= sprite_y + height {
            return false;
        }

        let attr = self.oam_data[2];
        let sprite_x = self.oam_data[3] as usize;
        let mut row = line - sprite_y;
        if attr >> 7 & 1 == 1 {
            row = height - 1 - row;
        }

        let row_addr = self.sprite_row_addr(self.oam_data[1], row);
        let low_plane = self.chr_read(row_addr);
        let high_plane = self.chr_read(row_addr + 8);

        let scroll = self.current_scroll();
        for col in 0..8 {
            let x = sprite_x + col;
            // no hit at x=255
            if x >= 255 {
                break;
            }
            let shift = if attr >> 6 & 1 == 1 { col } else { 7 - col };
            let pixel = ((high_plane >> shift) & 1) << 1 | ((low_plane >> shift) & 1);
            if pixel != 0 && self.bg_pixel(x, line, scroll).3 != 0 {
                return true;
            }
        }
        false
    }

    pub fn poll_nmi_interrupt(&mut self) -> bool {
        std::mem::take(&mut self.nmi_pending)
    }
}

fn palette_index(addr: u16) -> usize {
    let index = (addr & 0x1F) as usize;
    if index & 0x13 == 0x10 { index & 0x0F } else { index } 
}

impl PPU for NesPPU {
    fn write_to_ppu_addr(&mut self, value: u8) {
        let first_write = !self.write_latch;
        if first_write {
            self.ctrl.set_nametable((value >> 2) & 0b11);
            self.scroll.write_addr_hi(value);
        } else {
            self.scroll.write_addr_lo(value);
            self.render_y_bump = 0;
        }
        self.addr.update(value, first_write);
        self.write_latch = !self.write_latch;
    }

    fn write_to_ctrl(&mut self, value: u8) {
        let before_nmi_status = self.ctrl.generate_vblank_nmi();
        self.ctrl.update(value);
        if !before_nmi_status && self.ctrl.generate_vblank_nmi() && self.status.is_in_vblank() {
            self.nmi_pending = true;
        }
    }

    fn write_to_mask(&mut self, value: u8) {
        self.mask.update(value);
    }

    fn write_to_oam_addr(&mut self, value: u8) {
        self.oam_addr = value;
    }

    fn write_to_oam_data(&mut self, value: u8) {
        self.oam_data[self.oam_addr as usize] = value;
        self.oam_addr = self.oam_addr.wrapping_add(1);
    }

    fn write_to_scroll(&mut self, value: u8) {
        self.scroll.write(value, !self.write_latch);
        self.write_latch = !self.write_latch;
    }

    fn write_to_data(&mut self, value: u8) {
        let addr = self.addr.get();
        match addr {
            // CHR-RAM; the mapper ignores the write if the cartridge has CHR-ROM
            0..=0x1fff => self.mapper.borrow_mut().ppu_write(addr, value),
            // 0x3000..=0x3eff mirrors 0x2000..=0x2eff; mirror_vram_addr folds it down
            0x2000..=0x3eff => {
                self.vram[self.mirror_vram_addr(addr) as usize] = value;
            }
            // palette RAM is 6 bits wide, so only colours $00-$3F can be stored.
            // palette_index handles the $3F10/$3F14/$3F18/$3F1C mirrors.
            0x3f00..=0x3fff => {
                self.palette_table[palette_index(addr)] = value & 0x3F
            }
            _ => panic!("unexpected access to mirrored space {}", addr),
        }
        self.increment_vram_addr();
    }

    fn write_oam_dma(&mut self, data: &[u8; 256]) {
        for x in data.iter() {
            self.oam_data[self.oam_addr as usize] = *x;
            self.oam_addr = self.oam_addr.wrapping_add(1);
        }
    }

    fn read_data(&mut self) -> u8 {
        let addr = self.addr.get();
        self.increment_vram_addr();

        match addr {
            0..=0x1fff => {
                let result = self.internal_data_buf;
                self.internal_data_buf = self.chr_read(addr);
                result
            }
            0x2000..=0x3eff => {
                let result = self.internal_data_buf;
                self.internal_data_buf = self.vram[self.mirror_vram_addr(addr) as usize];
                result
            }
            0x3f00..=0x3fff => self.palette_table[palette_index(addr)],
            _ => panic!("unexpected access to mirrored space {}", addr),
        }
    }

    fn read_status(&mut self) -> u8 {
        let data = self.status.snapshot();
        self.status.reset_vblank_status();
        self.write_latch = false;
        data
    }

    fn read_oam_data(&self) -> u8 {
        self.oam_data[self.oam_addr as usize]
    }
}

#[cfg(test)]
pub mod test {
    use super::*;

    #[test]
    fn test_ppu_vram_writes() {
        let mut ppu = NesPPU::new_empty_rom();
        ppu.write_to_ppu_addr(0x23);
        ppu.write_to_ppu_addr(0x05);
        ppu.write_to_data(0x66);

        assert_eq!(ppu.vram[0x0305], 0x66);
    }

    #[test]
    fn test_ppu_vram_reads() {
        let mut ppu = NesPPU::new_empty_rom();
        ppu.write_to_ctrl(0);
        ppu.vram[0x0305] = 0x66;

        ppu.write_to_ppu_addr(0x23);
        ppu.write_to_ppu_addr(0x05);

        ppu.read_data(); //load_into_buffer
        assert_eq!(ppu.addr.get(), 0x2306);
        assert_eq!(ppu.read_data(), 0x66);
    }

    #[test]
    fn test_ppu_vram_reads_cross_page() {
        let mut ppu = NesPPU::new_empty_rom();
        ppu.write_to_ctrl(0);
        ppu.vram[0x01ff] = 0x66;
        ppu.vram[0x0200] = 0x77;

        ppu.write_to_ppu_addr(0x21);
        ppu.write_to_ppu_addr(0xff);

        ppu.read_data(); //load_into_buffer
        assert_eq!(ppu.read_data(), 0x66);
        assert_eq!(ppu.read_data(), 0x77);
    }

    #[test]
    fn test_ppu_vram_reads_step_32() {
        let mut ppu = NesPPU::new_empty_rom();
        ppu.write_to_ctrl(0b100);
        ppu.vram[0x01ff] = 0x66;
        ppu.vram[0x01ff + 32] = 0x77;
        ppu.vram[0x01ff + 64] = 0x88;

        ppu.write_to_ppu_addr(0x21);
        ppu.write_to_ppu_addr(0xff);

        ppu.read_data(); //load_into_buffer
        assert_eq!(ppu.read_data(), 0x66);
        assert_eq!(ppu.read_data(), 0x77);
        assert_eq!(ppu.read_data(), 0x88);
    }

    // Horizontal: https://www.nesdev.org/wiki/Mirroring
    //   [0x2000 A ] [0x2400 a ]
    //   [0x2800 B ] [0x2C00 b ]
    #[test]
    fn test_vram_horizontal_mirror() {
        let mut ppu = NesPPU::new_empty_rom();
        ppu.write_to_ppu_addr(0x24);
        ppu.write_to_ppu_addr(0x05);

        ppu.write_to_data(0x66); //write to a

        ppu.write_to_ppu_addr(0x28);
        ppu.write_to_ppu_addr(0x05);

        ppu.write_to_data(0x77); //write to B

        ppu.write_to_ppu_addr(0x20);
        ppu.write_to_ppu_addr(0x05);

        ppu.read_data(); //load into buffer
        assert_eq!(ppu.read_data(), 0x66); //read from A

        ppu.write_to_ppu_addr(0x2C);
        ppu.write_to_ppu_addr(0x05);

        ppu.read_data(); //load into buffer
        assert_eq!(ppu.read_data(), 0x77); //read from b
    }

    // Vertical: https://www.nesdev.org/wiki/Mirroring
    //   [0x2000 A ] [0x2400 B ]
    //   [0x2800 a ] [0x2C00 b ]
    #[test]
    fn test_vram_vertical_mirror() {
        let mut ppu = NesPPU::with_mirroring(Mirroring::Vertical);

        ppu.write_to_ppu_addr(0x20);
        ppu.write_to_ppu_addr(0x05);

        ppu.write_to_data(0x66); //write to A

        ppu.write_to_ppu_addr(0x2C);
        ppu.write_to_ppu_addr(0x05);

        ppu.write_to_data(0x77); //write to b

        ppu.write_to_ppu_addr(0x28);
        ppu.write_to_ppu_addr(0x05);

        ppu.read_data(); //load into buffer
        assert_eq!(ppu.read_data(), 0x66); //read from a

        ppu.write_to_ppu_addr(0x24);
        ppu.write_to_ppu_addr(0x05);

        ppu.read_data(); //load into buffer
        assert_eq!(ppu.read_data(), 0x77); //read from B
    }

    #[test]
    fn test_read_status_resets_latch() {
        let mut ppu = NesPPU::new_empty_rom();
        ppu.vram[0x0305] = 0x66;

        ppu.write_to_ppu_addr(0x21);
        ppu.write_to_ppu_addr(0x23);
        ppu.write_to_ppu_addr(0x05);

        ppu.read_data(); //load_into_buffer
        assert_ne!(ppu.read_data(), 0x66);

        ppu.read_status();

        ppu.write_to_ppu_addr(0x23);
        ppu.write_to_ppu_addr(0x05);

        ppu.read_data(); //load_into_buffer
        assert_eq!(ppu.read_data(), 0x66);
    }

    #[test]
    fn test_ppu_vram_mirroring() {
        let mut ppu = NesPPU::new_empty_rom();
        ppu.write_to_ctrl(0);
        ppu.vram[0x0305] = 0x66;

        ppu.write_to_ppu_addr(0x63); //0x6305 -> 0x2305
        ppu.write_to_ppu_addr(0x05);

        ppu.read_data(); //load into_buffer
        assert_eq!(ppu.read_data(), 0x66);
        // assert_eq!(ppu.addr.read(), 0x0306)
    }

    #[test]
    fn test_read_status_resets_vblank() {
        let mut ppu = NesPPU::new_empty_rom();
        ppu.status.set_vblank_status(true);

        let status = ppu.read_status();

        assert_eq!(status >> 7, 1);
        assert_eq!(ppu.status.snapshot() >> 7, 0);
    }

    #[test]
    fn test_oam_read_write() {
        let mut ppu = NesPPU::new_empty_rom();
        ppu.write_to_oam_addr(0x10);
        ppu.write_to_oam_data(0x66);
        ppu.write_to_oam_data(0x77);

        ppu.write_to_oam_addr(0x10);
        assert_eq!(ppu.read_oam_data(), 0x66);

        ppu.write_to_oam_addr(0x11);
        assert_eq!(ppu.read_oam_data(), 0x77);
    }

    #[test]
    fn test_oam_dma() {
        let mut ppu = NesPPU::new_empty_rom();

        let mut data = [0x66; 256];
        data[0] = 0x77;
        data[255] = 0x88;

        ppu.write_to_oam_addr(0x10);
        ppu.write_oam_dma(&data);

        ppu.write_to_oam_addr(0xf); //wrap around
        assert_eq!(ppu.read_oam_data(), 0x88);

        ppu.write_to_oam_addr(0x10);
        assert_eq!(ppu.read_oam_data(), 0x77);

        ppu.write_to_oam_addr(0x11);
        assert_eq!(ppu.read_oam_data(), 0x66);
    }

     #[test]
    fn test_four_screen_uses_4k_vram() {
        let mut ppu = NesPPU::with_mirroring(Mirroring::FourScreen);
        ppu.write_to_ppu_addr(0x2C);
        ppu.write_to_ppu_addr(0x05);
        ppu.write_to_data(0x66);
        assert_eq!(ppu.vram[0x0C05], 0x66);
    }

    #[test]
    fn test_single_screen_mirroring() {
        let ppu = NesPPU::with_mirroring(Mirroring::SingleScreenUpper);
        for nt in [0x2000u16, 0x2400, 0x2800, 0x2C00] {
            assert_eq!(ppu.mirror_vram_addr(nt + 5), 0x405);
        }
    }

    #[test]
    fn test_chr_ram_write_and_read() {
        use crate::mapper::Nrom;
        use std::cell::RefCell;
        use std::rc::Rc;
        let cart = Nrom::from_parts(vec![0; 0x4000], vec![], Mirroring::Vertical);
        let mut ppu = NesPPU::new(Rc::new(RefCell::new(cart)));
        ppu.write_to_ppu_addr(0x01);
        ppu.write_to_ppu_addr(0x23);
        ppu.write_to_data(0x5A);
        assert_eq!(ppu.chr_read(0x0123), 0x5A);
    }

    #[test]
    fn test_sprite_row_addr() {
        let mut ppu = NesPPU::new_empty_rom();
        ppu.write_to_ctrl(0b0000_1000); // 8x8 sprites from $1000
        assert_eq!(ppu.sprite_row_addr(0x02, 3), 0x1000 + 0x20 + 3);

        ppu.write_to_ctrl(0b0010_0000); // 8x16; table comes from the tile index
        assert_eq!(ppu.sprite_row_addr(0x03, 0), 0x1000 + 0x20);
        assert_eq!(ppu.sprite_row_addr(0x03, 9), 0x1000 + 0x30 + 1);
        assert_eq!(ppu.sprite_row_addr(0x04, 15), 0x0000 + 0x50 + 7);
    }

    #[test]
    fn test_palette_stores_only_6_bits() {
        let mut ppu = NesPPU::new_empty_rom();
        ppu.write_to_ppu_addr(0x3F);
        ppu.write_to_ppu_addr(0x11);
        ppu.write_to_data(0xF0);
        assert_eq!(ppu.palette_table[0x11], 0x30);
    }

    #[test]
    fn test_palette_mirror_write_is_masked() {
        let mut ppu = NesPPU::new_empty_rom();
        ppu.write_to_ppu_addr(0x3F);
        ppu.write_to_ppu_addr(0x10); // mirror of $3F00
        ppu.write_to_data(0xFF);
        assert_eq!(ppu.palette_table[0x00], 0x3F);
    }

    #[test]
    fn test_palette_mirrors() {
        let mut ppu = NesPPU::new_empty_rom();
        ppu.write_to_ppu_addr(0x3F);
        ppu.write_to_ppu_addr(0x30); // mirror of $3F10, which mirrors $3F00
        ppu.write_to_data(0x21);
        assert_eq!(ppu.palette_table[0x00], 0x21);

        ppu.write_to_ppu_addr(0x3F);
        ppu.write_to_ppu_addr(0x10);
        assert_eq!(ppu.read_data(), 0x21);
    }

    fn counting_ppu() -> (NesPPU, std::rc::Rc<std::cell::RefCell<crate::mapper::test_util::ScanlineCounter>>) {
        use crate::mapper::test_util::ScanlineCounter;
        let cart = std::rc::Rc::new(std::cell::RefCell::new(ScanlineCounter::new(vec![0; 0x8000], None)));
        (NesPPU::new(cart.clone()), cart)
    }

    fn run_frame(ppu: &mut NesPPU) {
        while !ppu.tick(3) {}
    }

    #[test]
    fn test_mapper_clocked_once_per_rendered_line() {
        let (mut ppu, cart) = counting_ppu();
        ppu.write_to_mask(0b0000_1000); // show background
        run_frame(&mut ppu);
        // 240 visible lines + the pre-render line
        assert_eq!(cart.borrow().scanlines, 241);
    }

    #[test]
    fn test_mapper_not_clocked_when_rendering_off() {
        let (mut ppu, cart) = counting_ppu();
        run_frame(&mut ppu);
        assert_eq!(cart.borrow().scanlines, 0);
    }

    #[test]
    fn test_mid_frame_change_shows_on_later_lines() {
        use crate::render::palette::SYSTEM_PALETTE;
        // blank CHR, so every background pixel is the backdrop colour
        let (mut ppu, _cart) = counting_ppu();
        ppu.palette_table[0] = 0x0F;
        while ppu.scanlines < 100 {
            ppu.tick(3);
        }
        ppu.palette_table[0] = 0x16; // change the backdrop mid-frame
        run_frame(&mut ppu);

        let pixel = |y: usize| {
            let i = y * 256 * 3;
            (ppu.frame.data[i], ppu.frame.data[i + 1], ppu.frame.data[i + 2])
        };
        assert_eq!(pixel(50), SYSTEM_PALETTE[0x0F]);
        assert_eq!(pixel(150), SYSTEM_PALETTE[0x16]);
    }
}
