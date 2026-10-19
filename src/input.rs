use crate::joypad::{Joypad, JoypadButton};
use sdl2::GameControllerSubsystem;
use sdl2::controller::{Axis, Button, GameController};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;

/// How far the left stick must be pushed (out of 32767) to count as a
/// d-pad press: half way.
const STICK_THRESHOLD: i16 = 16384;

const KEYS: [(Keycode, usize, JoypadButton); 16] = [
    (Keycode::Up, 0, JoypadButton::UP),
    (Keycode::Down, 0, JoypadButton::DOWN),
    (Keycode::Left, 0, JoypadButton::LEFT),
    (Keycode::Right, 0, JoypadButton::RIGHT),
    (Keycode::A, 0, JoypadButton::BUTTON_A),
    (Keycode::S, 0, JoypadButton::BUTTON_B),
    (Keycode::Return, 0, JoypadButton::START),
    (Keycode::Space, 0, JoypadButton::SELECT),
    (Keycode::I, 1, JoypadButton::UP),
    (Keycode::K, 1, JoypadButton::DOWN),
    (Keycode::J, 1, JoypadButton::LEFT),
    (Keycode::L, 1, JoypadButton::RIGHT),
    (Keycode::G, 1, JoypadButton::BUTTON_A),
    (Keycode::F, 1, JoypadButton::BUTTON_B),
    (Keycode::RShift, 1, JoypadButton::START),
    (Keycode::RCtrl, 1, JoypadButton::SELECT),
];

/// Held to speak into the Famicom's microphone (controller 2).
const MICROPHONE_KEY: Keycode = Keycode::M;

fn pad_button(button: Button) -> Option<JoypadButton> {
    Some(match button {
        Button::DPadUp => JoypadButton::UP,
        Button::DPadDown => JoypadButton::DOWN,
        Button::DPadLeft => JoypadButton::LEFT,
        Button::DPadRight => JoypadButton::RIGHT,
        Button::B | Button::Y => JoypadButton::BUTTON_A,
        Button::A | Button::X => JoypadButton::BUTTON_B,
        Button::Start => JoypadButton::START,
        Button::Back => JoypadButton::SELECT,
        _ => return None
    })
}

struct Pad {
    controller: GameController,
    player: usize,
    buttons: JoypadButton,
    stick: JoypadButton
}

pub struct Input {
    subsystem: Option<GameControllerSubsystem>,
    pads: Vec<Pad>,
    keyboard: [JoypadButton; 2],
    microphone: bool
}

impl Input {
    pub fn new(subsystem: Option<GameControllerSubsystem>) -> Self {
        Input {
            subsystem,
            pads: Vec::new(),
            keyboard: [JoypadButton::empty(); 2],
            microphone: false
        }
    }

    pub fn handle_event(&mut self, event: &Event) {
        match *event {
            Event::KeyDown {
                keycode: Some(key), ..
            } => self.key(key, true),
            Event::KeyUp {
                keycode: Some(key), ..
            } => self.key(key, false),
            Event::ControllerDeviceAdded { which, .. } => self.add_pad(which),
            Event::ControllerDeviceRemoved { which, .. } => {
                if let Some(i) = self
                    .pads
                    .iter()
                    .position(|p| p.controller.instance_id() == which)
                {
                    let pad = self.pads.remove(i);
                    println!("gamepad disconnected: {}", pad.controller.name());
                }
            }
            Event::ControllerButtonDown { which, button, .. } => {
                self.pad_button(which, button, true)
            }
            Event::ControllerButtonUp { which, button, .. } => {
                self.pad_button(which, button, false)
            }
            Event::ControllerAxisMotion {
                which, axis, value, ..
            } => {
                if let Some(pad) = self.pad(which) {
                    let (negative, positive) = match axis {
                        Axis::LeftX => (JoypadButton::LEFT, JoypadButton::RIGHT),
                        Axis::LeftY => (JoypadButton::UP, JoypadButton::DOWN),
                        _ => return,
                    };
                    pad.stick.set(negative, value <= -STICK_THRESHOLD);
                    pad.stick.set(positive, value >= STICK_THRESHOLD);
                }
            }
            _ => {}
        }
    }

    pub fn apply(&self, joypads: &mut [Joypad; 2]) {
        for (player, joypad) in joypads.iter_mut().enumerate() {
            let mut buttons = self.keyboard[player];
            for pad in self.pads.iter().filter(|p| p.player == player) {
                buttons |= pad.buttons | pad.stick;
            }
            joypad.set_buttons(without_opposites(buttons));
        }
        joypads[1].microphone = self.microphone;
    }

