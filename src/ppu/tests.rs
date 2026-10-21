use super::*;

fn set_addr(ppu: &mut NesPPU, addr: u16) {
    ppu.write_to_ppu_addr((addr >> 8) as u8);
    ppu.write_to_ppu_addr(addr as u8);
    ppu.tick(PPU_ADDR_DELAY_DOTS as u8); // v changes a few dots later
}

/// A CPU read of $2007, then a few dots for the PPU to refill the read
/// buffer and move v on.
fn read(ppu: &mut NesPPU) -> u8 {
    let value = ppu.cpu_read(7);
    ppu.tick(6);
    value
}

#[test]
fn test_ppu_vram_writes() {
    let mut ppu = NesPPU::new_empty_rom();
    set_addr(&mut ppu, 0x2305);
    ppu.write_to_data(0x66);

    assert_eq!(ppu.vram[0x0305], 0x66);
}

#[test]
fn test_ppu_vram_reads() {
    let mut ppu = NesPPU::new_empty_rom();
    ppu.write_to_ctrl(0);
    ppu.vram[0x0305] = 0x66;

    set_addr(&mut ppu, 0x2305);

    read(&mut ppu); //load_into_buffer
    assert_eq!(ppu.vram_addr(), 0x2306);
    assert_eq!(read(&mut ppu), 0x66);
}

#[test]
fn test_ppu_vram_reads_cross_page() {
    let mut ppu = NesPPU::new_empty_rom();
    ppu.write_to_ctrl(0);
    ppu.vram[0x01ff] = 0x66;
    ppu.vram[0x0200] = 0x77;

    set_addr(&mut ppu, 0x21ff);

    read(&mut ppu); //load_into_buffer
    assert_eq!(read(&mut ppu), 0x66);
    assert_eq!(read(&mut ppu), 0x77);
}

#[test]
fn test_ppu_vram_reads_step_32() {
    let mut ppu = NesPPU::new_empty_rom();
    ppu.write_to_ctrl(0b100);
    ppu.vram[0x01ff] = 0x66;
    ppu.vram[0x01ff + 32] = 0x77;
    ppu.vram[0x01ff + 64] = 0x88;

    set_addr(&mut ppu, 0x21ff);

    read(&mut ppu); //load_into_buffer
    assert_eq!(read(&mut ppu), 0x66);
    assert_eq!(read(&mut ppu), 0x77);
    assert_eq!(read(&mut ppu), 0x88);
}

// Horizontal: https://www.nesdev.org/wiki/Mirroring
//   [0x2000 A ] [0x2400 a ]
//   [0x2800 B ] [0x2C00 b ]
#[test]
fn test_vram_horizontal_mirror() {
    let mut ppu = NesPPU::new_empty_rom();
    set_addr(&mut ppu, 0x2405);
    ppu.write_to_data(0x66); //write to a

    set_addr(&mut ppu, 0x2805);
    ppu.write_to_data(0x77); //write to B

    set_addr(&mut ppu, 0x2005);
    read(&mut ppu); //load into buffer
    assert_eq!(read(&mut ppu), 0x66); //read from A

    set_addr(&mut ppu, 0x2C05);
    read(&mut ppu); //load into buffer
    assert_eq!(read(&mut ppu), 0x77); //read from b
}

// Vertical: https://www.nesdev.org/wiki/Mirroring
//   [0x2000 A ] [0x2400 B ]
//   [0x2800 a ] [0x2C00 b ]
#[test]
fn test_vram_vertical_mirror() {
    let mut ppu = NesPPU::with_mirroring(Mirroring::Vertical);

    set_addr(&mut ppu, 0x2005);
    ppu.write_to_data(0x66); //write to A

    set_addr(&mut ppu, 0x2C05);
    ppu.write_to_data(0x77); //write to b

    set_addr(&mut ppu, 0x2805);
    read(&mut ppu); //load into buffer
    assert_eq!(read(&mut ppu), 0x66); //read from a

    set_addr(&mut ppu, 0x2405);
    read(&mut ppu); //load into buffer
    assert_eq!(read(&mut ppu), 0x77); //read from B
}

