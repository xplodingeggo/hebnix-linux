//! Windows registers a SYSTEM boot task that strips Hebnix's hosts redirects
//! after a crash or power loss. Linux has no equivalent yet: leftover
//! redirects are cleared the next time Hebnix starts.

use std::path::Path;

pub fn session_marker(base_dir: &Path) -> std::path::PathBuf {
    base_dir.join("state").join("proxy_session.dirty")
}

pub fn install(_base_dir: &Path) -> Result<(), String> {
    Ok(())
}
