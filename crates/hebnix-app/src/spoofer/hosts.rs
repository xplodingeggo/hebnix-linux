//! hosts file redirect, the game ignores any http proxy config. needs root -
//! callers go through `spoofer::run_privileged` (a one-shot pkexec call for
//! just this action) rather than expecting the whole process to already be
//! running elevated.

use std::path::{Path, PathBuf};

// our lines end with this so we can find them later
pub const MARK: &str = "# hebnix spoofer";

pub fn hosts_path() -> PathBuf {
    PathBuf::from("/etc/hosts")
}

fn line_for(host: &str) -> String {
    format!("127.0.0.1 {host} {MARK}")
}

/// point host at us. does nothing if its already there.
pub fn set_redirects(hosts: &[&str]) -> Result<(), String> {
    let path = hosts_path();
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("cant read the hosts file: {e}"))?;
    let mut kept = content
        .lines()
        .filter(|line| !is_ours(line))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    for host in hosts {
        kept.push(line_for(host));
    }
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }
    write(&path, &out)?;
    Ok(())
}

pub fn has_redirects() -> bool {
    std::fs::read_to_string(hosts_path())
        .map(|content| content.lines().any(is_ours))
        .unwrap_or(false)
}

/// drops our lines, whatever host they were for
pub fn clear() -> Result<(), String> {
    let path = hosts_path();
    let content =
        std::fs::read_to_string(&path).map_err(|e| format!("cant read the hosts file: {e}"))?;

    if !content.lines().any(is_ours) {
        return Ok(());
    }

    let kept: Vec<&str> = content.lines().filter(|l| !is_ours(l)).collect();
    let mut out = kept.join("\n");
    if !out.is_empty() {
        out.push('\n');
    }

    write(&path, &out)?;
    Ok(())
}

fn is_ours(line: &str) -> bool {
    line.contains(MARK)
}

fn write(path: &Path, content: &str) -> Result<(), String> {
    std::fs::write(path, content).map_err(|e| {
        if e.kind() == std::io::ErrorKind::PermissionDenied {
            "the hosts file needs root, restart hebnix elevated".to_string()
        } else {
            format!("cant write the hosts file: {e}")
        }
    })
}

/// glibc's resolver doesn't cache by default, but systemd-resolved (the
/// common Arch setup with NetworkManager) does -- flush it if present.
/// Best-effort: silently does nothing if resolved isn't running.
///
/// Deliberately NOT called from set_redirects/clear above, even though
/// they used to do this internally: those two run inside the root-elevated
/// `run_privileged` one-shot helper, and flushing your own session's DNS
/// cache as root (rather than as yourself) can hit a stricter/different
/// polkit rule than the same request from your own active session -
/// caused a *second* unexpected auth prompt right after the hosts-file one
/// on at least one real setup. Callers call this separately, from the
/// normal (never-elevated) app process, after run_privileged succeeds.
pub fn flush_dns() {
    let _ = std::process::Command::new("resolvectl")
        .arg("flush-caches")
        .output();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_our_lines_are_ours() {
        assert!(is_ours(&line_for("config.psynet.gg")));
        assert!(!is_ours("127.0.0.1 localhost"));
        assert!(!is_ours("# some user comment"));
    }

    #[test]
    fn line_carries_host_and_mark() {
        let l = line_for("config.psynet.gg");
        assert!(l.starts_with("127.0.0.1 config.psynet.gg"));
        assert!(l.contains(MARK));
    }
}