#[test]
fn test_read_status_resets_latch() {
    let mut ppu = NesPPU::new_empty_rom();
    ppu.vram[0x0305] = 0x66;

    ppu.write_to_ppu_addr(0x21);
    ppu.write_to_ppu_addr(0x23);
    ppu.write_to_ppu_addr(0x05);

    read(&mut ppu); //load_into_buffer
    assert_ne!(read(&mut ppu), 0x66);

    ppu.read_status();

    set_addr(&mut ppu, 0x2305);

    read(&mut ppu); //load_into_buffer
    assert_eq!(read(&mut ppu), 0x66);
}

#[test]
fn test_ppu_vram_mirroring() {
    let mut ppu = NesPPU::new_empty_rom();
    ppu.write_to_ctrl(0);
    ppu.vram[0x0305] = 0x66;

    set_addr(&mut ppu, 0x6305); //0x6305 -> 0x2305

    read(&mut ppu); //load into_buffer
    assert_eq!(read(&mut ppu), 0x66);
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
    set_addr(&mut ppu, 0x2C05);
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
    set_addr(&mut ppu, 0x0123);
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
    assert_eq!(ppu.sprite_row_addr(0x04, 15), 0x50 + 7);
}

#[test]
fn test_palette_stores_only_6_bits() {
    let mut ppu = NesPPU::new_empty_rom();
    set_addr(&mut ppu, 0x3F11);
    ppu.write_to_data(0xF0);
    assert_eq!(ppu.palette_table[0x11], 0x30);
}

#[test]
fn test_palette_mirror_write_is_masked() {
    let mut ppu = NesPPU::new_empty_rom();
    set_addr(&mut ppu, 0x3F10); // mirror of $3F00
    ppu.write_to_data(0xFF);
    assert_eq!(ppu.palette_table[0x00], 0x3F);
}

#[test]
fn test_ppu_data_read_fills_the_buffer_a_few_dots_later() {
    let mut ppu = NesPPU::new_empty_rom();
    ppu.vram[0x0305] = 0x66;
    set_addr(&mut ppu, 0x2305);
    ppu.cpu_read(7);
    assert_eq!(ppu.internal_data_buf, 0, "not yet");
    assert_eq!(ppu.vram_addr(), 0x2305);
    ppu.tick(DATA_READ_ALE_DOTS as u8 + 2);
    assert_eq!(ppu.internal_data_buf, 0x66);
    assert_eq!(ppu.vram_addr(), 0x2306);
}

#[test]
fn test_palette_mirrors() {
    let mut ppu = NesPPU::new_empty_rom();
    set_addr(&mut ppu, 0x3F30); // mirror of $3F10, which mirrors $3F00
    ppu.write_to_data(0x21);
    assert_eq!(ppu.palette_table[0x00], 0x21);

    set_addr(&mut ppu, 0x3F10);
    assert_eq!(read(&mut ppu), 0x21);
}

// $2005/$2006 write into t, exactly as on https://www.nesdev.org/wiki/PPU_scrolling
#[test]
#[allow(clippy::unusual_byte_groupings)] // t's fields: fine Y, nametable, coarse Y, coarse X
fn test_scroll_and_addr_writes_share_t() {
    let mut ppu = NesPPU::new_empty_rom();
    ppu.write_to_ctrl(0b10); // nametable 2
    ppu.write_to_scroll(0b0111_1101); // coarse X 15, fine X 5
    ppu.write_to_scroll(0b0101_1110); // coarse Y 11, fine Y 6
    assert_eq!(ppu.t, 0b110_10_01011_01111);
    assert_eq!(ppu.fine_x, 5);

    ppu.write_to_ppu_addr(0x3D); // bit 14 cleared, high bits replaced
    ppu.write_to_ppu_addr(0xF0);
    assert_eq!(ppu.t, 0x3DF0);
    ppu.tick(PPU_ADDR_DELAY_DOTS as u8 - 1);
    assert_ne!(ppu.v, 0x3DF0, "v changes a few dots later");
    ppu.tick(1);
    assert_eq!(ppu.v, 0x3DF0);
    assert_eq!(ppu.fine_x, 5, "$2006 leaves fine X alone");
}

