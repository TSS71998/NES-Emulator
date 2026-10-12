use crate::cartridge::Mirroring;
use registers::addr::AddrRegister;
use registers::control::ControlRegister;
use registers::mask::MaskRegister;
use registers::scroll::ScrollRegister;
use registers::status::StatusRegister;

pub mod registers;

pub struct NesPPU {
    pub chr_rom: Vec<u8>,
    pub palette_table: [u8; 32],
    pub vram: [u8; 2048],

    pub addr: AddrRegister,
    pub ctrl: ControlRegister,
    pub mask: MaskRegister,
    pub status: StatusRegister,
    pub scroll: ScrollRegister,

    pub oam_data: [u8; 256],
    pub oam_addr: u8,

    pub mirroring: Mirroring,
    internal_data_buf: u8,
    pub scanlines: u16,
    cycles: usize,
    pub nmi_interrupt: Option<u8>,
    // (scroll_x, scroll_y, nametable_addr) in effect for each visible scanline,
    // so mid-frame scroll splits (e.g. SMB status bar) render correctly
    pub scanline_scroll: [(u8, u8, u16); 240],
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
    pub fn new(chr_rom: Vec<u8>, mirroring: Mirroring) -> Self {
        NesPPU {
            chr_rom: chr_rom,
            mirroring: mirroring,
            vram: [0; 2048],
            oam_data: [0; 64 * 4],
            oam_addr: 0,
            palette_table: [0; 32],
            internal_data_buf: 0,
            cycles: 0,
            scanlines: 0,
            nmi_interrupt: None,
            scanline_scroll: [(0, 0, 0x2000); 240],

            addr: AddrRegister::new(),
            ctrl: ControlRegister::new(),
            mask: MaskRegister::new(),
            scroll: ScrollRegister::new(),
            status: StatusRegister::new(),
        }
    }

    pub fn new_empty_rom() -> Self {
        NesPPU::new(vec![0; 2048], Mirroring::Horizontal)
    }

    fn increment_vram_addr(&mut self) {
        self.addr.increment(self.ctrl.vram_addr_increment());
    }

    pub fn mirror_vram_addr(&self, addr: u16) -> u16 {
        let mirrored_vram = addr & 0b10111111111111;
        let vram_index = mirrored_vram - 0x2000;
        let name_table = vram_index / 0x400;
        match (&self.mirroring, name_table) {
            (Mirroring::Vertical, 2) | (Mirroring::Vertical, 3) => vram_index - 0x800,
            (Mirroring::Horizontal, 2) => vram_index - 0x400,
            (Mirroring::Horizontal, 1) => vram_index - 0x400,
            (Mirroring::Horizontal, 3) => vram_index - 0x800,
            _ => vram_index
        } 
    }  

    pub fn tick(&mut self, cycles: u8) -> bool {
        self.cycles += cycles as usize;
        if self.cycles >= 341 {
            if self.is_sprite_0_hit() {
                self.status.set_sprite_zero_hit(true);
            }

            if (self.scanlines as usize) < 240 {
                self.scanline_scroll[self.scanlines as usize] =
                    (self.scroll.scroll_x, self.scroll.scroll_y, self.ctrl.nametable_addr());
            }

            self.cycles = self.cycles - 341;
            self.scanlines += 1;
            
            if self.scanlines == 241 {
                self.status.set_vblank_status(true);
                self.status.set_sprite_zero_hit(false);
                if self.ctrl.generate_vblank_nmi() {
                    self.nmi_interrupt = Some(1);
                }
            }

            if self.scanlines >= 262 {
                self.scanlines = 0;
                self.nmi_interrupt = None;
                self.status.set_sprite_zero_hit(false);
                self.status.reset_vblank_status();
                return true;
            }
        }
        return false;
    } 

