use crate::state::{StateReader, StateWriter};

/// The IRQ counter Konami put in the VRC4, VRC6 and VRC7. It runs off the
/// CPU clock: an 8-bit counter counts up from a reload value (the latch)
/// and raises the IRQ when it overflows. In scanline mode a prescaler
/// clocks it every 113⅔ CPU cycles, about once per NTSC scanline; in cycle
/// mode it counts every CPU cycle.
#[derive(Default)]
pub struct VrcIrq {
    latch: u8,
    counter: u8,
    /// Counts down by 3 every CPU cycle from 341; the counter is clocked
    /// each time it reaches 0 (114, 114, then 113 cycles).
    prescaler: i16,
    /// Control bits: A (enable again after acknowledge), E (enabled),
    /// M (cycle mode).
    enable_after_ack: bool,
    enabled: bool,
    cycle_mode: bool,
    pending: bool,
}

impl VrcIrq {
    pub fn write_latch(&mut self, data: u8) {
        self.latch = data;
    }

    pub fn write_latch_low(&mut self, data: u8) {
        self.latch = (self.latch & 0xF0) | (data & 0x0F);
    }

    pub fn write_latch_high(&mut self, data: u8) {
        self.latch = (self.latch & 0x0F) | ((data & 0x0F) << 4);
    }

    /// Control: acknowledges, sets the mode, and with E set restarts the
    /// prescaler and reloads the counter.
    pub fn write_control(&mut self, data: u8) {
        self.pending = false;
        self.enable_after_ack = data & 1 != 0;
        self.enabled = data & 2 != 0;
        self.cycle_mode = data & 4 != 0;
        self.prescaler = 341;
        if self.enabled {
            self.counter = self.latch;
        }
    }

    pub fn acknowledge(&mut self) {
        self.pending = false;
        self.enabled = self.enable_after_ack;
    }

    pub fn clock(&mut self) {
        if !self.enabled {
            return;
        }
        if self.cycle_mode {
            self.clock_counter();
        } else {
            self.prescaler -= 3;
            if self.prescaler <= 0 {
                self.prescaler += 341;
                self.clock_counter();
            }
        }
    }

    fn clock_counter(&mut self) {
        if self.counter == 0xFF {
            self.counter = self.latch;
            self.pending = true;
        } else {
            self.counter += 1;
        }
    }

    pub fn pending(&self) -> bool {
        self.pending
    }

    pub fn save_state(&self, w: &mut StateWriter) {
        w.u8(self.latch);
        w.u8(self.counter);
        w.u16(self.prescaler as u16);
        w.bool(self.enable_after_ack);
        w.bool(self.enabled);
        w.bool(self.cycle_mode);
        w.bool(self.pending);
    }

    pub fn load_state(&mut self, r: &mut StateReader) {
        self.latch = r.u8();
        self.counter = r.u8();
        self.prescaler = (r.u16() as i16).clamp(-2, 341);
        self.enable_after_ack = r.bool();
        self.enabled = r.bool();
        self.cycle_mode = r.bool();
        self.pending = r.bool();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_cycle_mode_counts_to_overflow() {
        let mut irq = VrcIrq::default();
        irq.write_latch(0xFD);
        irq.write_control(0b110); // enabled, cycle mode
        irq.clock();
        irq.clock();
        assert!(!irq.pending());
        irq.clock(); // $FF -> reload and IRQ
        assert!(irq.pending());
        irq.acknowledge();
        assert!(!irq.pending());
        irq.clock();
        assert!(!irq.pending(), "acknowledge copied A (0) into E: stopped");
    }

    #[test]
    fn test_scanline_mode_prescaler() {
        let mut irq = VrcIrq::default();
        irq.write_latch(0xFE);
        irq.write_control(0b010);
        let mut cycles = 0;
        while !irq.pending() {
            irq.clock();
            cycles += 1;
        }
        assert_eq!(cycles, 114 + 114, "two counter clocks: 114 cycles each");
        irq.write_latch(0xFD);
        irq.write_control(0b010);
        for _ in 0..114 + 114 + 112 {
            irq.clock();
        }
        assert!(!irq.pending());
        irq.clock();
        assert!(irq.pending(), "three clocks: 114 + 114 + 113 cycles");
    }
}