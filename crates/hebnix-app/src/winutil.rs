//! linux bits: single-instance lock, run-at-startup (.desktop autostart),
//! focus handoff, kill/restart RL, epic multihome ini editing.
//!
//! There's no portable Wayland equivalent of Win32's window-management APIs
//! (SetForegroundWindow, topmost z-order, SetLayeredWindowAttributes, etc) --
//! those are compositor-side concerns on Wayland, not something a client can
//! do to itself or to other windows. The handful of functions that did that
//! on Windows (`set_main_window_topmost`, `focus_main_window`,
//! `set_main_window_invisible`'s visual layering, the minimize subclass hook)
//! are kept as best-effort no-ops here so callers don't need to change; if
//! Hyprland's IPC grows the right requests later this is the place to wire
//! them in (see `hebnix_sdk::process::window` for the existing Hyprland IPC
//! client pattern).

use std::sync::atomic::{AtomicBool, Ordering};

/// linux has no HWND; kept as an opaque unit so call sites (`if let
/// Some(hwnd) = winutil::main_window_hwnd()`) don't need to change.
pub type WindowHandle = ();

fn runtime_dir() -> std::path::PathBuf {
    std::env::var("XDG_RUNTIME_DIR")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|_| std::env::temp_dir())
}

fn lock_path(name: &str) -> std::path::PathBuf {
    runtime_dir().join(format!("hebnix_{name}.lock"))
}

/// flock-based single-instance guard. the fd is leaked (kept open) for the
/// process lifetime -- the lock releases automatically when the process
/// exits or dies, same "don't bother cleaning up" behavior as the windows
/// named-mutex version.
pub struct SingleInstanceLock {
    _file: std::fs::File,
}

fn acquire_lock(name: &str) -> Option<SingleInstanceLock> {
    use std::fs::OpenOptions;
    use std::os::fd::AsRawFd;

    let path = lock_path(name);
    let file = OpenOptions::new()
        .create(true)
        .write(true)
        .open(&path)
        .ok()?;
    match nix::fcntl::flock(file.as_raw_fd(), nix::fcntl::FlockArg::LockExclusiveNonblock) {
        Ok(()) => Some(SingleInstanceLock { _file: file }),
        Err(_) => None, // already held by another instance
    }
}

pub fn acquire_single_instance() -> Option<SingleInstanceLock> {
    acquire_lock("single_instance")
}

/// a second instance calls this before exiting. We have no reliable
/// client-side way to raise another process's window on Wayland, so this
/// just logs -- the user has to alt-tab/click the taskbar entry themselves.
pub fn focus_existing_instance() {
    tracing::info!("hebnix is already running (another instance holds the single-instance lock)");
}

/// no HWND concept on Wayland; always None (logged once) so callers fall
/// back to their "not found at startup" path.
pub fn main_window_hwnd() -> Option<WindowHandle> {
    static WARNED: std::sync::Once = std::sync::Once::new();
    WARNED.call_once(|| {
        tracing::debug!(
            "winutil: no client-side window handle on Wayland, minimize-hook/dpi-fix are no-ops"
        );
    });
    None
}

pub fn set_main_window_topmost(_topmost: bool) {}

pub fn install_minimize_hook(_hwnd: WindowHandle, _ctx: &eframe::egui::Context) {}

// focus handoff

static MAIN_HIDDEN: AtomicBool = AtomicBool::new(false);
static CAME_FROM_GAME: AtomicBool = AtomicBool::new(false);

/// best-effort: on Hyprland we can ask via IPC whether we're the active
/// window; elsewhere (no reliable signal) assume not.
pub fn foreground_window_is_ours() -> bool {
    false
}

pub fn main_window_hidden() -> bool {
    MAIN_HIDDEN.load(Ordering::Relaxed)
}

pub fn note_foreground() {
    if foreground_window_is_ours() {
        return;
    }
    CAME_FROM_GAME.store(
        hebnix_sdk::process::is_rocket_league_focused(),
        Ordering::Relaxed,
    );
}

