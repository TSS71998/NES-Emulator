bitflags! {
    /// PPUMASK ($2001).
    pub struct MaskRegister: u8 {
        const GREYSCALE                = 0b0000_0001;
        const LEFTMOST_8PXL_BACKGROUND = 0b0000_0010;
        const LEFTMOST_8PXL_SPRITE     = 0b0000_0100;
        const SHOW_BACKGROUND          = 0b0000_1000;
        const SHOW_SPRITES             = 0b0001_0000;
        const EMPHASISE_RED            = 0b0010_0000;
        const EMPHASISE_GREEN          = 0b0100_0000;
        const EMPHASISE_BLUE           = 0b1000_0000;
    }
}

impl MaskRegister {
    pub fn new() -> Self {
        MaskRegister::empty()
    }

    pub fn is_greyscale(&self) -> bool {
        self.contains(MaskRegister::GREYSCALE)
    }

    pub fn leftmost_8pxl_background(&self) -> bool {
        self.contains(MaskRegister::LEFTMOST_8PXL_BACKGROUND)
    }

    pub fn leftmost_8pxl_sprite(&self) -> bool {
        self.contains(MaskRegister::LEFTMOST_8PXL_SPRITE)
    }

    pub fn show_background(&self) -> bool {
        self.contains(MaskRegister::SHOW_BACKGROUND)
    }

    pub fn show_sprites(&self) -> bool {
        self.contains(MaskRegister::SHOW_SPRITES)
    }

    /// The emphasis bits as an index 0-7 (bit 0 red, 1 green, 2 blue). PAL
    /// and Dendy PPUs swap the red and green bits.
    pub fn emphasise(&self, swap_red_green: bool) -> usize {
        let bits = (self.bits >> 5) as usize;
        if swap_red_green {
            (bits & 0b100) | (bits & 0b001) << 1 | (bits & 0b010) >> 1
        } else {
            bits
        }
    }

    pub fn update(&mut self, data: u8) {
        self.bits = data;
    }
}
