//! What the launcher remembers between runs. eframe saves it (as RON) in the
//! platform's app data folder, along with the window size and position.

use crate::controls::Controls;
use crate::library::{Console, Region};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Config {
    pub folders: Vec<PathBuf>,
    pub files: Vec<PathBuf>,
    pub games: BTreeMap<String, GameRecord>,
    pub sort: SortKey,
    pub descending: bool,
    pub screen_size: ScreenSize,
    pub tv_aspect: bool,
    pub pause_on_focus_loss: bool,
    pub fast_forward: FastForward,
    pub view: LibraryView,
    pub controls: Controls,
    pub get_covers: bool,
    pub volume: Volume,
    pub muted: bool,
    pub favorites_only: bool
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameRecord {
    pub region: Option<Region>,
    pub console: Option<Console>,
    pub no_oam_corruption: bool,
    pub last_played: Option<u64>,
    pub play_secs: u64,
    pub favorite: bool
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Volume(pub u8);

impl Default for Volume {
    fn default() -> Self {
        Volume(100)
    }
}

impl Volume {
    pub fn gain(self, muted: bool) -> f32 {
        if muted {
            return 0.0
        }
        let fraction = self.0.min(100) as f32 / 100.0;
        fraction * fraction
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScreenSize {
    #[default]
    WholePixels,
    FillWindow
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum LibraryView {
    #[default]
    List,
    Covers
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum FastForward {
    Double,
    Triple,
    #[default]
    Four,
    Eight,
    Unlimited
}

impl FastForward {
    pub const ALL: [FastForward; 5] = [
        FastForward::Double,
        FastForward::Triple,
        FastForward::Four,
        FastForward::Eight,
        FastForward::Unlimited,
    ];

    pub fn frames(self) -> u32 {
        match self {
            FastForward::Double => 2,
            FastForward::Triple => 3,
            FastForward::Four => 4,
            FastForward::Eight => 8,
            FastForward::Unlimited => 0
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            FastForward::Double => "2×",
            FastForward::Triple => "3×",
            FastForward::Four => "4×",
            FastForward::Eight => "8×",
            FastForward::Unlimited => "As fast as possible"
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum SortKey {
    #[default]
    Title,
    Region,
    Mapper,
    Size,
    LastPlayed,
    PlayTime
}

#[cfg(test)]
mod test {
    use super::*;

    #[test]
    fn test_volume_gain() {
        assert_eq!(Volume::default().gain(false), 1.0);
        assert_eq!(Volume(50).gain(false), 0.25);
        assert_eq!(Volume(100).gain(true), 0.0);
        assert_eq!(Volume(0).gain(false), 0.0);
    }

    #[test]
    fn test_settings_start_at_full_volume() {
        let config = Config::default();
        assert_eq!(config.volume, Volume(100));
        assert!(!config.muted && !config.favorites_only);
    }
}