//! The 2C02 picture processing unit, emulated one dot (PPU clock) at a time.
//!
//! Each scanline is 341 dots. On the 240 visible lines and the pre-render
//! line the PPU fetches background tiles every 8 dots into shift registers,
//! evaluates and fetches the sprites for the next line during dots 257-320,
//! and outputs one pixel per dot during dots 1-256. Scrolling uses the real
//! internal registers ("loopy" v, t, x and w), so mid-frame writes to
//! $2000/$2005/$2006 take effect at the exact dot they happen on.
//!
//! The pipeline itself lives in `pipeline.rs`; this file has the CPU-facing
//! registers and the frame timing (vblank, NMI, odd-frame skip).

use crate::cartridge::Mirroring;
use crate::mapper::SharedMapper;
use crate::region::Region;
use crate::render::frame::Frame;
use registers::control::ControlRegister;
use registers::mask::MaskRegister;
use registers::status::StatusRegister;

mod registers;
pub mod pipeline;

use pipeline::{Background, Sprites};

pub struct NesPPU {
    mapper: SharedMapper,
    pub palette_table: [u8; 32],
    pub vram: [u8; 4096],

    pub ctrl: ControlRegister,
    pub mask: MaskRegister,
    pub status: StatusRegister,

    pub oam_data: [u8; 256],
    pub oam_addr: u8,

    // Loopy's registers (https://www.nesdev.org/wiki/PPU_scrolling):
    // v is the current VRAM address, and while rendering also the scroll
    // position (yyy NN YYYYY XXXXX: fine Y, nametable, coarse Y, coarse X);
    // t is the temporary address that $2000/$2005/$2006 write into; fine_x
    // is the 3-bit fine X scroll; write_latch is the shared $2005/$2006
    // first/second write toggle.
    v: u16,
    t: u16,
    fine_x: u8,
    write_latch: bool,

    internal_data_buf: u8,
    region: Region,
    vblank_line: u16,
    prerender_line: u16,
    lines_per_frame: u16,
    skips_odd_dot: bool,
    nametable_base: [u16; 4],
    mapper_watches_bus: bool,
    odd_frame: bool,
    
    pub scanline: u16,
    pub dot: u16,
    ppu_cycle: u64,
    
    /// A $2002 read one dot before vblank starts reads the flag as clear and
    /// stops it being set (and the NMI) for that frame.
    suppress_vblank: bool,
    rendering: bool,
    /// The PPU's /NMI output: vblank flag AND the NMI enable bit. The CPU
    /// detects its rising edge (see Bus).
    nmi_line: bool,

    /// The PPU's I/O data latch, the "decay register". Writing any PPU
    /// register fills it; reading a write-only register, or the undefined
    /// bits of $2002 and of palette reads, returns it. Each bit fades to 0
    /// about 600 ms after it was last refreshed (see `cpu_read`).
    io_latch: u8,
    io_latch_refreshed: [u64; 8],

