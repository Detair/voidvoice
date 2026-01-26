use serde::{Deserialize, Serialize};
use directories::ProjectDirs;
use std::fs;
use std::path::PathBuf;

/// Application configuration for persisting user preferences.
#[derive(Serialize, Deserialize, Clone)]
pub struct AppConfig {
    pub last_input: String,
    pub last_output: String,
    #[serde(default = "default_gate_threshold")]
    pub gate_threshold: f32,
    #[serde(default = "default_suppression_strength")]
    pub suppression_strength: f32,
    #[serde(default)]
    pub start_on_boot: bool,
    #[serde(default)]
    pub output_filter_enabled: bool,
    #[serde(default)]
    pub echo_cancel_enabled: bool,
    #[serde(default)]
    pub dynamic_threshold_enabled: bool,
}

fn default_gate_threshold() -> f32 {
    0.015
}

fn default_suppression_strength() -> f32 {
    1.0
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            last_input: String::new(),
            last_output: String::new(),
            gate_threshold: default_gate_threshold(),
            suppression_strength: default_suppression_strength(),
            start_on_boot: false,
            output_filter_enabled: false,
            echo_cancel_enabled: false,
            dynamic_threshold_enabled: false,
        }
    }
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
