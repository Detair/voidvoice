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
                "load-module", "module-null-sink",
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
        Err("Windows requires VB-Cable. Please install from vb-audio.com/Cable/".to_string())
    }

    #[cfg(target_os = "macos")]
    {
        // On macOS, we can't auto-create. Return instruction to install BlackHole.
        Err("macOS requires BlackHole. Please install from github.com/ExistentialAudio/BlackHole".to_string())
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Err("Virtual device creation not supported on this platform".to_string())
    }
}

/// Destroys a virtual sink by module ID.
pub fn destroy_virtual_sink(module_id: u32) -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        if module_id == 0 {
            // Unknown module ID, try to unload by sink name
            let _ = Command::new("pactl")
                .args(["unload-module", "module-null-sink"])
                .output();
            return Ok(());
        }
        
        let result = Command::new("pactl")
            .args(["unload-module", &module_id.to_string()])
            .output()
            .map_err(|e| format!("Failed to unload module: {}", e))?;

        if result.status.success() {
            Ok(())
        } else {
            // Ignore errors on cleanup
            Ok(())
        }
    }

    #[cfg(not(target_os = "linux"))]
    {
        let _ = module_id;
        Ok(()) // No-op on other platforms
    }
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
