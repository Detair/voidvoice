//! Virtual audio device management for VoidMic.
//!
//! Handles automatic creation and cleanup of virtual sinks/sources
//! for PulseAudio and PipeWire on Linux.

use std::process::Command;

/// Name of the virtual sink created by VoidMic
pub const VIRTUAL_SINK_NAME: &str = "VoidMic_Clean";

/// Information about a created virtual device
#[derive(Debug, Clone)]
pub struct VirtualDevice {
    pub module_id: u32,
    pub sink_name: String,
}

/// Creates a virtual null-sink for VoidMic output.
///
/// On Linux, uses `pactl` to load module-null-sink.
/// Returns the module ID for later unloading.
pub fn create_virtual_sink() -> Result<VirtualDevice, String> {
    #[cfg(target_os = "linux")]
    {
        // Check if sink already exists
        let check = Command::new("pactl")
            .args(["list", "short", "sinks"])
            .output()
            .map_err(|e| format!("Failed to list sinks: {}", e))?;

        let output = String::from_utf8_lossy(&check.stdout);
        if output.contains(VIRTUAL_SINK_NAME) {
            // Already exists, try to find module ID
            return Ok(VirtualDevice {
                module_id: 0, // Unknown, but exists
                sink_name: VIRTUAL_SINK_NAME.to_string(),
            });
        }

        // Create new null-sink
        let result = Command::new("pactl")
            .args([
                "load-module",
                "module-null-sink",
                &format!("sink_name={}", VIRTUAL_SINK_NAME),
                &format!("sink_properties=device.description={}", VIRTUAL_SINK_NAME),
            ])
            .output()
            .map_err(|e| format!("Failed to create sink: {}", e))?;

        if result.status.success() {
            let module_id: u32 = String::from_utf8_lossy(&result.stdout)
                .trim()
                .parse()
                .unwrap_or(0);

            Ok(VirtualDevice {
                module_id,
                sink_name: VIRTUAL_SINK_NAME.to_string(),
            })
        } else {
            let stderr = String::from_utf8_lossy(&result.stderr);
            Err(format!("pactl failed: {}", stderr))
        }
    }

    #[cfg(target_os = "windows")]
    {
        // On Windows, we can't auto-create. Return instruction to install VB-Cable.
        Err("Windows requires VB-Cable. Install from: https://vb-audio.com/Cable/".to_string())
    }

    #[cfg(target_os = "macos")]
    {
        // On macOS, we can't auto-create. Return instruction to install BlackHole.
        Err("macOS requires BlackHole. Install via: brew install blackhole-2ch".to_string())
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Err("Virtual device creation not supported on this platform".to_string())
    }
}

/// Destroys a virtual sink by module ID.
///
/// If `module_id` is 0 (unknown), looks up the specific module ID for VoidMic_Clean
/// rather than unloading all null-sink modules on the system.
pub fn destroy_virtual_sink(module_id: u32) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let effective_id = if module_id == 0 {
            // Find VoidMic_Clean's specific module ID instead of unloading all null-sinks
            find_voidmic_module_id().unwrap_or(0)
        } else {
            module_id
        };

        if effective_id == 0 {
            return Err("Could not find VoidMic_Clean module to unload".to_string());
        }

        let result = Command::new("pactl")
            .args(["unload-module", &effective_id.to_string()])
            .output()
            .map_err(|e| format!("Failed to unload module: {}", e))?;

        if result.status.success() {
            Ok(())
        } else {
            // Ignore errors on cleanup (module may have already been unloaded)
            Ok(())
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = module_id;
        Ok(()) // No-op on other platforms
    }
}

/// Finds the PulseAudio module ID for the VoidMic_Clean null-sink.
#[cfg(target_os = "linux")]
fn find_voidmic_module_id() -> Option<u32> {
    let output = Command::new("pactl")
        .args(["list", "short", "modules"])
        .output()
        .ok()?;

    if !output.status.success() {
        return None;
    }

    let text = String::from_utf8_lossy(&output.stdout);
    for line in text.lines() {
        // Format: "ID\tmodule-null-sink\tsink_name=VoidMic_Clean ..."
        if line.contains("module-null-sink") && line.contains(VIRTUAL_SINK_NAME) {
            return line.split_whitespace().next()?.parse().ok();
        }
    }
    None
}

/// Checks if virtual sink exists.
pub fn virtual_sink_exists() -> bool {
    #[cfg(target_os = "linux")]
    {
        Command::new("pactl")
            .args(["list", "short", "sinks"])
            .output()
            .map(|o| String::from_utf8_lossy(&o.stdout).contains(VIRTUAL_SINK_NAME))
            .unwrap_or(false)
    }

    #[cfg(not(target_os = "linux"))]
    {
        false
    }
}

/// Gets the monitor source name for the virtual sink.
/// This is what apps should select as their microphone input.
pub fn get_monitor_source_name() -> String {
    format!("{}.monitor", VIRTUAL_SINK_NAME)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_virtual_sink_name_constant() {
        assert_eq!(VIRTUAL_SINK_NAME, "VoidMic_Clean");
        assert!(!VIRTUAL_SINK_NAME.is_empty());
        assert!(!VIRTUAL_SINK_NAME.contains(' ')); // No spaces for pactl compatibility
    }

    #[test]
    fn test_monitor_source_name_format() {
        let monitor = get_monitor_source_name();
        assert_eq!(monitor, "VoidMic_Clean.monitor");
        assert!(monitor.ends_with(".monitor"));
        assert!(monitor.starts_with(VIRTUAL_SINK_NAME));
    }

    #[test]
    fn test_virtual_device_struct_construction() {
        let device = VirtualDevice {
            module_id: 42,
            sink_name: "test_sink".to_string(),
        };
        assert_eq!(device.module_id, 42);
        assert_eq!(device.sink_name, "test_sink");
    }

    #[test]
    fn test_virtual_device_clone() {
        let device = VirtualDevice {
            module_id: 123,
            sink_name: VIRTUAL_SINK_NAME.to_string(),
        };
        let cloned = device.clone();
        assert_eq!(cloned.module_id, device.module_id);
        assert_eq!(cloned.sink_name, device.sink_name);
    }

    #[test]
    fn test_virtual_device_debug() {
        let device = VirtualDevice {
            module_id: 1,
            sink_name: "test".to_string(),
        };
        let debug_str = format!("{:?}", device);
        assert!(debug_str.contains("VirtualDevice"));
        assert!(debug_str.contains("module_id"));
        assert!(debug_str.contains("sink_name"));
    }
}