#[test]
fn test_palette_read_fills_buffer_from_nametable() {
    let mut ppu = NesPPU::with_mirroring(Mirroring::Vertical);
    let underneath = ppu.mirror_vram_addr(0x2F05) as usize; // $2F05 is "underneath" $3F05
    ppu.vram[underneath] = 0x42;
    ppu.palette_table[5] = 0x11;
    set_addr(&mut ppu, 0x3F05);
    assert_eq!(read(&mut ppu), 0x11);
    set_addr(&mut ppu, 0x0000);
    assert_eq!(read(&mut ppu), 0x42);
}

pub(crate) fn counting_ppu() -> (
    NesPPU,
    std::rc::Rc<std::cell::RefCell<crate::mapper::test_util::ScanlineCounter>>,
) {
    use crate::mapper::test_util::ScanlineCounter;
    let cart = std::rc::Rc::new(std::cell::RefCell::new(ScanlineCounter::new(
        vec![0; 0x8000],
        None,
    )));
    (NesPPU::new(cart.clone()), cart)
}

fn run_frame(ppu: &mut NesPPU) {
    while !ppu.tick(1) {}
}

#[test]
fn test_a12_rises_once_per_rendered_line() {
    let (mut ppu, cart) = counting_ppu();
    ppu.write_to_ctrl(0b0000_1000); // sprites at $1000, background at $0000
    ppu.write_to_mask(0b0000_1000); // show background
    run_frame(&mut ppu);
    run_frame(&mut ppu);
    // 240 visible lines + the pre-render line
    let first = cart.borrow().rises_at.clone();
    assert_eq!(first.len(), 2 * 241);
    // each rise is at the first sprite pattern fetch: dot 262
    assert!(first.iter().all(|&(_, dot)| dot == 262));
}

#[test]
fn test_a12_not_toggled_when_rendering_off() {
    let (mut ppu, cart) = counting_ppu();
    ppu.write_to_ctrl(0b0000_1000);
    run_frame(&mut ppu);
    assert_eq!(cart.borrow().scanlines, 0);
}

#[test]
fn test_mid_line_palette_change_shows_from_that_dot() {
    use crate::render::palette::SYSTEM_PALETTE;
    // blank CHR, so every background pixel is the backdrop colour
    let (mut ppu, _cart) = counting_ppu();
    ppu.write_to_mask(0b0000_1010);
    ppu.palette_table[0] = 0x0F;
    while !(ppu.scanline == 100 && ppu.dot == 128) {
        ppu.tick(1);
    }
    ppu.palette_table[0] = 0x16; // change the backdrop mid-line
    run_frame(&mut ppu);

    let pixel = |x: usize, y: usize| {
        let i = (y * 256 + x) * 3;
        (
            ppu.frame.data[i],
            ppu.frame.data[i + 1],
            ppu.frame.data[i + 2],
        )
    };
    assert_eq!(pixel(50, 50), SYSTEM_PALETTE[0x0F]);
    assert_eq!(pixel(126, 100), SYSTEM_PALETTE[0x0F]); // output at dot 127
    assert_eq!(pixel(128, 100), SYSTEM_PALETTE[0x16]); // output at dot 129
    assert_eq!(pixel(50, 150), SYSTEM_PALETTE[0x16]);
}

/// PPU dots from the start of a frame to the end of it, and to vblank.
fn measure_frame(region: Region, rendering: bool) -> (usize, usize) {
    let (mut ppu, _cart) = counting_ppu();
    ppu.set_region(region);
    if rendering {
        ppu.write_to_mask(0b0000_1000);
    }
    run_frame(&mut ppu);
    let (mut dots, mut vblank_at) = (0, 0);
    loop {
        dots += 1;
        let done = ppu.tick(1);
        if vblank_at == 0 && ppu.status.is_in_vblank() {
            vblank_at = dots;
        }
        if done {
            return (dots, vblank_at);
        }
    }
}

#[test]
fn test_frame_lengths_by_region() {
    assert_eq!(measure_frame(Region::Ntsc, false).0, 341 * 262);
    assert_eq!(measure_frame(Region::Pal, false).0, 341 * 312);
    assert_eq!(measure_frame(Region::Dendy, false).0, 341 * 312);
    // no odd-frame skip outside NTSC, even with rendering on
    assert_eq!(measure_frame(Region::Pal, true).0, 341 * 312);
}

