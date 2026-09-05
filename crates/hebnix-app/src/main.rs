mod app;
mod patcher;
mod ball {
    pub use crate::patcher::ball::*;
}
mod boost_patcher {
    pub use crate::patcher::boost_patcher::*;
}
mod config;
mod cosmetic_thumbnail {
    pub use crate::patcher::cosmetic_thumbnail::*;
}
mod cosmetic_upk {
    pub use crate::patcher::cosmetic_upk::*;
}
mod decal_patcher {
    pub use crate::patcher::decal_patcher::*;
}
mod dpi_fix;
mod hotkey;
mod messages;
mod monitor;
mod multiplayer_lan;
mod overlay;
mod patch_core {
    pub use crate::patcher::patch_core::*;
}
mod plugins;
mod presets;
mod spoofer;
mod statsapi_ini;
mod swapper {
    pub use crate::patcher::swapper::*;
}
mod theme;
mod tray;
mod ui;
mod upk_keys {
    pub use crate::patcher::upk_keys::*;
}
mod watchdog;
mod webview;
mod winutil;

use app::HebnixApp;

fn load_window_icon(base_dir: &std::path::Path) -> Option<eframe::egui::IconData> {
    let bytes =
        std::fs::read(base_dir.join("hebnix.ico")).unwrap_or_else(|_| tray::EMBEDDED_ICON.to_vec());
    let img = image::load_from_memory(&bytes).ok()?;
    let rgba = img.to_rgba8();
    let (width, height) = rgba.dimensions();
    Some(eframe::egui::IconData {
        rgba: rgba.into_raw(),
        width,
        height,
    })
}

fn setup_logging(base_dir: &std::path::Path) {
    use tracing_subscriber::fmt::writer::MakeWriterExt;

    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| "info,wgpu_core=warn,wgpu_hal=warn,naga=warn".into());

    // sync writer, nothing buffered so the log survives a hard crash.
    // volume's low enough that sync writes don't matter.
    let log_file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(base_dir.join("hebnix.log"))
        .ok();

    match log_file {
        Some(file) => {
            let file = std::sync::Mutex::new(file);
            tracing_subscriber::fmt()
                .with_env_filter(filter)
                .with_ansi(false)
                .with_writer(file.and(std::io::stdout))
                .init();
        }
        None => {
            tracing_subscriber::fmt().with_env_filter(filter).init();
        }
    }
}

/// on panic dump msg + backtrace to crash.txt next to the exe, also log it,
/// then fall through to the default hook.
fn setup_panic_hook(base_dir: &std::path::Path) {
    let crash_path = base_dir.join("crash.txt");
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let _ = spoofer::hosts::clear();
        let backtrace = std::backtrace::Backtrace::force_capture();
        let ts = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let thread = std::thread::current();
        let text = format!(
            "=== PANIC (unix time {ts}, thread {:?}) ===\n{info}\n\nbacktrace:\n{backtrace}\n\n",
            thread.name().unwrap_or("<unnamed>")
        );
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&crash_path)
        {
            use std::io::Write;
            let _ = f.write_all(text.as_bytes());
        }
        tracing::error!("PANIC: {info}");
        default_hook(info);
    }));
}

fn main() -> eframe::Result {
    // WebKitGTK's own escape hatch for a driver bug in its GPU-accelerated
    // compositing path (DMA-BUF + explicit sync via
    // wp_linux_drm_syncobj_surface_v1) -- confirmed live to throw a fatal
    // Wayland protocol error ("Missing acquire timeline") that GTK treats
    // as unrecoverable and aborts the whole process on, at least on this
    // NVIDIA/Wayland combo. The html plugin overlay (webview.rs) is plain
    // CSS/DOM, nothing worth GPU-compositing, so this is a pure safety net.
    // Must be set before any other thread exists (env vars aren't safely
    // mutable once threads that might read the environment are running),
    // so first thing in main(), before anything else spawns.
    // SAFETY: single-threaded here, nothing else has run yet.
    unsafe {
        std::env::set_var("WEBKIT_DISABLE_COMPOSITING_MODE", "1");
        // GTK3 defaults to the X11 backend (via XWayland, which is present
        // here -- DISPLAY is set) unless told otherwise, even under a
        // native Wayland session. gtk-layer-shell requires a real Wayland
        // GdkWindow to do anything -- on an X11-backed window
        // init_layer_shell() silently no-ops, so both the tray icon and the
        // html overlay's window need this forced.
        std::env::set_var("GDK_BACKEND", "wayland");
    }

    let base_dir = config::base_dir();
    if let Some(parent_pid) = watchdog::parent_pid() {
        watchdog::run(parent_pid);
        return Ok(());
    }
    setup_logging(&base_dir);
    setup_panic_hook(&base_dir);
    tracing::info!("Hebnix {} starting", app::APP_VERSION);

    let cfg = config::Config::load(&base_dir);

    // relaunch elevated (via pkexec) if the user asked for it. --no-elevate
    // comes back when the polkit auth dialog was declined
    let skip_elevate = std::env::args().any(|a| a == spoofer::SKIP_ELEVATE_ARG);
    if cfg.settings.run_as_admin && !skip_elevate && !spoofer::is_admin() {
        if spoofer::spawn_elevated_relaunch() {
            return Ok(());
        }
        tracing::warn!("couldnt spawn the elevated relaunch helper (is polkit/pkexec installed?)");
    }

    // single instance guard
    let Some(_lock) = winutil::acquire_single_instance() else {
        winutil::focus_existing_instance();
        return Ok(());
    };

    spoofer::restore_if_crashed(&base_dir);
    let _ = watchdog::spawn();

    #[cfg(target_os = "linux")]
    hebnix_sdk::process::exempt_own_window_decorations();

    let mut viewport = eframe::egui::ViewportBuilder::default()
        .with_title("Hebnix")
        .with_inner_size([
            (cfg.window.width as f32).max(app::MIN_WIDTH),
            (cfg.window.height as f32).max(app::MIN_HEIGHT),
        ])
        .with_min_inner_size([app::MIN_WIDTH, app::MIN_HEIGHT])
        .with_transparent(true)
        .with_visible(!cfg.settings.start_in_tray);
    if let Some(icon) = load_window_icon(&base_dir) {
        viewport = viewport.with_icon(icon);
    }

    // linux-port: the windows build forces a DX12 wgpu backend bound to a
    // DirectComposition visual (dcomp_wgpu_options in the reference). On
    // Wayland the default eframe/wgpu backend selection (Vulkan via
    // WGPU_BACKEND, falling back to GL through the "glow" feature already
    // enabled in Cargo.toml) is the right call -- no analogous backend
    // pinning needed.
    let options = eframe::NativeOptions {
        viewport,
        // linux-port: plugin windows render as egui "immediate viewports" -
        // synchronous nested paints called mid-frame, inside the outer
        // winit/wayland event dispatch. Their SwapBuffers call pumps the
        // same wl_display connection the outer dispatch is already
        // pumping; with vsync on, NVIDIA's EGL-on-Wayland layer blocks
        // that nested SwapBuffers waiting on a frame callback only the
        // (currently-blocked) outer dispatch could ever deliver, freezing
        // the whole app the moment any plugin window is visible. Disabling
        // vsync makes SwapBuffers return immediately instead of waiting on
        // the compositor, breaking that deadlock.
        glow_options: eframe::egui_glow::GlowConfiguration {
            vsync: false,
            ..Default::default()
        },
        ..Default::default()
    };

    eframe::run_native(
        "Hebnix",
        options,
        Box::new(|cc| Ok(Box::new(HebnixApp::new(cc)))),
    )
}
