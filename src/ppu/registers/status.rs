bitflags! {
    /// PPUSTATUS ($2002). The low 5 bits aren't driven: a read returns the
    /// PPU's I/O latch there (see NesPPU::cpu_read).
    pub struct StatusRegister: u8 {
        const SPRITE_OVERFLOW = 0b0010_0000;
        const SPRITE_ZERO_HIT = 0b0100_0000;
        const VBLANK_STARTED  = 0b1000_0000;
    }
}

impl StatusRegister {
    pub fn new() -> Self {
        StatusRegister::empty()
    }

    pub fn set_vblank_status(&mut self, status: bool) {
        self.set(StatusRegister::VBLANK_STARTED, status);
    }

    pub fn set_sprite_zero_hit(&mut self, status: bool) {
        self.set(StatusRegister::SPRITE_ZERO_HIT, status);
    }

    pub fn set_sprite_overflow(&mut self, status: bool) {
        self.set(StatusRegister::SPRITE_OVERFLOW, status);
    }

    pub fn reset_vblank_status(&mut self) {
        self.remove(StatusRegister::VBLANK_STARTED);
    }

    pub fn is_in_vblank(&self) -> bool {
        self.contains(StatusRegister::VBLANK_STARTED)
    }

    pub fn snapshot(&self) -> u8 {
        self.bits
    }
}
