use crate::state::{StateReader, StateWriter};

bitflags! {
    /// The buttons, in the order the controller reports them (A first).
    pub struct JoypadButton: u8 {
        const RIGHT    = 0b1000_0000;
        const LEFT     = 0b0100_0000;
        const DOWN     = 0b0010_0000;
        const UP       = 0b0001_0000;
        const START    = 0b0000_1000;
        const SELECT   = 0b0000_0100;
        const BUTTON_B = 0b0000_0010;
        const BUTTON_A = 0b0000_0001;
    }
}

/// A standard controller: an 8-bit shift register loaded with the buttons
/// while the strobe line ($4016 bit 0) is high, read out one bit at a time.
pub struct Joypad {
    strobe: bool,
    button_index: u8,
    button_status: JoypadButton,
    /// The Famicom's controller 2 microphone (read on $4016 bit 2).
    pub microphone: bool,
}

impl Default for Joypad {
    fn default() -> Self {
        Self::new()
    }
}

impl Joypad {
    pub fn new() -> Self {
        Joypad {
            strobe: false,
            button_index: 0,
            button_status: JoypadButton::empty(),
            microphone: false,
        }
    }

    /// The strobe line: while it's high the controller keeps reloading, so
    /// reads return button A.
    pub fn write(&mut self, data: u8) {
        self.strobe = data & 1 == 1;
        if self.strobe {
            self.button_index = 0;
        }
    }

    /// The next button on bit 0 (1 = pressed). After all 8, an official
    /// controller returns 1s.
    pub fn read(&mut self) -> u8 {
        if self.button_index > 7 {
            return 1;
        }
        let bit = (self.button_status.bits >> self.button_index) & 1;
        if !self.strobe {
            self.button_index += 1;
        }
        bit
    }

    pub fn set_button_pressed_status(&mut self, button: JoypadButton, pressed: bool) {
        self.button_status.set(button, pressed);
    }

    pub fn set_buttons(&mut self, buttons: JoypadButton) {
        self.button_status = buttons;
    }
}

impl Joypad {
    pub fn save_state(&self, w: &mut StateWriter) {
        w.bool(self.strobe);
        w.u8(self.button_index);
        w.u8(self.button_status.bits());
        w.bool(self.microphone);
    }

    pub fn load_state(&mut self, r: &mut StateReader) {
        self.strobe = r.bool();
        self.button_index = r.u8();
        self.button_status = JoypadButton::from_bits_truncate(r.u8());
        self.microphone = r.bool();
    }
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_strobe_mode() {
        let mut joypad = Joypad::new();
        joypad.write(1);
        joypad.set_button_pressed_status(JoypadButton::BUTTON_A, true);
        for _x in 0..10 {
            assert_eq!(joypad.read(), 1);
        }
    }

    #[test]
    fn test_strobe_mode_on_off() {
        let mut joypad = Joypad::new();

        joypad.write(0);
        joypad.set_button_pressed_status(JoypadButton::RIGHT, true);
        joypad.set_button_pressed_status(JoypadButton::LEFT, true);
        joypad.set_button_pressed_status(JoypadButton::SELECT, true);
        joypad.set_button_pressed_status(JoypadButton::BUTTON_B, true);

        for _ in 0..=1 {
            assert_eq!(joypad.read(), 0);
            assert_eq!(joypad.read(), 1);
            assert_eq!(joypad.read(), 1);
            assert_eq!(joypad.read(), 0);
            assert_eq!(joypad.read(), 0);
            assert_eq!(joypad.read(), 0);
            assert_eq!(joypad.read(), 1);
            assert_eq!(joypad.read(), 1);

            for _x in 0..10 {
                assert_eq!(joypad.read(), 1);
            }
            joypad.write(1);
            joypad.write(0);
        }
    }
}
