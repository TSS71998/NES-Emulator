pub struct ScrollRegister {
    pub scroll_x: u8,
    pub scroll_y: u8,
}

impl ScrollRegister {
    pub fn new() -> Self {
        ScrollRegister {
            scroll_x: 0,
            scroll_y: 0,
        }
    }

    pub fn write(&mut self, data: u8, first_write: bool) {
        if first_write {
            self.scroll_x = data;
        } else {
            self.scroll_y = data;
        }
    }

    // $2006 writes go through the same internal register as $2005 (loopy t),
    // so they also change the scroll. scroll_y is coarse_y << 3 | fine_y.

    // first $2006 write: bits 0-1 -> coarse Y bits 3-4, bits 4-5 -> fine Y bits 0-1 (fine Y bit 2 cleared)
    pub fn write_addr_hi(&mut self, data: u8) {
        self.scroll_y = ((data & 0b11) << 6) | (self.scroll_y & 0b0011_1000) | ((data >> 4) & 0b11);
    }

    // second $2006 write: bits 0-4 -> coarse X, bits 5-7 -> coarse Y bits 0-2
    pub fn write_addr_lo(&mut self, data: u8) {
        self.scroll_x = ((data & 0b1_1111) << 3) | (self.scroll_x & 0b111);
        self.scroll_y = (self.scroll_y & 0b1100_0111) | ((data >> 5) << 3);
    }
}