#[test]
fn test_ntsc_odd_frames_skip_a_dot() {
    let (mut ppu, _cart) = counting_ppu();
    ppu.write_to_mask(0b0000_1000);
    let mut lengths = vec![];
    for _ in 0..4 {
        let mut dots = 0;
        loop {
            dots += 1;
            if ppu.tick(1) {
                break;
            }
        }
        lengths.push(dots);
    }
    lengths.sort();
    assert_eq!(
        lengths,
        vec![341 * 262 - 1, 341 * 262 - 1, 341 * 262, 341 * 262]
    );
}

#[test]
fn test_vblank_start_by_region() {
    // the frame starts at line 0 dot 0; vblank starts at dot 1 of line
    // 241 (NTSC/PAL) or 291 (Dendy)
    assert_eq!(measure_frame(Region::Ntsc, false).1, 341 * 241 + 1);
    assert_eq!(measure_frame(Region::Pal, false).1, 341 * 241 + 1);
    assert_eq!(measure_frame(Region::Dendy, false).1, 341 * 291 + 1);
}

#[test]
fn test_status_read_just_before_vblank_suppresses_it() {
    let (mut ppu, _cart) = counting_ppu();
    ppu.write_to_ctrl(0x80);
    while !(ppu.scanline == 241 && ppu.dot == 0) {
        ppu.tick(1);
    }
    assert_eq!(ppu.read_status() & 0x80, 0);
    ppu.tick(5);
    assert!(!ppu.status.is_in_vblank());
    assert!(!ppu.nmi_line());
}

#[test]
fn test_nmi_line_follows_vblank_and_enable() {
    let (mut ppu, _cart) = counting_ppu();
    while !ppu.status.is_in_vblank() {
        ppu.tick(1);
    }
    assert!(!ppu.nmi_line());
    ppu.write_to_ctrl(0x80); // enabling NMI during vblank raises it at once
    assert!(ppu.nmi_line());
    ppu.read_status(); // and reading $2002 drops it with the flag
    assert!(!ppu.nmi_line());
}

#[test]
fn test_a12_rises_on_pal_prerender_line() {
    let (mut ppu, cart) = counting_ppu();
    ppu.set_region(Region::Pal);
    ppu.write_to_ctrl(0b0000_1000);
    ppu.write_to_mask(0b0000_1000);
    run_frame(&mut ppu);
    assert_eq!(cart.borrow().scanlines, 241);
}

#[test]
fn test_reset_ignores_some_writes_until_vblank_ends() {
    let mut ppu = NesPPU::new_empty_rom();
    ppu.cpu_write(0, 0x80);
    ppu.cpu_write(1, 0x1E);
    ppu.reset();
    assert_eq!(ppu.ctrl.bits(), 0);
    assert_eq!(ppu.mask.bits(), 0);

    // $2000, $2001, $2005 and $2006 are ignored...
    ppu.cpu_write(0, 0x80);
    ppu.cpu_write(1, 0x1E);
    ppu.cpu_write(6, 0x21);
    assert_eq!((ppu.ctrl.bits(), ppu.mask.bits()), (0, 0));
    assert!(!ppu.write_latch, "the write toggle doesn't flip");
    // ...but $2003, $2004 and $2007 work
    ppu.cpu_write(3, 0x10);
    ppu.cpu_write(4, 0xAB);
    assert_eq!(ppu.oam_data[0x10], 0xAB);
    let addr = ppu.vram_addr();
    ppu.cpu_write(7, 0x55);
    assert_ne!(ppu.vram_addr(), addr);

    // the reset signal clears at dot 1 of the pre-render line
    while !(ppu.scanline == ppu.prerender_line && ppu.dot == 1) {
        ppu.tick(1);
    }
    ppu.cpu_write(0, 0x80);
    assert_eq!(ppu.ctrl.bits(), 0x80);
}

#[test]
fn test_palette_reads_honour_greyscale() {
    let mut ppu = NesPPU::new_empty_rom();
    set_addr(&mut ppu, 0x3F1C);
    ppu.write_to_data(0x1A);
    ppu.write_to_mask(0x01); // greyscale on
    set_addr(&mut ppu, 0x3F1C);
    assert_eq!(read(&mut ppu), 0x10, "the low four bits read as 0");
    ppu.write_to_data(0x16); // writes keep all six bits
    ppu.write_to_mask(0x00);
    set_addr(&mut ppu, 0x3F1D);
    assert_eq!(read(&mut ppu), 0x16);
}