    bg: Background,
    sprites: Sprites,

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
        let mapper_watches_bus = mapper.borrow().watches_ppu_bus();
        let region = Region::Ntsc;
        let mut ppu = NesPPU {
            mapper,
            vblank_line: region.vblank_scanline(),
            prerender_line: region.prerender_scanline(),
            lines_per_frame: region.scanlines_per_frame(),
            skips_odd_dot: region.skips_odd_frame_dot(),
            nametable_base: [0; 4],
            mapper_watches_bus,
            vram: [0; 4096],
            oam_data: [0; 64 * 4],
            oam_addr: 0,
            palette_table: [0; 32],
            ctrl: ControlRegister::new(),
            mask: MaskRegister::new(),
            status: StatusRegister::new(),
            v: 0,
            t: 0,
            fine_x: 0,
            write_latch: false,
            internal_data_buf: 0,
            region,
            odd_frame: false,
            scanline: 0,
            dot: 0,
            ppu_cycle: 0,
            suppress_vblank: false,
            nmi_line: false,
            io_latch: 0,
            io_latch_refreshed: [0; 8],
            rendering: false,
            bg: Background::default(),
            sprites: Sprites::default(),
            frame: Frame::new(),
        };
        ppu.refresh_mirroring();
        ppu
    }

    pub fn refresh_mirroring(&mut self) {
        let mirroring = self.mapper.borrow().mirroring();
        for (nt, base) in self.nametable_base.iter_mut().enumerate() {
            *base = mirror_nametable(mirroring, 0x2000 + nt as u16 * 0x400);
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

    pub fn chr_read(&self, addr: u16) -> u8 {
        self.mapper.borrow().ppu_read(addr)
    }

    pub fn mirroring(&self) -> Mirroring {
        self.mapper.borrow().mirroring()
    }

    /// The VRAM address register (loopy v), as used by $2007.
    pub fn vram_addr(&self) -> u16 {
        self.v & 0x3FFF
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

    pub fn set_region(&mut self, region: Region) {
        self.region = region;
        self.vblank_line = region.vblank_scanline();
        self.prerender_line = region.prerender_scanline();
        self.lines_per_frame = region.scanlines_per_frame();
        self.skips_odd_dot = region.skips_odd_frame_dot();
    }

    pub fn region(&self) -> Region {
        self.region
    }

    fn rendering_enabled(&self) -> bool {
        self.rendering
    }

    fn is_rendering(&self) -> bool {
        self.rendering_enabled()
            && (self.scanline < 240 || self.scanline == self.prerender_line)
    }

    #[inline]
    fn bus_address(&mut self, addr: u16) {
        if self.mapper_watches_bus {
            self.mapper.borrow_mut().ppu_address(addr, self.ppu_cycle);
        }
    }

    fn increment_vram_addr(&mut self) {
        if self.is_rendering() {
            self.increment_coarse_x();
            self.increment_y();
        } else {
            self.v = (self.v + self.ctrl.vram_addr_increment() as u16) & 0x7FFF;
            self.bus_address(self.v & 0x3FFF);
        }
    }

    /// Maps a nametable address ($2000-$3EFF) to an index into `vram`
    #[inline]
    pub fn mirror_vram_addr(&self, addr: u16) -> u16 {
        self.nametable_base[((addr >> 10) & 3) as usize] + (addr & 0x3FF)
    }

    pub fn tick(&mut self, dots: u8) -> bool {
        let mut new_frame = false;
        for _ in 0..dots {
            new_frame |= self.step_dot();
        }
        new_frame
    }

    /// Advances one dot and emulates it. Returns true when the frame has just
    /// finished (the pre-render line ended), with `frame` holding the picture.
    #[inline]
    pub fn step_dot(&mut self) -> bool {
        self.ppu_cycle += 1;
        let mut new_frame = false;
        if self.dot < 340 {
            self.dot += 1;
        } else {
            self.dot = 0;
            self.scanline += 1;
            if self.scanline == self.lines_per_frame {
                self.scanline = 0;
                self.odd_frame = !self.odd_frame;
                new_frame = true;
            }
        }

        let line = self.scanline;
        let prerender = self.prerender_line;
        if line < 240 || line == prerender {
            self.render_dot();
        }

        if self.dot == 1 {
            if line == self.vblank_line {
                if !self.suppress_vblank {
                    self.status.set_vblank_status(true);
                }
            } else if line == prerender {
                self.status.reset_vblank_status();
                self.status.set_sprite_zero_hit(false);
                self.status.set_sprite_overflow(false);
            }
            self.suppress_vblank = false;
        }

        if self.dot == 2 && (line == self.vblank_line || line == prerender) {
            self.update_nmi_line();
        }

        // NTSC drops the last dot of the pre-render line on odd frames when
        // rendering is on, so the frame is 89341.5 dots on average
        if line == prerender 
            && self.dot == 339
            && self.odd_frame
            && self.skips_odd_dot
            && self.rendering_enabled()
        {
            self.dot = 340;
        }
        self.rendering = self.mask.show_background() || self.mask.show_sprites();
        new_frame
    }

    fn update_nmi_line(&mut self) {
        self.nmi_line = self.status.is_in_vblank() && self.ctrl.generate_vblank_nmi();
    }

    pub fn nmi_line(&self) -> bool {
        self.nmi_line
    }

    pub fn is_odd_frame(&self) -> bool {
        self.odd_frame
    }

    /// A CPU read of PPU register `reg` ($2000 + reg). Bits the register
    /// doesn't drive come from the I/O latch; the bits it does drive refresh it.
    pub fn cpu_read(&mut self, reg: u16) -> u8 {
        match reg & 7 {
            2 => {
                let value = (self.read_status() & 0xE0) | (self.io_latch() & 0x1F);
                self.refresh_io_latch(value, 0xE0);
                value
            }
            4 => {
                let value = self.read_oam_data();
                self.refresh_io_latch(value, 0xFF);
                value
            }
            7 => {
                let palette = self.vram_addr() >= 0x3F00;
                let value = self.read_data();
                if palette {
                    let value = (value & 0x3F) | (self.io_latch() & 0xC0);
                    self.refresh_io_latch(value, 0x3F);
                    value
                } else {
                    self.refresh_io_latch(value, 0xFF);
                    value
                }
            }
            _ => self.io_latch()
        }
    }

    pub fn cpu_write(&mut self, reg: u16, value: u8) {
        self.refresh_io_latch(value, 0xFF);
        match reg & 7 {
            0 => self.write_to_ctrl(value),
            1 => self.write_to_mask(value),
            2 => {}
            3 => self.write_to_oam_addr(value),
            4 => self.write_to_oam_data(value),
            5 => self.write_to_scroll(value),
            6 => self.write_to_ppu_addr(value),
            _ => self.write_to_data(value),
        }
    }

    /// The I/O latch, with bits not refreshed for ~600 ms decayed to 0.
    fn io_latch(&mut self) -> u8 {
        for bit in 0..8 {
            if self.ppu_cycle - self.io_latch_refreshed[bit] > IO_LATCH_DECAY_DOTS {
                self.io_latch &= !(1 << bit);
            }
        }
        self.io_latch
    }

    /// Sets the latch bits in `mask` to `value` and restarts their decay.
    fn refresh_io_latch(&mut self, value: u8, mask: u8) {
        self.io_latch = (self.io_latch & !mask) | (value & mask);
        for bit in 0..8 {
            if mask & (1 << bit) != 0 {
                self.io_latch_refreshed[bit] = self.ppu_cycle;
            }
        }
    }
}

/// About 600 ms of PPU dots (NTSC 5.37 MHz; PAL's 5.32 MHz is close enough
/// given how much real consoles vary).
const IO_LATCH_DECAY_DOTS: u64 = 3_200_000;

/// OAM byte 2 of each sprite (the attributes) has no bits 2-4: they always
/// read back as 0.
fn oam_byte(index: u8, value: u8) -> u8 {
    if index & 3 == 2 { value & 0xE3 } else { value }
}

/// Maps a nametable address ($2000-$3EFF) to an index into the PPU's 4 KiB
/// of nametable memory (2 KiB on the console; four-screen carts add 2 KiB).
fn mirror_nametable(mirroring: Mirroring, addr: u16) -> u16 {
    let vram_index = addr & 0x0FFF;
    let name_table = vram_index / 0x400;
    let offset = vram_index & 0x3FF;
    match (mirroring, name_table) {
        (Mirroring::Vertical, 2) | (Mirroring::Vertical, 3) => vram_index - 0x800,
        (Mirroring::Horizontal, 1) | (Mirroring::Horizontal, 2) => vram_index - 0x400,
        (Mirroring::Horizontal, 3) => vram_index - 0x800,
        (Mirroring::SingleScreenLower, _) => offset,
        (Mirroring::SingleScreenUpper, _) => 0x400 + offset,
        _ => vram_index,
    }
}


fn palette_index(addr: u16) -> usize {
    let index = (addr & 0x1F) as usize;
    if index & 0x13 == 0x10 {
        index & 0x0F
    } else {
        index
    }
}

impl PPU for NesPPU {
    fn write_to_ppu_addr(&mut self, value: u8) {
        if !self.write_latch {
            // t: .CDEFGH ........ <- d: ..CDEFGH, and bit 14 is cleared
            self.t = (self.t & 0x00FF) | ((value as u16 & 0x3F) << 8);
        } else {
            self.t = (self.t & 0x7F00) | value as u16;
            self.v = self.t;
            if !self.is_rendering() {
                self.bus_address(self.v & 0x3FFF);
            }
        }
        self.write_latch = !self.write_latch;
    }

    fn write_to_ctrl(&mut self, value: u8) {
        self.ctrl.update(value);
        // t: ...GH.. ........ <- d: ......GH (nametable select)
        self.t = (self.t & !0x0C00) | ((value as u16 & 0b11) << 10);
        self.update_nmi_line();
    }

    fn write_to_mask(&mut self, value: u8) {
        self.mask.update(value);
    }

    fn write_to_oam_addr(&mut self, value: u8) {
        self.oam_addr = value;
    }

    fn write_to_oam_data(&mut self, value: u8) {
        self.oam_data[self.oam_addr as usize] = oam_byte(self.oam_addr, value);
        self.oam_addr = self.oam_addr.wrapping_add(1);
    }

    fn write_to_scroll(&mut self, value: u8) {
        if !self.write_latch {
            // t: ....... ...ABCDE <- d: ABCDE...; x <- d: .....FGH
            self.t = (self.t & !0x001F) | (value as u16 >> 3);
            self.fine_x = value & 0b111;
        } else {
            // t: FGH..AB CDE..... <- d: ABCDEFGH
            self.t = (self.t & !0x73E0)
                | ((value as u16 & 0b111) << 12)
                | ((value as u16 & 0xF8) << 2);
        }
        self.write_latch = !self.write_latch;
    }

    fn write_to_data(&mut self, value: u8) {
        let addr = self.vram_addr();
        match addr {
            // CHR-RAM; the mapper ignores the write if the cartridge has CHR-ROM
            0..=0x1fff => self.mapper.borrow_mut().ppu_write(addr, value),
            // 0x3000..=0x3eff mirrors 0x2000..=0x2eff; mirror_vram_addr folds it down
            0x2000..=0x3eff => {
                self.vram[self.mirror_vram_addr(addr) as usize] = value;
            }
            // palette RAM is 6 bits wide, so only colours $00-$3F can be stored.
            // palette_index handles the $3F10/$3F14/$3F18/$3F1C mirrors.
            _ => self.palette_table[palette_index(addr)] = value & 0x3F
        }
        self.increment_vram_addr();
    }

    fn write_oam_dma(&mut self, data: &[u8; 256]) {
        for x in data.iter() {
            self.oam_data[self.oam_addr as usize] = oam_byte(self.oam_addr, *x);
            self.oam_addr = self.oam_addr.wrapping_add(1);
        }
    }

    fn read_data(&mut self) -> u8 {
        let addr = self.vram_addr();
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
            // palette reads aren't buffered, but the buffer is filled with the
            // nametable byte "underneath" the palette
            _ => {
                self.internal_data_buf = self.vram[self.mirror_vram_addr(addr) as usize];
                self.palette_table[palette_index(addr)]
            }
        }
    }

    fn read_status(&mut self) -> u8 {
        if self.scanline == self.vblank_line && self.dot == 0 {
            // one dot before the flag is set: it reads as clear and won't be set
            self.suppress_vblank = true;
        }
        let data = self.status.snapshot();
        self.status.reset_vblank_status();
        self.update_nmi_line();
        self.write_latch = false;
        data
    }

    fn read_oam_data(&self) -> u8 {
        self.oam_data[self.oam_addr as usize]
    }
}

#[cfg(test)]
mod tests;