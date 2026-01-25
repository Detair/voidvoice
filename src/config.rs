use serde::{Deserialize, Serialize};
use directories::ProjectDirs;
use std::fs;
use std::path::PathBuf;

/// Application configuration for persisting user preferences.
#[derive(Serialize, Deserialize, Default, Clone)]
pub struct AppConfig {
    pub last_input: String,
    pub last_output: String,
}

impl AppConfig {
    /// Loads configuration from disk, or returns default if not found.
    pub fn load() -> Self {
        if let Some(path) = config_path() {
            if let Ok(content) = fs::read_to_string(path) {
                if let Ok(cfg) = serde_json::from_str(&content) {
                    return cfg;
                }
            }
        }
        Self::default()
    }

    /// Saves configuration to disk in JSON format.
    pub fn save(&self) {
        if let Some(path) = config_path() {
            if let Some(parent) = path.parent() {
                let _ = fs::create_dir_all(parent);
            }
            if let Ok(json) = serde_json::to_string_pretty(self) {
                let _ = fs::write(path, json);
            }
        }
    }
}

fn config_path() -> Option<PathBuf> {
    ProjectDirs::from("com", "voidmic", "voidmic")
        .map(|dirs| dirs.config_dir().join("config.json"))
}
