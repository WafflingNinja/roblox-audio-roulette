// Saved-sounds list, persisted as JSON in the config dir.

use serde::{Deserialize, Serialize};
use std::fs;
use std::path::PathBuf;

#[derive(Clone, Serialize, Deserialize)]
pub struct SavedSound {
    pub id: u64,
    pub name: String,
    pub creator: String,
    pub duration: u32,
    pub kind: String,
    #[serde(default)]
    pub deleted: bool,
}

pub fn config_dir() -> PathBuf {
    let dir = dirs::config_dir()
        .unwrap_or_else(|| PathBuf::from("."))
        .join("roblox-audio-roulette");
    let _ = fs::create_dir_all(&dir);
    dir
}

fn saved_path() -> PathBuf {
    config_dir().join("saved.json")
}

pub fn load() -> Vec<SavedSound> {
    match fs::read_to_string(saved_path()) {
        Ok(s) => serde_json::from_str(&s).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

pub fn save(list: &[SavedSound]) {
    if let Ok(s) = serde_json::to_string_pretty(list) {
        let _ = fs::write(saved_path(), s);
    }
}
