//! PulseAudio information queries for VoidMic.
//!
//! Provides utilities to query connected apps using VoidMic's virtual source.

use std::process::Command;

/// Information about an app connected to a PulseAudio source.
#[derive(Debug, Clone)]
pub struct ConnectedApp {
    pub name: String,
}

/// Gets list of applications connected to VoidMic's virtual source.
pub fn get_connected_apps() -> Vec<ConnectedApp> {
    #[cfg(target_os = "linux")]
    {
        let output = Command::new("pactl")
            .args(["list", "source-outputs"])
            .output()
            .ok();

        let Some(output) = output else {
            return Vec::new();
        };

        if !output.status.success() {
            return Vec::new();
        }

        let text = String::from_utf8_lossy(&output.stdout);
        parse_source_outputs(&text)
    }

    #[cfg(not(target_os = "linux"))]
    {
        Vec::new()
    }
}

#[cfg(target_os = "linux")]
fn parse_source_outputs(text: &str) -> Vec<ConnectedApp> {
    let mut apps = Vec::new();
    let mut current_name: Option<String> = None;
    let mut on_voidmic = false;

    for line in text.lines() {
        let line = line.trim();

        if line.starts_with("Source Output #") {
            // Save previous if valid
            if on_voidmic {
                if let Some(name) = current_name.take() {
                    apps.push(ConnectedApp { name });
                }
            }
            current_name = None;
            on_voidmic = false;
        } else if line.starts_with("Source:") {
            on_voidmic = line.contains("VoidMic_Clean");
        } else if let Some(name) = line.strip_prefix("application.name = ") {
            current_name = Some(name.trim_matches('"').to_string());
        }
    }

    // Handle last entry
    if on_voidmic {
        if let Some(name) = current_name {
            apps.push(ConnectedApp { name });
        }
    }

    apps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_source_outputs() {
        let sample = r#"
Source Output #42
        Source: VoidMic_Clean.monitor
        application.name = "Discord"
        
Source Output #43
        Source: alsa_input.pci-0000
        application.name = "Firefox"
"#;
        let apps = parse_source_outputs(sample);
        assert_eq!(apps.len(), 1);
        assert_eq!(apps[0].name, "Discord");
    }
}
