use serde_json::Value;
use std::path::Path;
use std::time::Duration;

#[derive(Debug)]
pub struct UpdateInfo {
    pub version: String,
    pub setup_url: String,
}

fn is_newer_version(current: &str, remote: &str) -> bool {
    let curr_parts: Vec<u32> = current.split('.').filter_map(|s| s.parse().ok()).collect();
    let rem_parts: Vec<u32> = remote.split('.').filter_map(|s| s.parse().ok()).collect();

    for i in 0..std::cmp::max(curr_parts.len(), rem_parts.len()) {
        let c = curr_parts.get(i).unwrap_or(&0);
        let r = rem_parts.get(i).unwrap_or(&0);
        if r > c {
            return true;
        }
        if r < c {
            return false;
        }
    }
    false
}

/// Pings the API and checks if an update is available. Fully portable (just
/// an HTTP GET), kept as-is from the windows version.
pub fn check_for_updates(current_version: &str) -> Result<Option<UpdateInfo>, String> {
    let resp = ureq::get("https://api.hebnix.com/info")
        .set("User-Agent", "Mozilla/5.0 (X11; Linux x86_64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/120.0.0.0 Safari/537.36")
        .timeout(Duration::from_secs(10))
        .call()
        .map_err(|e| format!("Network error while checking for updates: {e}"))?;

    let json: Value = resp
        .into_json()
        .map_err(|e| format!("Invalid JSON response: {e}"))?;

    let latest_version = json
        .get("latest_version")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();
    let setup_url = json
        .get("setup_url")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if latest_version.is_empty() || setup_url.is_empty() {
        return Err("API response missing 'latest_version' or 'setup_url'".to_string());
    }

    if is_newer_version(current_version, &latest_version) {
        Ok(Some(UpdateInfo {
            version: latest_version,
            setup_url,
        }))
    } else {
        Ok(None)
    }
}

/// linux-port: the windows version downloads a zip containing `setup.exe`
/// and execs it. There's no Linux installer artifact published yet (this
/// port doesn't produce one), so self-update is disabled here rather than
/// pretending to run a Windows PE binary. TODO(linux-port): once there's a
/// Linux release artifact (AppImage/tarball/AUR package), wire this up to
/// download + swap it in, or just point users at the package manager.
pub fn download_and_install_update(_setup_url: &str, _base_dir: &Path) -> Result<(), String> {
    Err(
        "In-app updates aren't available on Linux yet -- please update Hebnix through however \
         you installed it (package manager, AppImage, etc)."
            .to_string(),
    )
}
