//! PulseAudio information queries for VoidMic.
//!
//! Provides utilities to query connected apps using VoidMic's virtual source.

use std::process::Command;

/// Information about an app connected to a PulseAudio source.
#[derive(Debug, Clone)]
pub struct ConnectedApp {
    pub name: String,
    pub source_output_id: u32,
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
    let mut current_id: Option<u32> = None;
    let mut current_name: Option<String> = None;
    let mut on_voidmic = false;
    
    for line in text.lines() {
        let line = line.trim();
        
        // New source output block
        if line.starts_with("Source Output #") {
            // Save previous if valid
            if let (Some(id), Some(name)) = (current_id.take(), current_name.take()) {
                if on_voidmic {
                    apps.push(ConnectedApp {
                        name,
                        source_output_id: id,
                    });
                }
            }
            
            // Parse new ID
            if let Some(id_str) = line.strip_prefix("Source Output #") {
                current_id = id_str.parse().ok();
            }
            current_name = None;
            on_voidmic = false;
        }
        // Check if connected to VoidMic source
        else if line.starts_with("Source:") {
            on_voidmic = line.contains("VoidMic_Clean");
        }
        // Get application name
        else if line.starts_with("application.name = ") {
            current_name = line
                .strip_prefix("application.name = ")
                .map(|s| s.trim_matches('"').to_string());
        }
    }
    
    // Handle last entry
    if let (Some(id), Some(name)) = (current_id, current_name) {
        if on_voidmic {
            apps.push(ConnectedApp {
                name,
                source_output_id: id,
            });
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
