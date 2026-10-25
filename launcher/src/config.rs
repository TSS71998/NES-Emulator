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
    pub descending: bool
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
pub enum SortKey {
    #[default]
    Title,
    Region,
    Mapper,
    Size,
    LastPlayed,
    PlayTime
}