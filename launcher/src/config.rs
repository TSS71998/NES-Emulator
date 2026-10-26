//! What the launcher remembers between runs. eframe saves it (as RON) in the
//! platform's app data folder, along with the window size and position.

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
    pub get_covers: bool
}

#[derive(Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct GameRecord {
    pub region: Option<Region>,
    pub console: Option<Console>,
    pub no_oam_corruption: bool,
    pub last_played: Option<u64>,
    pub play_secs: u64
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