/// hand focus back to RL. no portable way to force-focus another app's
/// window from a Wayland client, so this is a no-op that just reports
/// whether it *would* apply (kept so callers' control flow is unchanged).
pub fn restore_foreground() -> bool {
    CAME_FROM_GAME.load(Ordering::Relaxed) && hebnix_sdk::process::rocket_league_hwnd().is_some()
}

/// Hyprland-only: pops our window on top of a fullscreened RL, on RL's own
/// workspace (mirrors BakkesMod's overlay-toggle behavior on Windows). The
/// real OS-level show is done separately via ViewportCommand::Visible from
/// the caller; this just handles raising + focus.
pub fn focus_main_window() -> bool {
    hebnix_sdk::process::focus_own_window_over_game(std::process::id())
}

static MINIMIZE_REQUESTED: AtomicBool = AtomicBool::new(false);
static SHOW_REQUESTED: AtomicBool = AtomicBool::new(false);

pub fn take_minimize_request() -> bool {
    MINIMIZE_REQUESTED.swap(false, Ordering::Relaxed)
}

pub fn take_show_request() -> bool {
    SHOW_REQUESTED.swap(false, Ordering::Relaxed)
}

/// "invisible" on windows dropped the layered-window alpha to 0; there's no
/// client-side equivalent on Wayland (a hidden toplevel is just unmapped by
/// the compositor when minimized). We track the intent so the rest of the
/// app's hidden-state logic (monitor.rs, overlay topmost decisions) keeps
/// working, we just can't actually hide the window ourselves here -- the
/// app's own egui viewport visibility (`ViewportCommand::Visible`) is what
/// should be driven from this flag at the call site.
pub fn set_main_window_invisible(invisible: bool) {
    MAIN_HIDDEN.store(invisible, Ordering::Relaxed);
}

// run-at-startup via XDG autostart

fn autostart_path() -> std::path::PathBuf {
    dirs::config_dir()
        .unwrap_or_else(|| std::path::PathBuf::from("."))
        .join("autostart")
        .join("hebnix.desktop")
}

pub fn is_startup_enabled() -> bool {
    autostart_path().is_file()
}

pub fn set_startup_enabled(enabled: bool) -> std::io::Result<()> {
    let path = autostart_path();
    if enabled {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let exe = std::env::current_exe()?;
        let contents = format!(
            "[Desktop Entry]\n\
             Type=Application\n\
             Name=Hebnix\n\
             Comment=Rocket League companion tool\n\
             Exec=\"{}\"\n\
             Icon=hebnix\n\
             Terminal=false\n\
             X-GNOME-Autostart-enabled=true\n",
            exe.display()
        );
        std::fs::write(&path, contents)
    } else {
        match std::fs::remove_file(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(e),
        }
    }
}

// kill / restart RL

pub fn kill_rocket_league() -> std::io::Result<()> {
    use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, Signal, System};
    let mut sys = System::new();
    sys.refresh_processes_specifics(ProcessesToUpdate::All, true, ProcessRefreshKind::nothing());
    for (_, proc_) in sys.processes() {
        if proc_
            .name()
            .to_string_lossy()
            .to_lowercase()
            .contains("rocketleague")
        {
            proc_.kill_with(Signal::Kill);
        }
    }
    Ok(())
}

fn launch_uri(uri: &str) -> std::io::Result<()> {
    // `xdg-open` dispatches steam:// (and most custom schemes Proton/Steam
    // registers) to the right handler; falls back to `steam` directly for
    // the common rungameid case since that's always available if Steam is
    // installed at all.
    if uri.starts_with("steam://") {
        if std::process::Command::new("steam").arg(uri).spawn().is_ok() {
            return Ok(());
        }
    }
    std::process::Command::new("xdg-open").arg(uri).spawn().map(|_| ())
}

