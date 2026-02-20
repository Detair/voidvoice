//! Auto-updater functionality for VoidMic.
//!
//! Checks GitHub Releases API for newer versions and provides download links.

use semver::Version;
use serde::Deserialize;

const CURRENT_VERSION: &str = env!("CARGO_PKG_VERSION");
const GITHUB_API_URL: &str = "https://api.github.com/repos/Detair/voidvoice/releases/latest";

/// Information about an available update.
#[derive(Clone, Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub download_url: String,
}

#[derive(Deserialize)]
struct GitHubRelease {
    tag_name: String,
    html_url: String,
}

/// Checks GitHub for available updates.
///
/// Returns `Some(UpdateInfo)` if a newer version is available, `None` otherwise.
/// Returns `Err` on network or parsing errors.
pub fn check_for_updates() -> Result<Option<UpdateInfo>, String> {
    let client = reqwest::blocking::Client::builder()
        .user_agent("VoidMic-Updater")
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|e| format!("Failed to create HTTP client: {}", e))?;

    let response = client
        .get(GITHUB_API_URL)
        .send()
        .map_err(|e| format!("Failed to fetch release info: {}", e))?;

    if !response.status().is_success() {
        return Err(format!("GitHub API returned status: {}", response.status()));
    }

    let release: GitHubRelease = response
        .json()
        .map_err(|e| format!("Failed to parse release JSON: {}", e))?;

    // Parse versions (strip 'v' prefix if present)
    let remote_version_str = release.tag_name.trim_start_matches('v');
    let current_version = Version::parse(CURRENT_VERSION)
        .map_err(|e| format!("Failed to parse current version: {}", e))?;
    let remote_version = Version::parse(remote_version_str).map_err(|e| {
        format!(
            "Failed to parse remote version '{}': {}",
            remote_version_str, e
        )
    })?;

    if remote_version > current_version {
        Ok(Some(UpdateInfo {
            version: release.tag_name,
            download_url: release.html_url,
        }))
    } else {
        Ok(None)
    }
}

/// Spawns a background thread to check for updates.
///
/// Returns a receiver that will contain the update info when available.
pub fn check_for_updates_async() -> std::sync::mpsc::Receiver<Option<UpdateInfo>> {
    let (tx, rx) = std::sync::mpsc::channel();

    std::thread::spawn(move || {
        let result = check_for_updates().ok().flatten();
        let _ = tx.send(result);
    });

    rx
}

#[cfg(test)]
mod tests {
    use super::*;
    use semver::Version;

    #[test]
    fn test_version_parsing_with_v_prefix() {
        let tag = "v1.2.3";
        let version_str = tag.trim_start_matches('v');
        let version = Version::parse(version_str).unwrap();
        assert_eq!(version.major, 1);
        assert_eq!(version.minor, 2);
        assert_eq!(version.patch, 3);
    }

    #[test]
    fn test_version_parsing_without_prefix() {
        let tag = "0.7.0";
        let version = Version::parse(tag).unwrap();
        assert_eq!(version.major, 0);
        assert_eq!(version.minor, 7);
        assert_eq!(version.patch, 0);
    }

    #[test]
    fn test_version_comparison() {
        let current = Version::parse("0.7.0").unwrap();
        let newer = Version::parse("0.8.0").unwrap();
        let older = Version::parse("0.6.0").unwrap();

        assert!(newer > current);
        assert!(older < current);
        assert!(current == Version::parse("0.7.0").unwrap());
    }

    #[test]
    fn test_github_release_deserialization() {
        let json =
            r#"{"tag_name":"v1.0.0","html_url":"https://example.com","body":"Release notes"}"#;
        let release: GitHubRelease = serde_json::from_str(json).unwrap();
        assert_eq!(release.tag_name, "v1.0.0");
        assert_eq!(release.html_url, "https://example.com");
    }
}
