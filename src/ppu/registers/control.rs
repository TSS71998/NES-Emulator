bitflags! {
    /// PPUCTRL ($2000). The nametable bits go into `t` (see NesPPU).
    pub struct ControlRegister: u8 {
        const NAMETABLE1              = 0b0000_0001;
        const NAMETABLE2              = 0b0000_0010;
        const VRAM_ADD_INCREMENT      = 0b0000_0100;
        const SPRITE_PATTERN_ADDR     = 0b0000_1000;
        const BACKGROUND_PATTERN_ADDR = 0b0001_0000;
        const SPRITE_SIZE             = 0b0010_0000;
        const MASTER_SLAVE_SELECT     = 0b0100_0000;
        const GENERATE_NMI            = 0b1000_0000;
    }
}

impl ControlRegister {
    pub fn new() -> Self {
        ControlRegister::empty()
    }

    /// How far $2007 accesses move v: 1 (across) or 32 (down).
    pub fn vram_addr_increment(&self) -> u8 {
        if self.contains(ControlRegister::VRAM_ADD_INCREMENT) {
            32
        } else {
            1
        }
    }

    /// The pattern table 8x8 sprites use.
    pub fn sprt_pattern_addr(&self) -> u16 {
        if self.contains(ControlRegister::SPRITE_PATTERN_ADDR) {
            0x1000
        } else {
            0
        }
    }

    pub fn bknd_pattern_addr(&self) -> u16 {
        if self.contains(ControlRegister::BACKGROUND_PATTERN_ADDR) {
            0x1000
        } else {
            0
        }
    }

    /// Sprite height: 8 or 16.
    pub fn sprite_size(&self) -> u8 {
        if self.contains(ControlRegister::SPRITE_SIZE) {
            16
        } else {
            8
        }
    }

    pub fn generate_vblank_nmi(&self) -> bool {
        self.contains(ControlRegister::GENERATE_NMI)
    }

    pub fn update(&mut self, data: u8) {
        self.bits = data;
    }
}
