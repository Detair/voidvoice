//! Platform-specific autostart functionality for VoidMic.
//!
//! Supports:
//! - Linux: XDG autostart (.desktop file in ~/.config/autostart/)
//! - Windows: Registry key (HKCU\Software\Microsoft\Windows\CurrentVersion\Run)
//! - macOS: LaunchAgent plist

use std::fs;
use std::path::PathBuf;

/// Returns the path to the current executable, if available.
fn get_exe_path() -> Option<PathBuf> {
    std::env::current_exe().ok()
}

/// Enables autostart for VoidMic.
pub fn enable_autostart() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let autostart_dir = dirs_autostart_dir()?;
        fs::create_dir_all(&autostart_dir)
            .map_err(|e| format!("Failed to create autostart dir: {}", e))?;

        let desktop_path = autostart_dir.join("voidmic.desktop");
        let exe_path = get_exe_path().ok_or("Could not determine executable path")?;

        let desktop_content = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=VoidMic\n\
             Comment=Hybrid AI Noise Reduction\n\
             Exec={}\n\
             Terminal=false\n\
             StartupNotify=false\n\
             Categories=AudioVideo;Audio;\n",
            exe_path.display()
        );

        fs::write(&desktop_path, desktop_content)
            .map_err(|e| format!("Failed to write desktop file: {}", e))?;

        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;
        let exe_path = get_exe_path().ok_or("Could not determine executable path")?;

        let result = Command::new("reg")
            .args([
                "add",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "VoidMic",
                "/t",
                "REG_SZ",
                "/d",
                &exe_path.to_string_lossy(),
                "/f",
            ])
            .output()
            .map_err(|e| format!("Failed to run reg command: {}", e))?;

        if result.status.success() {
            Ok(())
        } else {
            Err(format!(
                "Registry command failed: {}",
                String::from_utf8_lossy(&result.stderr)
            ))
        }
    }

    #[cfg(target_os = "macos")]
    {
        let launch_agents_dir = dirs::home_dir()
            .ok_or("Could not find home directory")?
            .join("Library/LaunchAgents");

        fs::create_dir_all(&launch_agents_dir)
            .map_err(|e| format!("Failed to create LaunchAgents dir: {}", e))?;

        let plist_path = launch_agents_dir.join("com.voidmic.plist");
        let exe_path = get_exe_path().ok_or("Could not determine executable path")?;

        let plist_content = format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>com.voidmic</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
</dict>
</plist>
"#,
            exe_path.display()
        );

        fs::write(&plist_path, plist_content)
            .map_err(|e| format!("Failed to write plist: {}", e))?;

        Ok(())
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Err("Autostart not supported on this platform".to_string())
    }
}

/// Disables autostart for VoidMic.
pub fn disable_autostart() -> Result<(), String> {
    #[cfg(target_os = "linux")]
    {
        let autostart_dir = dirs_autostart_dir()?;
        let desktop_path = autostart_dir.join("voidmic.desktop");

        if desktop_path.exists() {
            fs::remove_file(&desktop_path)
                .map_err(|e| format!("Failed to remove desktop file: {}", e))?;
        }
        Ok(())
    }

    #[cfg(target_os = "windows")]
    {
        use std::process::Command;

        let result = Command::new("reg")
            .args([
                "delete",
                r"HKCU\Software\Microsoft\Windows\CurrentVersion\Run",
                "/v",
                "VoidMic",
                "/f",
            ])
            .output()
            .map_err(|e| format!("Failed to run reg command: {}", e))?;

        // Ignore error if key doesn't exist
        let _ = result;
        Ok(())
    }

    #[cfg(target_os = "macos")]
    {
        let plist_path = dirs::home_dir()
            .ok_or("Could not find home directory")?
            .join("Library/LaunchAgents/com.voidmic.plist");

        if plist_path.exists() {
            fs::remove_file(&plist_path).map_err(|e| format!("Failed to remove plist: {}", e))?;
        }
        Ok(())
    }

    #[cfg(not(any(target_os = "linux", target_os = "windows", target_os = "macos")))]
    {
        Err("Autostart not supported on this platform".to_string())
    }
}



#[cfg(target_os = "linux")]
fn dirs_autostart_dir() -> Result<PathBuf, String> {
    dirs::config_dir()
        .map(|c| c.join("autostart"))
        .ok_or_else(|| "Could not find config directory".to_string())
}
