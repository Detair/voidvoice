use directories::ProjectDirs;
use serde::{Deserialize, Serialize};
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
    #[serde(default)]
    pub start_minimized: bool,
    #[serde(default)]
    pub auto_start_processing: bool,
    #[serde(default)]
    pub window_x: Option<f32>,
    #[serde(default)]
    pub window_y: Option<f32>,
    #[serde(default = "default_dark_mode")]
    pub dark_mode: bool,
    #[serde(default = "default_preset")]
    pub preset: String,
    #[serde(default = "default_toggle_hotkey")]
    pub toggle_hotkey: String,
    #[serde(default = "default_first_run")]
    pub first_run: bool,
    #[serde(default = "default_vad_sensitivity")]
    pub vad_sensitivity: i32,
    #[serde(default)]
    pub eq_enabled: bool,
    #[serde(default)]
    pub eq_low_gain: f32, // dB
    #[serde(default)]
    pub eq_mid_gain: f32, // dB
    #[serde(default)]
    pub eq_high_gain: f32, // dB
    
    // Phase 4 field
    #[serde(default)]
    pub agc_enabled: bool,
    #[serde(default = "default_agc_target")]
    pub agc_target_level: f32,

    // Phase 6
    #[serde(default)]
    pub mini_mode: bool,
}

fn default_agc_target() -> f32 {
    0.7 // Approx -3dB
}

fn default_vad_sensitivity() -> i32 {
    2 // 0-3, 3 is most aggressive
}

fn default_first_run() -> bool {
    true
}

fn default_toggle_hotkey() -> String {
    "Control+Shift+M".to_string()
}

fn default_preset() -> String {
    "Standard".to_string()
}

fn default_gate_threshold() -> f32 {
    0.015
}

fn default_suppression_strength() -> f32 {
    1.0
}

fn default_dark_mode() -> bool {
    true
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
            start_minimized: false,
            auto_start_processing: false,
            window_x: None,
            window_y: None,
            dark_mode: true,
            preset: default_preset(),
            toggle_hotkey: default_toggle_hotkey(),
            first_run: true,
            vad_sensitivity: default_vad_sensitivity(),
            eq_enabled: false,
            eq_low_gain: 0.0,
            eq_mid_gain: 0.0,
            eq_high_gain: 0.0,
            agc_enabled: false,
            agc_target_level: default_agc_target(),
            mini_mode: false,
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
    ProjectDirs::from("com", "voidmic", "voidmic").map(|dirs| dirs.config_dir().join("config.json"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_default_config_values() {
        let config = AppConfig::default();
        assert_eq!(config.gate_threshold, 0.015);
        assert_eq!(config.suppression_strength, 1.0);
        assert!(!config.start_on_boot);
        assert!(!config.echo_cancel_enabled);
        assert!(!config.dynamic_threshold_enabled);
    }

    #[test]
    fn test_config_serialization() {
        let config = AppConfig {
            last_input: "Test Mic".to_string(),
            last_output: "Test Output".to_string(),
            gate_threshold: 0.02,
            suppression_strength: 0.8,
            start_on_boot: true,
            output_filter_enabled: false,
            echo_cancel_enabled: true,
            dynamic_threshold_enabled: true,
            start_minimized: false,
            auto_start_processing: false,
            window_x: None,
            window_y: None,
            dark_mode: true,
            preset: "Gaming".to_string(),
            toggle_hotkey: "Control+Shift+M".to_string(),
            first_run: true,
            vad_sensitivity: 2,
            eq_enabled: true,
            eq_low_gain: 3.0,
            eq_mid_gain: -1.0,
            eq_high_gain: 0.0,
        };

        let json = serde_json::to_string(&config).unwrap();
        assert!(json.contains("\"gate_threshold\":0.02"));
        assert!(json.contains("\"echo_cancel_enabled\":true"));
        assert!(json.contains("\"vad_sensitivity\":2"));
        assert!(json.contains("\"eq_enabled\":true"));
    }

    #[test]
    fn test_config_deserialization_with_defaults() {
        // Minimal JSON - should fill in defaults
        let json = r#"{"last_input":"Mic","last_output":"Out"}"#;
        let config: AppConfig = serde_json::from_str(json).unwrap();

        assert_eq!(config.last_input, "Mic");
        assert_eq!(config.gate_threshold, 0.015); // Default
        assert_eq!(config.suppression_strength, 1.0); // Default
        assert!(!config.echo_cancel_enabled); // Default false
    }

    #[test]
    fn test_config_roundtrip() {
        let original = AppConfig {
            last_input: "Input".to_string(),
            last_output: "Output".to_string(),
            gate_threshold: 0.025,
            suppression_strength: 0.5,
            start_on_boot: false,
            output_filter_enabled: true,
            echo_cancel_enabled: false,
            dynamic_threshold_enabled: true,
            start_minimized: true,
            auto_start_processing: true,
            window_x: Some(100.0),
            window_y: Some(200.0),
            dark_mode: false,
            preset: "Podcast".to_string(),
            toggle_hotkey: "Control+Shift+K".to_string(),
            first_run: false,
            vad_sensitivity: 3,
            eq_enabled: false,
            eq_low_gain: 0.0,
            eq_mid_gain: 0.0,
            eq_high_gain: 0.0,
        };

        let json = serde_json::to_string(&original).unwrap();
        let restored: AppConfig = serde_json::from_str(&json).unwrap();

        assert_eq!(original.gate_threshold, restored.gate_threshold);
        assert_eq!(
            original.dynamic_threshold_enabled,
            restored.dynamic_threshold_enabled
        );
        assert_eq!(
            original.output_filter_enabled,
            restored.output_filter_enabled
        );
    }
}