    fn key(&mut self, key: Keycode, pressed: bool) {
        if key == MICROPHONE_KEY {
            self.microphone = pressed;
        }
        for &(k, player, button) in KEYS.iter() {
            if k == key {
                self.keyboard[player].set(button, pressed);
            }
        }
    }

    fn add_pad(&mut self, joystick_index: u32) {
        let Some(subsystem) = &self.subsystem else {
            return;
        };
        match subsystem.open(joystick_index) {
            Ok(controller) => {
                let player = if self.pads.iter().filter(|p| p.player == 0).count()
                    <= self.pads.iter().filter(|p| p.player == 1).count()
                {
                    0
                } else {
                    1
                };
                println!(
                    "gamepad connected: {} (player {})",
                    controller.name(),
                    player + 1
                );
                self.pads.push(Pad {
                    controller,
                    player,
                    buttons: JoypadButton::empty(),
                    stick: JoypadButton::empty(),
                });
            }
            Err(e) => eprintln!("warning: couldn't open gamepad {}: {}", joystick_index, e)
        }
    }

    fn pad(&mut self, instance_id: u32) -> Option<&mut Pad> {
        self.pads
            .iter_mut()
            .find(|p| p.controller.instance_id() == instance_id)
    }

    fn pad_button(&mut self, instance_id: u32, button: Button, pressed: bool) {
        if let (Some(nes_button), Some(pad)) = (pad_button(button), self.pad(instance_id)) {
            pad.buttons.set(nes_button, pressed);
        }
    }
}

fn without_opposites(mut buttons: JoypadButton) -> JoypadButton {
    for pair in [
        JoypadButton::LEFT | JoypadButton::RIGHT,
        JoypadButton::UP | JoypadButton::DOWN
    ] {
        if buttons.contains(pair) {
            buttons.remove(pair);
        }
    }
    buttons
}

#[cfg(test)]
mod test {
    use super::*;

    fn key_down(key: Keycode) -> Event {
        Event::KeyDown {
            timestamp: 0,
            window_id: 0,
            keycode: Some(key),
            scancode: None,
            keymod: sdl2::keyboard::Mod::NOMOD,
            repeat: false,
        }
    }

    fn key_up(key: Keycode) -> Event {
        Event::KeyUp {
            timestamp: 0,
            window_id: 0,
            keycode: Some(key),
            scancode: None,
            keymod: sdl2::keyboard::Mod::NOMOD,
            repeat: false,
        }
    }

    fn pressed(input: &Input) -> [JoypadButton; 2] {
        let mut joypads = [Joypad::new(), Joypad::new()];
        input.apply(&mut joypads);
        joypads.map(|mut j| {
            j.write(1);
            j.write(0);
            let mut bits = 0;
            for i in 0..8 {
                bits |= (j.read() & 1) << i;
            }
            JoypadButton::from_bits_truncate(bits)
        })
    }

    #[test]
    fn test_keyboard_drives_both_players() {
        let mut input = Input::new(None);
        input.handle_event(&key_down(Keycode::A));
        input.handle_event(&key_down(Keycode::L));
        input.handle_event(&key_down(Keycode::RShift));
        assert_eq!(
            pressed(&input),
            [
                JoypadButton::BUTTON_A,
                JoypadButton::RIGHT | JoypadButton::START
            ]
        );
        input.handle_event(&key_up(Keycode::L));
        assert_eq!(pressed(&input)[1], JoypadButton::START);
    }

    #[test]
    fn test_opposite_directions_cancel() {
        let mut input = Input::new(None);
        input.handle_event(&key_down(Keycode::Left));
        input.handle_event(&key_down(Keycode::Right));
        input.handle_event(&key_down(Keycode::Up));
        assert_eq!(pressed(&input)[0], JoypadButton::UP);
    }

    #[test]
    fn test_microphone_key() {
        let mut input = Input::new(None);
        let mut joypads = [Joypad::new(), Joypad::new()];
        input.handle_event(&key_down(Keycode::M));
        input.apply(&mut joypads);
        assert!(joypads[1].microphone);
        input.handle_event(&key_up(Keycode::M));
        input.apply(&mut joypads);
        assert!(!joypads[1].microphone);
    }

    #[test]
    fn test_pad_layout() {
        assert_eq!(pad_button(Button::B), Some(JoypadButton::BUTTON_A));
        assert_eq!(pad_button(Button::A), Some(JoypadButton::BUTTON_B));
        assert_eq!(pad_button(Button::Back), Some(JoypadButton::SELECT));
        assert_eq!(pad_button(Button::Guide), None);
    }
}