//! Daemon management for VoidMic.
//!
//! Provides PID file management for graceful shutdown of background processes.

use directories::ProjectDirs;
use std::fs;
use std::path::PathBuf;

const PID_FILENAME: &str = "daemon.pid";

/// Gets the path to the PID file.
fn pid_file_path() -> Option<PathBuf> {
    ProjectDirs::from("com", "voidmic", "voidmic").map(|dirs| dirs.data_dir().join(PID_FILENAME))
}

/// Writes the given process ID to the PID file.
///
/// # Arguments
/// * `pid` - The process ID to write (typically the child/daemon process ID)
pub fn write_pid_file(pid: u32) -> Result<(), String> {
    let path = pid_file_path().ok_or("Could not determine data directory")?;

    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("Failed to create data directory: {}", e))?;
    }

    fs::write(&path, pid.to_string()).map_err(|e| format!("Failed to write PID file: {}", e))?;

    Ok(())
}

/// Reads the daemon PID from the PID file.
pub fn read_pid_file() -> Option<u32> {
    let path = pid_file_path()?;
    let content = fs::read_to_string(&path).ok()?;
    content.trim().parse().ok()
}

/// Removes the PID file.
pub fn remove_pid_file() -> Result<(), String> {
    if let Some(path) = pid_file_path() {
        if path.exists() {
            fs::remove_file(&path).map_err(|e| format!("Failed to remove PID file: {}", e))?;
        }
    }
    Ok(())
}


/// Stops the running daemon by sending SIGTERM.
#[cfg(target_os = "linux")]
pub fn stop_daemon() -> Result<(), String> {
    if let Some(pid) = read_pid_file() {
        use std::process::Command;

        let result = Command::new("kill")
            .arg("-TERM")
            .arg(pid.to_string())
            .output()
            .map_err(|e| format!("Failed to send signal: {}", e))?;

        if result.status.success() {
            // Wait briefly for process to exit
            std::thread::sleep(std::time::Duration::from_millis(500));
            remove_pid_file()?;
            Ok(())
        } else {
            // Process may have already exited
            remove_pid_file()?;
            Ok(())
        }
    } else {
        Err("No daemon PID file found".to_string())
    }
}

#[cfg(not(target_os = "linux"))]
pub fn stop_daemon() -> Result<(), String> {
    Err("Daemon management not supported on this platform".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_pid_file_path_exists() {
        // Should return Some path on most systems
        assert!(pid_file_path().is_some());
    }
}
