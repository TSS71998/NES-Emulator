//! Key and gamepad bindings: which keys and gamepad buttons press each
//! player's NES buttons. They're kept with the launcher's settings, as
//! names: egui's key names ("A", "Up", "Enter"), "Shift", "Ctrl" or "Alt"
//! for the modifier keys, and SDL's gamepad button names ("a", "dpup",
//! "start").

use eframe::egui::{Key, Modifiers};
use nes_emulator::input::PadLayout;
use nes_emulator::joypad::JoypadButton;
use sdl2::controller::Button;
use serde::{Deserialize, Serialize};

/// The NES controller's buttons, in the order the settings show them.
pub const BUTTONS: [(JoypadButton, &str); 8] = [
    (JoypadButton::UP, "Up"),
    (JoypadButton::DOWN, "Down"),
    (JoypadButton::LEFT, "Left"),
    (JoypadButton::RIGHT, "Right"),
    (JoypadButton::BUTTON_A, "A"),
    (JoypadButton::BUTTON_B, "B"),
    (JoypadButton::START, "Start"),
    (JoypadButton::SELECT, "Select"),
];

/// Keys the launcher keeps for itself while a game runs (menu, fast
/// forward, save states, fullscreen).
pub const RESERVED_KEYS: [Key; 5] = [Key::Escape, Key::Tab, Key::F5, Key::F8, Key::F11];

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct PlayerControls {
    /// The keys for each of `BUTTONS`.
    pub keys: [Vec<String>; 8],
    /// The gamepad buttons for each of `BUTTONS`.
    pub pad: [Vec<String>; 8],
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct Controls {
    pub players: [PlayerControls; 2],
    /// The Famicom's microphone, on player 2's controller.
    pub microphone: Vec<String>,
}

fn names<const N: usize>(lists: [&[&str]; N]) -> [Vec<String>; N] {
    lists.map(|list| list.iter().map(|name| name.to_string()).collect())
}

fn default_pad() -> [Vec<String>; 8] {
    names([
        &["dpup"],
        &["dpdown"],
        &["dpleft"],
        &["dpright"],
        &["b", "y"],
        &["a", "x"],
        &["start"],
        &["back"],
    ])
}

impl PlayerControls {
    /// The standalone emulator's keys for player 1 or 2.
    pub fn default_for(player: usize) -> PlayerControls {
        let keys = if player == 0 {
            names([&["Up"], &["Down"], &["Left"], &["Right"], &["A"], &["S"], &["Enter"], &["Space"]])
        } else {
            names([&["I"], &["K"], &["J"], &["L"], &["G"], &["F"], &["Shift"], &["Ctrl"]])
        };
        PlayerControls {
            keys,
            pad: default_pad(),
        }
    }
}

impl Default for PlayerControls {
    fn default() -> Self {
        PlayerControls::default_for(0)
    }
}

impl Default for Controls {
    fn default() -> Self {
        Controls {
            players: [PlayerControls::default_for(0), PlayerControls::default_for(1)],
            microphone: vec!["M".to_string()],
        }
    }
}

impl Controls {
    /// The NES buttons the held keys press, for each player, and the
    /// microphone. `held` has the names of the keys held down, modifiers
    /// included (see `held_names`).
    pub fn keyboard_state(&self, held: &[String]) -> ([JoypadButton; 2], bool) {
        let is_held = |bound: &Vec<String>| bound.iter().any(|name| held.contains(name));
        let buttons = std::array::from_fn(|player| {
            let mut buttons = JoypadButton::empty();
            for (keys, (button, _)) in self.players[player].keys.iter().zip(BUTTONS) {
                if is_held(keys) {
                    buttons |= button;
                }
            }
            buttons
        });
        (buttons, is_held(&self.microphone))
    }

    /// Each player's gamepad layout, for the emulator.
    pub fn pad_layouts(&self) -> [PadLayout; 2] {
        std::array::from_fn(|player| {
            let mut layout = PadLayout::new();
            for (pad, (button, _)) in self.players[player].pad.iter().zip(BUTTONS) {
                for name in pad {
                    if let Some(pad_button) = Button::from_string(name) {
                        layout.push((pad_button, button));
                    }
                }
            }
            layout
        })
    }
}

/// The names of the held keys and modifiers, for `keyboard_state`.
pub fn held_names(keys: &[Key], modifiers: Modifiers) -> Vec<String> {
    let mut names: Vec<String> = keys.iter().map(|key| key.name().to_string()).collect();
    for (held, name) in [(modifiers.shift, "Shift"), (modifiers.ctrl, "Ctrl"), (modifiers.alt, "Alt")] {
        if held {
            names.push(name.to_string());
        }
    }
    names
}

/// How a key is shown: its symbol for the arrows, otherwise its name.
pub fn key_label(name: &str) -> String {
    match Key::from_name(name) {
        Some(key) => key.symbol_or_name().to_string(),
        None => name.to_string(),
    }
}

/// How a gamepad button is shown (named as on an Xbox pad, which is how SDL
/// names them).
pub fn pad_label(name: &str) -> String {
    match name {
        "a" => "A",
        "b" => "B",
        "x" => "X",
        "y" => "Y",
        "back" => "Back",
        "guide" => "Guide",
        "start" => "Start",
        "leftstick" => "Left stick",
        "rightstick" => "Right stick",
        "leftshoulder" => "LB",
        "rightshoulder" => "RB",
        "dpup" => "D-pad ⏶",
        "dpdown" => "D-pad ⏷",
        "dpleft" => "D-pad ⏴",
        "dpright" => "D-pad ⏵",
        "misc1" => "Share",
        "touchpad" => "Touchpad",
        other => return other.to_string(),
    }
    .to_string()
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_default_keys_match_the_standalone_emulator() {
        let controls = Controls::default();
        let held = held_names(&[Key::A, Key::L], Modifiers::SHIFT);
        let (buttons, microphone) = controls.keyboard_state(&held);
        assert_eq!(buttons, [JoypadButton::BUTTON_A, JoypadButton::RIGHT | JoypadButton::START]);
        assert!(!microphone);
        let (_, microphone) = controls.keyboard_state(&held_names(&[Key::M], Modifiers::NONE));
        assert!(microphone);
    }

    #[test]
    fn test_rebinding_and_pad_layouts() {
        let mut controls = Controls::default();
        controls.players[0].keys[4] = vec!["Z".to_string()];
        controls.players[1].pad[6] = vec!["guide".to_string(), "nonsense".to_string()];
        let (buttons, _) = controls.keyboard_state(&held_names(&[Key::Z, Key::A], Modifiers::NONE));
        assert_eq!(buttons[0], JoypadButton::BUTTON_A);
        let layouts = controls.pad_layouts();
        assert!(layouts[0].contains(&(Button::Y, JoypadButton::BUTTON_A)));
        assert!(layouts[1].contains(&(Button::Guide, JoypadButton::START)));
        assert!(!layouts[1].contains(&(Button::Start, JoypadButton::START)));
    }

    #[test]
    fn test_labels() {
        assert_eq!(key_label("Up"), "⏶");
        assert_eq!(key_label("Shift"), "Shift");
        assert_eq!(pad_label("leftshoulder"), "LB");
        assert_eq!(Button::from_string("dpup"), Some(Button::DPadUp));
    }
}