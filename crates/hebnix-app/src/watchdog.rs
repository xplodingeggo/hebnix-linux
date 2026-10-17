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

use std::sync::{Arc, Mutex, OnceLock};

use nix::sys::signal::kill;
use nix::unistd::Pid;

use crate::spoofer::SpooferManager;

// Keep the live listeners in this process after the UI closes. Recreating
// them in the cleanup child would drop Rocket League's active WebSocket.
static LIVE_SPOOFER: OnceLock<Arc<SpooferManager>> = OnceLock::new();

pub fn handoff_live_spoofer(spoofer: Arc<SpooferManager>) -> bool {
    if !hebnix_sdk::process::is_rocket_league_running()
        || !(spoofer.http_running() || spoofer.socket_running() || spoofer.relay_running())
    {
        return false;
    }
    spoofer.disable_spoofing();
    LIVE_SPOOFER.set(spoofer).is_ok() || LIVE_SPOOFER.get().is_some()
}

pub fn has_live_handoff() -> bool {
    LIVE_SPOOFER.get().is_some()
}

pub fn finish_live_handoff() {
    let Some(spoofer) = LIVE_SPOOFER.get() else {
        return;
    };
    tracing::info!("Hebnix UI closed; proxy watchdog remains active until Rocket League exits");
    while hebnix_sdk::process::is_rocket_league_running() {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    spoofer.shutdown();
    for _ in 0..5 {
        if crate::winutil::clear_rocket_league_web_cache().is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let _ = crate::winutil::clear_rocket_league_multihome();
    tracing::info!("Rocket League closed; proxy watchdog cleaned up");
}

pub const CLEANUP_WATCHDOG_ARG: &str = "--cleanup-watchdog";
static CHILD: Mutex<Option<std::process::Child>> = Mutex::new(None);
static RECOVERY_INSTALLED: std::sync::atomic::AtomicBool =
    std::sync::atomic::AtomicBool::new(false);

pub fn is_ready() -> bool {
    CHILD.lock().ok().is_some_and(|mut child| {
        child
            .as_mut()
            .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    })
}

pub fn ensure_protected(base_dir: &std::path::Path) -> Result<(), String> {
    use std::sync::atomic::Ordering;
    if !RECOVERY_INSTALLED.load(Ordering::Acquire) {
        crate::recovery_tasks::install(base_dir)?;
        RECOVERY_INSTALLED.store(true, Ordering::Release);
    }
    if !spawn() {
        return Err(
            "Could not start the Epic connection cleanup watchdog. Proxy was left off.".into(),
        );
    }
    Ok(())
}

fn pid_alive(pid: u32) -> bool {
    kill(Pid::from_raw(pid as i32), None).is_ok()
}

/// spawn the watchdog subprocess. Doesn't need to already be root itself -
/// its own hosts-file cleanup goes through spoofer::run_privileged (a
/// one-shot pkexec call) the same as everywhere else in spoofer, not by
/// assuming it inherited root.
pub fn spawn() -> bool {
    use std::io::Read;

    let Ok(mut child_slot) = CHILD.lock() else {
        return false;
    };
    if child_slot
        .as_mut()
        .is_some_and(|child| matches!(child.try_wait(), Ok(None)))
    {
        return true;
    }
    *child_slot = None;

    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let owner = std::process::id();
    if watchdog_owner_path()
        .parent()
        .is_some_and(|parent| std::fs::create_dir_all(parent).is_err())
        || std::fs::write(watchdog_owner_path(), owner.to_string()).is_err()
    {
        return false;
    }
    // the child acknowledges on this socket once it is watching, so a
    // watchdog that died on startup isn't mistaken for a running one
    let Ok(ready) = std::net::TcpListener::bind("127.0.0.1:0") else {
        return false;
    };
    let Ok(address) = ready.local_addr() else {
        return false;
    };
    if ready.set_nonblocking(true).is_err() {
        return false;
    }
    let token = rand::random::<u64>().to_string();
    let started = std::process::Command::new(&exe)
        .args([
            CLEANUP_WATCHDOG_ARG,
            &owner.to_string(),
            &address.to_string(),
            &token,
        ])
        .spawn();
    if let Ok(mut child) = started {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while std::time::Instant::now() < deadline {
            if let Ok((mut stream, _)) = ready.accept() {
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(std::time::Duration::from_millis(200)));
                let mut received = vec![0; token.len()];
                if stream.read_exact(&mut received).is_ok()
                    && received == token.as_bytes()
                    && matches!(child.try_wait(), Ok(None))
                {
                    *child_slot = Some(child);
                    return true;
                }
            }
            if !matches!(child.try_wait(), Ok(None)) {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(25));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
    let _ = std::fs::remove_file(watchdog_owner_path());
    false
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
    if pid_alive(parent_pid) {
        let args: Vec<String> = std::env::args().collect();
        if let Some(index) = args.iter().position(|arg| arg == CLEANUP_WATCHDOG_ARG) {
            if let (Some(address), Some(token)) = (args.get(index + 2), args.get(index + 3)) {
                use std::io::Write;
                if let Ok(mut stream) = std::net::TcpStream::connect(address) {
                    let _ = stream.write_all(token.as_bytes());
                }
            }
        }
    }
    // Windows also clears the hosts file here whenever the game is closed,
    // without waiting for Hebnix to exit. On Linux that needs a pkexec
    // prompt, so only the unprivileged session cache is cleaned meanwhile.
    while pid_alive(parent_pid) {
        if !hebnix_sdk::process::is_rocket_league_running() && !replacement_is_running(parent_pid) {
            let _ = crate::spoofer::clean_session_cache(&crate::config::base_dir());
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
    }
    if replacement_is_running(parent_pid) {
        return;
    }
    if crate::spoofer::hosts::has_redirects() {
        let base_dir = crate::config::base_dir();
        let _ =
            crate::spoofer::run_privileged(crate::spoofer::PrivilegedAction::ClearHosts, &base_dir);
        // only flush if we actually just cleared something - resolvectl
        // flush-caches needs its own separate polkit authorization on some
        // setups, so calling it unconditionally at the end of every run()
        // was a second, unrelated auth prompt on every close even when the
        // hosts file was never touched this session.
        crate::spoofer::hosts::flush_dns();
    }
    while hebnix_sdk::process::is_rocket_league_running() {
        if replacement_is_running(parent_pid) {
            return;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    if replacement_is_running(parent_pid) {
        return;
    }
    // Game shutdown can briefly keep WebCache files locked. Retry cleanup
    // after the process has gone.
    for _ in 0..5 {
        if crate::winutil::clear_rocket_league_web_cache().is_ok() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    for _ in 0..3 {
        let _ = crate::winutil::clear_rocket_league_multihome();
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
    let _ = crate::multiplayer_lan::cleanup_system_state();
    let _ = crate::spoofer::clean_session_cache(&crate::config::base_dir());
    if watchdog_owner().is_some_and(|owner| owner == parent_pid) {
        let _ = std::fs::remove_file(watchdog_owner_path());
    }
}

fn watchdog_owner_path() -> std::path::PathBuf {
    crate::config::base_dir()
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
