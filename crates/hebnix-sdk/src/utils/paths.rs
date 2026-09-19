//! Shared Hebnix runtime and data paths.

use std::path::PathBuf;

/// The root for all Hebnix-owned files: `~/.config/hebnix` (XDG), or
/// `HEBNIX_BASE_DIR` for isolated development and test runs. Must resolve to
/// the same directory as the app crate's `config::base_dir()`.
pub fn base_dir() -> PathBuf {
    let dir = std::env::var_os("HEBNIX_BASE_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::config_dir().map(|dir| dir.join("hebnix")))
        .unwrap_or_else(|| std::env::temp_dir().join("hebnix"));

    if let Err(error) = std::fs::create_dir_all(&dir) {
        eprintln!(
            "failed to create Hebnix data directory {}: {error}",
            dir.display()
        );
    }
    dir
}
