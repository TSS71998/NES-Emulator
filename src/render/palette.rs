#[rustfmt::skip]
use std::sync::LazyLock;

/// SYSTEM_PALETTE for every combination of PPUMASK's color emphasis bits:
/// entry `emphasis << 6 | color`, where emphasis bit 0 is red, bit 1 green
/// and bit 2 blue (PPUMASK bits 5-7 on NTSC). Entries 0-63 are
/// SYSTEM_PALETTE itself.
pub static EMPHASIS_PALETTE: LazyLock<[(u8, u8, u8); 512]> = LazyLock::new(|| {
    let mut palette = [(0, 0, 0); 512];
    for color in 0..64 {
        let plain = ntsc_rgb(color, 0);
        let base = SYSTEM_PALETTE[color];
        for emphasis in 0..8 {
            let tinted = ntsc_rgb(color, emphasis);
            let channel = |base: u8, k: usize| {
                (base as f64 + 255.0 * (tinted[k] - plain[k])).round().clamp(0.0, 255.0) as u8
            };
            palette[emphasis << 6 | color] = (channel(base.0, 0), channel(base.1, 1), channel(base.2, 2));
        }
    }
    palette
});

/// The color the 2C02's composite signal decodes to, as RGB from 0 to 1,
/// following https://www.nesdev.org/wiki/NTSC_video. Each color is a square
/// wave between two voltages over the 12 phases of the colur subcarrier,
/// high for the 6 phases matching its hue. Each emphasis bit attenuates the
/// signal during the 6 phases centred opposite its color, which pulls the
/// picture towards that color, and all three together darken it evenly.
fn ntsc_rgb(color: usize, emphasis: usize) -> [f64; 3] {
    const LOW: [f64; 4] = [0.350, 0.518, 0.962, 1.550];
    const HIGH: [f64; 4] = [1.094, 1.506, 1.962, 1.962];
    const BLACK: f64 = 0.518;
    const WHITE: f64 = 1.962;
    const ATTENUATION: f64 = 0.746;
    const HUE_PHASE: f64 = 3.8;

    let hue = color & 0x0F;
    let level = if hue > 13 { 1 } else { (color >> 4) & 3 };
    let mut low = LOW[level];
    let mut high = HIGH[level];
    if hue == 0 {
        low = high;
    }
    if hue > 12 {
        high = low;
    }
    let in_phase = |hue: usize, phase: usize| (hue + phase) % 12 < 6;

    let (mut y, mut i, mut q) = (0.0, 0.0, 0.0);
    for phase in 0..12 {
        let mut signal = if in_phase(hue, phase) { high } else { low };
        if (emphasis & 1 != 0 && in_phase(0, phase))
            || (emphasis & 2 != 0 && in_phase(4, phase))
            || (emphasis & 4 != 0 && in_phase(8, phase))
        {
            signal *= ATTENUATION;
        }
        let signal = (signal - BLACK) / (WHITE - BLACK);
        let angle = std::f64::consts::PI * (phase as f64 + HUE_PHASE) / 6.0;
        y += signal / 12.0;
        i += 2.0 * signal * angle.cos() / 12.0;
        q += 2.0 * signal * angle.sin() / 12.0;
    }
    [
        y + 0.946882 * i + 0.623557 * q,
        y - 0.274788 * i - 0.635691 * q,
        y - 1.108545 * i + 1.709007 * q,
    ]
}

