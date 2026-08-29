//! cleanup watchdog: a *separate* subprocess (`hebnix-app --cleanup-watchdog
//! <owner-pid>`) that outlives the main app and undoes anything that needs
//! undoing (hosts file redirect, multihome state) if the main process dies
//! without cleaning up after itself.
//!
//! Since the owner is a real separate process (exec'd, not a thread), we
//! can't `waitpid` on it -- that only works for actual child processes, and
//! by the time this runs it's re-parented to init/systemd like any other
//! detached process. Poll `kill(pid, None)` (liveness probe, sends no
//! signal) instead, the standard /proc-less way to check if a pid is alive.

use nix::sys::signal::kill;
use nix::unistd::Pid;

pub const CLEANUP_WATCHDOG_ARG: &str = "--cleanup-watchdog";

fn pid_alive(pid: u32) -> bool {
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

/// spawn the watchdog subprocess. Only makes sense once we're already
/// running as root (it exists to clean up /etc/hosts edits, which need
/// root) -- mirrors the windows version's admin-only gate.
pub fn spawn() -> bool {
    if !crate::spoofer::is_admin() {
        return false;
    }
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let owner = std::process::id();
    if let Some(parent) = watchdog_owner_path().parent() {
        let _ = std::fs::create_dir_all(parent);
    }
    let _ = std::fs::write(watchdog_owner_path(), owner.to_string());
    std::process::Command::new(exe)
        .args([CLEANUP_WATCHDOG_ARG, &owner.to_string()])
        .spawn()
        .is_ok()
}

pub fn parent_pid() -> Option<u32> {
    let mut args = std::env::args();
    while let Some(argument) = args.next() {
        if argument == CLEANUP_WATCHDOG_ARG {
            return args.next()?.parse().ok();
        }
    }
    None
}

pub fn run(parent_pid: u32) {
    while pid_alive(parent_pid) {
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    let _ = crate::spoofer::hosts::clear();
    while hebnix_sdk::process::is_rocket_league_running() {
        if replacement_is_running(parent_pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    if replacement_is_running(parent_pid) {
        return;
    }
    for _ in 0..3 {
        let _ = crate::winutil::clear_rocket_league_multihome();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let _ = crate::multiplayer_lan::cleanup_system_state();
    crate::spoofer::hosts::flush_dns();
    if watchdog_owner().is_some_and(|owner| owner == parent_pid) {
        let _ = std::fs::remove_file(watchdog_owner_path());
    }
}

fn watchdog_owner_path() -> std::path::PathBuf {
    dirs::data_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("Hebnix")
        .join("state")
        .join("watchdog_owner.pid")
}

fn watchdog_owner() -> Option<u32> {
    std::fs::read_to_string(watchdog_owner_path())
        .ok()?
        .trim()
        .parse()
        .ok()
}

fn replacement_is_running(parent_pid: u32) -> bool {
    let Some(owner) = watchdog_owner().filter(|owner| *owner != parent_pid) else {
        return false;
    };
    pid_alive(owner)
}