    /// Returns the logical nametable's offset into vram, the tile column/row and the
    /// 2-bit pixel value of the background at a screen position for the given
    /// (scroll_x, scroll_y, nametable_addr).
    pub fn bg_pixel(&self, screen_x: usize, screen_y: usize, scroll: (u8, u8, u16)) -> (usize, usize, usize, u8) {
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
        let tile = self.ctrl.bknd_pattern_addr() as usize + tile_idx * 16;

        let shift = 7 - (x % 8);
        let upper = (self.chr_rom[tile + y % 8] >> shift) & 1;
        let lower = (self.chr_rom[tile + y % 8 + 8] >> shift) & 1;
        (nt_start, tile_column, tile_row, lower << 1 | upper)
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

        let tile_idx = self.oam_data[1] as usize;
        let tile = if height == 16 {
            // 8x16: bank from bit 0 of the tile index, bottom half is the next tile
            (tile_idx & 1) * 0x1000 + (tile_idx & 0xfe) * 16 + (row / 8) * 16
        } else {
            self.ctrl.sprt_pattern_addr() as usize + tile_idx * 16
        };
        let row = row % 8;

        let scroll = (self.scroll.scroll_x, self.scroll.scroll_y, self.ctrl.nametable_addr());
        for col in 0..8 {
            let x = sprite_x + col;
            // no hit at x=255
            if x >= 255 {
                break;
            }
            let shift = if attr >> 6 & 1 == 1 { col } else { 7 - col };
            let upper = (self.chr_rom[tile + row] >> shift) & 1;
            let lower = (self.chr_rom[tile + row + 8] >> shift) & 1;
            if (lower << 1 | upper) != 0 && self.bg_pixel(x, line, scroll).3 != 0 {
                return true;
            }
        }
        false
    }

    pub fn poll_nmi_interrupt(&mut self) -> Option<u8> {
        self.nmi_interrupt.take()
    }
}

impl PPU for NesPPU {
    fn write_to_ppu_addr(&mut self, value: u8) {
        // games like SMB rely on $2006 writes resetting the nametable select and scroll
        if self.addr.is_hi_byte_next() {
            self.ctrl.set_nametable((value >> 2) & 0b11);
            self.scroll.write_addr_hi(value);
        } else {
            self.scroll.write_addr_lo(value);
        }
        self.addr.update(value);
    }

    fn write_to_ctrl(&mut self, value: u8) {
        let before_nmi_status = self.ctrl.generate_vblank_nmi();
        self.ctrl.update(value);
        if !before_nmi_status && self.ctrl.generate_vblank_nmi() && self.status.is_in_vblank() {
            self.nmi_interrupt = Some(1);
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
        self.scroll.write(value);
    }

    fn write_to_data(&mut self, value: u8) {
        let addr = self.addr.get();
        match addr {
            0..=0x1fff => println!("attempt to write to chr rom space {}", addr), 
            // 0x3000..=0x3eff mirrors 0x2000..=0x2eff; mirror_vram_addr folds it down
            0x2000..=0x3eff => {
                self.vram[self.mirror_vram_addr(addr) as usize] = value;
            }

            //Addresses $3F10/$3F14/$3F18/$3F1C are mirrors of $3F00/$3F04/$3F08/$3F0C
            0x3f10 | 0x3f14 | 0x3f18 | 0x3f1c => {
                let add_mirror = addr - 0x10;
                self.palette_table[(add_mirror - 0x3f00) as usize] = value;
            }
            0x3f00..=0x3fff =>
            {
                self.palette_table[((addr - 0x3f00) % 32) as usize] = value;
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
                self.internal_data_buf = self.chr_rom[addr as usize];
                result
            },
            0x2000..=0x3eff => {
                let result = self.internal_data_buf;
                self.internal_data_buf = self.vram[self.mirror_vram_addr(addr) as usize];
                result
            },
            0x3f10 | 0x3f14 | 0x3f18 | 0x3f1c => {
                self.palette_table[(addr - 0x10 - 0x3f00) as usize]
            }
            0x3f00..=0x3fff =>
            {
                self.palette_table[((addr - 0x3f00) % 32) as usize]
            }
            _ => panic!("unexpected access to mirrored space {}", addr),
        }
    }

    fn read_status(&mut self) -> u8 {
        let data = self.status.snapshot();
        self.status.reset_vblank_status();
        self.addr.reset_latch();
        self.scroll.reset_latch();
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
        let mut ppu = NesPPU::new(vec![0; 2048], Mirroring::Vertical);

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
}