pub static SYSTEM_PALETTE: [(u8,u8,u8); 64] = [
    (0x80, 0x80, 0x80), (0x00, 0x3D, 0xA6), (0x00, 0x12, 0xB0), (0x44, 0x00, 0x96), (0xA1, 0x00, 0x5E), 
    (0xC7, 0x00, 0x28), (0xBA, 0x06, 0x00), (0x8C, 0x17, 0x00), (0x5C, 0x2F, 0x00), (0x10, 0x45, 0x00), 
    (0x05, 0x4A, 0x00), (0x00, 0x47, 0x2E), (0x00, 0x41, 0x66), (0x00, 0x00, 0x00), (0x05, 0x05, 0x05), 
    (0x05, 0x05, 0x05), (0xC7, 0xC7, 0xC7), (0x00, 0x77, 0xFF), (0x21, 0x55, 0xFF), (0x82, 0x37, 0xFA), 
    (0xEB, 0x2F, 0xB5), (0xFF, 0x29, 0x50), (0xFF, 0x22, 0x00), (0xD6, 0x32, 0x00), (0xC4, 0x62, 0x00), 
    (0x35, 0x80, 0x00), (0x05, 0x8F, 0x00), (0x00, 0x8A, 0x55), (0x00, 0x99, 0xCC), (0x21, 0x21, 0x21), 
    (0x09, 0x09, 0x09), (0x09, 0x09, 0x09), (0xFF, 0xFF, 0xFF), (0x0F, 0xD7, 0xFF), (0x69, 0xA2, 0xFF), 
    (0xD4, 0x80, 0xFF), (0xFF, 0x45, 0xF3), (0xFF, 0x61, 0x8B), (0xFF, 0x88, 0x33), (0xFF, 0x9C, 0x12), 
    (0xFA, 0xBC, 0x20), (0x9F, 0xE3, 0x0E), (0x2B, 0xF0, 0x35), (0x0C, 0xF0, 0xA4), (0x05, 0xFB, 0xFF), 
    (0x5E, 0x5E, 0x5E), (0x0D, 0x0D, 0x0D), (0x0D, 0x0D, 0x0D), (0xFF, 0xFF, 0xFF), (0xA6, 0xFC, 0xFF), 
    (0xB3, 0xEC, 0xFF), (0xDA, 0xAB, 0xEB), (0xFF, 0xA8, 0xF9), (0xFF, 0xAB, 0xB3), (0xFF, 0xD2, 0xB0), 
    (0xFF, 0xEF, 0xA6), (0xFF, 0xF7, 0x9C), (0xD7, 0xE8, 0x95), (0xA6, 0xED, 0xAF), (0xA2, 0xF2, 0xDA), 
    (0x99, 0xFF, 0xFC), (0xDD, 0xDD, 0xDD), (0x11, 0x11, 0x11), (0x11, 0x11, 0x11)
];

#[cfg(test)]
mod tests {
    use super::*;

    const RED: usize = 1;
    const GREEN: usize = 2;
    const BLUE: usize = 4;

    #[test]
    fn test_no_emphasis_is_the_system_palette() {
        assert_eq!(EMPHASIS_PALETTE[..64], SYSTEM_PALETTE[..]);
    }

    #[test]
    fn test_each_bit_tints_white_towards_its_colour() {
        let white = 0x30;
        let (r, g, b) = EMPHASIS_PALETTE[RED << 6 | white];
        assert!(r > g + 30 && r > b + 30, "red: {:?}", (r, g, b));
        let (r, g, b) = EMPHASIS_PALETTE[GREEN << 6 | white];
        assert!(g > r + 30 && g > b + 30, "green: {:?}", (r, g, b));
        let (r, g, b) = EMPHASIS_PALETTE[BLUE << 6 | white];
        assert!(b > r + 30 && b > g + 30, "blue: {:?}", (r, g, b));
    }

    #[test]
    fn test_all_bits_darken_grey_evenly() {
        for grey in [0x00, 0x10, 0x20, 0x30] {
            let (r, g, b) = EMPHASIS_PALETTE[7 << 6 | grey];
            let base = SYSTEM_PALETTE[grey];
            assert!(r < base.0 && g < base.1 && b < base.2, "{grey:#04x} is darker");
            assert!(r.abs_diff(g) <= 3 && g.abs_diff(b) <= 3, "{grey:#04x} stays grey: {:?}", (r, g, b));
        }
    }

    #[test]
    fn test_two_bits_tint_towards_the_mix() {
        // red + green leaves yellow's phases at full strength, so blue drops most
        let (r, g, b) = EMPHASIS_PALETTE[(RED | GREEN) << 6 | 0x30];
        assert!(b + 30 < r && b + 30 < g, "{:?}", (r, g, b));
    }
}