pub fn restart_rocket_league(_game_path: &std::path::Path) -> std::io::Result<()> {
    let _ = kill_rocket_league();
    for _ in 0..60 {
        if !hebnix_sdk::process::is_rocket_league_running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    // linux port: Epic Games Launcher relaunching isn't wired up (RL on
    // Linux is overwhelmingly a Steam install); Steam is the only path.
    // 252950 is RL's native Steam appid -- wrong for an Epic-linked install
    // added to Steam as a non-Steam shortcut (those get a large synthetic
    // rungameid instead), so let that be overridden per-install.
    let appid = std::env::var("HEBNIX_RL_APPID").unwrap_or_else(|_| "252950".to_string());
    launch_uri(&format!("steam://rungameid/{appid}"))
}

/// Workshop LAN "restart with -multihome=<address>". A native/Proton Steam
/// install (a real, owned Steam listing) just gets "-multihome=<address>"
/// as the override launch option via `steam://run/<appid>//<options>/`,
/// matching Windows exactly.
///
/// But RL has no official Linux/Steam listing anymore, so it's extremely
/// common to add it as a non-Steam shortcut whose actual target is a
/// separate launcher (Heroic, Lutris, ...) - and Steam's `run` verb (the
/// only one that supports an options override at all) flatly refuses that:
/// non-Steam shortcuts have no store-page "configuration" for it to run,
/// so Steam just errors "Game configuration unavailable" (verified live).
/// There's no way to get an extra argument into a shortcut's launch through
/// Steam's URI scheme at all.
///
/// `HEBNIX_RL_MULTIHOME_COMMAND_TEMPLATE`, if set, is spawned directly
/// instead (bypassing Steam for this one relaunch - no Steam overlay during
/// the hosted session, but it actually works), with `{multihome}`
/// ("-multihome=<address>", raw) and `{multihome_encoded}`
/// ("-multihome%3D<address>", percent-encoded for embedding in a URI query
/// string) placeholders substituted in, then shell-word-split into a
/// program + args. For Heroic: call Heroic's own binary directly (not raw
/// `legendary`) so it assembles the launch exactly like its own GUI would -
/// correct Proton/wine version, prefix, EAC runtime, Steam Runtime, GameMode
/// wrapper, all read from Heroic's own per-game config, not guessed at here.
/// Heroic's `heroic://launch` deep link forwards repeated `&arg=` query
/// params straight through to the game (verified against Heroic's own
/// protocol handler source):
///
///   HEBNIX_RL_MULTIHOME_COMMAND_TEMPLATE=/opt/Heroic/heroic --no-gui --no-sandbox "heroic://launch?appName=Sugar&runner=legendary&arg={multihome_encoded}"
pub fn restart_rocket_league_multihome(
    _game_path: &std::path::Path,
    address: &str,
) -> Result<(), String> {
    kill_rocket_league().map_err(|error| error.to_string())?;
    for _ in 0..60 {
        if !hebnix_sdk::process::is_rocket_league_running() {
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(250));
    }
    let raw = format!("-multihome={address}");
    let encoded = format!("-multihome%3D{address}");

    if let Ok(template) = std::env::var("HEBNIX_RL_MULTIHOME_COMMAND_TEMPLATE") {
        let rendered = template
            .replace("{multihome_encoded}", &encoded)
            .replace("{multihome}", &raw);
        let parts = shell_words::split(&rendered)
            .map_err(|error| format!("invalid HEBNIX_RL_MULTIHOME_COMMAND_TEMPLATE: {error}"))?;
        let (program, args) = parts
            .split_first()
            .ok_or_else(|| "HEBNIX_RL_MULTIHOME_COMMAND_TEMPLATE is empty".to_string())?;
        tracing::info!("multihome relaunch: spawning {program} {args:?}");
        std::process::Command::new(program)
            .args(args)
            .spawn()
            .map(|_| ())
            .map_err(|error| error.to_string())
    } else {
        let appid = std::env::var("HEBNIX_RL_APPID").unwrap_or_else(|_| "252950".to_string());
        let uri = format!("steam://run/{appid}//{raw}/");
        tracing::info!("multihome relaunch: opening {uri}");
        launch_uri(&uri).map_err(|error| error.to_string())
    }
}

pub fn clear_rocket_league_multihome() -> Result<(), String> {
    // no-op: apply_epic_multihome's ini edits are Windows Epic-launcher-path
    // specific and multihome LAN isn't wired up on Linux (see
    // multiplayer_lan/mod.rs), so there's nothing to clear.
    Ok(())
}
