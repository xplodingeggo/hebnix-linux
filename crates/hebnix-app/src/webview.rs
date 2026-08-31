//! HTML plugin overlays (`plugin.overlay_page` + `hebnix.overlay.send`),
//! linux-port backend.
//!
//! Windows renders this through a WebView2 composition controller bound
//! into the same DirectComposition visual tree as the game overlay. There's
//! no Wayland equivalent of that composition API, so this takes a different
//! (and honestly simpler) route: a dedicated click-through
//! `gtk-layer-shell` window on the `Overlay` layer, hosting one
//! `webkit2gtk::WebView` whose page is a small generated shell containing
//! one `<iframe>` per plugin that sets `overlay_page`. `hebnix.overlay.send`
//! delivers data by running JS that does
//! `iframe.contentWindow.postMessage(data, '*')`, matching how the plugin
//! pages already listen (`window.addEventListener('message', ...)`) since
//! that's the same mechanism Windows' host page uses per plugin's own
//! `window.addEventListener('message')` in the page itself.
//!
//! GTK/WebKit's GLib main loop is never run as its own `gtk::main()` -- this
//! app already owns the process's main loop via winit/eframe, so instead
//! `WebviewOverlay::pump()` is called once a frame from the main tick and
//! drains pending GLib main-context events non-blockingly
//! (`gtk::main_iteration_do(false)`), which is what actually drives WebKit
//! painting, JS execution and the layer-shell surface's own Wayland events.

use std::path::{Path, PathBuf};

use gtk::prelude::*;
use gtk_layer_shell::LayerShell;
use webkit2gtk::WebViewExt;

/// (slug, overlay_page filename, plugin's own assets dir)
pub type PageLayer = (String, String, PathBuf);

pub struct WebviewOverlay {
    window: Option<gtk::Window>,
    webview: Option<webkit2gtk::WebView>,
    active: Vec<PageLayer>,
    visible: bool,
    last_error: Option<String>,
}

fn file_url(path: &Path) -> String {
    url::Url::from_file_path(path)
        .map(|u| u.to_string())
        .unwrap_or_else(|_| format!("file://{}", path.display()))
}

fn json_escape(s: &str) -> String {
    serde_json::to_string(s).unwrap_or_else(|_| "\"\"".to_string())
}

impl WebviewOverlay {
    pub fn new() -> Self {
        Self {
            window: None,
            webview: None,
            active: Vec::new(),
            visible: false,
            last_error: None,
        }
    }

    /// drains pending GLib/GTK main-context work (WebKit painting, JS
    /// callbacks, layer-shell surface events). Cheap no-op when idle. Call
    /// once a frame regardless of whether a webview currently exists --
    /// also what actually services the tray icon's DBus menu.
    pub fn pump() {
        while gtk::events_pending() {
            gtk::main_iteration_do(false);
        }
    }

    /// true once at least one plugin's html overlay is loaded and ticking.
    pub fn is_active(&self) -> bool {
        self.webview.is_some()
    }

    pub fn take_error(&mut self) -> Option<String> {
        self.last_error.take()
    }

    fn create_window(&mut self) -> Result<(), String> {
        static GTK_INIT: std::sync::Once = std::sync::Once::new();
        let mut init_err = None;
        GTK_INIT.call_once(|| {
            if let Err(e) = gtk::init() {
                init_err = Some(e.to_string());
            }
        });
        if let Some(e) = init_err {
            return Err(format!("gtk::init() failed: {e}"));
        }

        let window = gtk::Window::new(gtk::WindowType::Toplevel);
        window.set_decorated(false);
        window.set_app_paintable(true);
        if let Some(screen) = gtk::prelude::GtkWindowExt::screen(&window) {
            if let Some(visual) = screen.rgba_visual() {
                window.set_visual(Some(&visual));
            }
        }

        window.init_layer_shell();
        window.set_layer(gtk_layer_shell::Layer::Overlay);
        window.set_namespace("hebnix-html-overlay");
        for edge in [
            gtk_layer_shell::Edge::Top,
            gtk_layer_shell::Edge::Bottom,
            gtk_layer_shell::Edge::Left,
            gtk_layer_shell::Edge::Right,
        ] {
            window.set_anchor(edge, true);
        }
        window.set_exclusive_zone(-1);
        // pure HUD, never grabs keyboard/pointer -- plugin pages aren't
        // interactive, same click-through intent as the draw overlay.
        window.set_keyboard_interactivity(false);
        window.set_keyboard_mode(gtk_layer_shell::KeyboardMode::None);
        let empty_region = cairo::Region::create();
        window.input_shape_combine_region(Some(&empty_region));

        // WEBKIT_DISABLE_COMPOSITING_MODE is set once at process start in
        // main() -- must happen before any other thread exists, so it can't
        // safely be set here (this runs long after startup, other threads
        // already running).
        let webview = webkit2gtk::WebView::new();
        // WebKit's GPU-accelerated compositing path (DMA-BUF + explicit
        // sync, wp_linux_drm_syncobj_surface_v1) hits a driver bug on at
        // least this system's NVIDIA/Wayland combo -- confirmed live: it
        // throws a fatal Wayland protocol error ("Missing acquire
        // timeline") that GTK treats as unrecoverable and aborts the whole
        // process on. This HUD is plain CSS/DOM with no video/canvas load,
        // so there's nothing worth GPU-compositing here anyway -- force
        // software rendering to sidestep the driver bug entirely.
        if let Some(settings) = webkit2gtk::WebViewExt::settings(&webview) {
            webkit2gtk::SettingsExt::set_hardware_acceleration_policy(
                &settings,
                webkit2gtk::HardwareAccelerationPolicy::Never,
            );
        }
        webview.set_background_color(&gdk::RGBA::new(0.0, 0.0, 0.0, 0.0));
        window.add(&webview);
        window.show_all();

        self.window = Some(window);
        self.webview = Some(webview);
        Ok(())
    }

    fn teardown(&mut self) {
        if let Some(window) = self.window.take() {
            window.close();
        }
        self.webview = None;
        self.active.clear();
    }

    /// (re)build the host page if the active plugin set changed since the
    /// last call. Cheap no-op otherwise -- this runs every frame.
    pub fn sync_pages(&mut self, pages: &[PageLayer]) {
        if pages.is_empty() {
            if !self.active.is_empty() {
                self.teardown();
            }
            return;
        }
        if pages == self.active.as_slice() {
            return;
        }
        self.active = pages.to_vec();

        if self.webview.is_none() {
            if let Err(e) = self.create_window() {
                self.last_error = Some(e);
                return;
            }
        }
        let Some(webview) = &self.webview else { return };

        let mut iframes = String::new();
        for (slug, page, assets_dir) in pages {
            let src = file_url(&assets_dir.join(page));
            iframes.push_str(&format!(
                "<iframe id=\"frame-{id}\" src=\"{src}\" \
                 style=\"position:absolute;inset:0;width:100%;height:100%;border:0;\"></iframe>\n",
                id = html_escape(slug),
            ));
        }
        let html = format!(
            "<!doctype html><html><head><meta charset=\"utf-8\"><style>\
             html,body{{margin:0;padding:0;background:transparent;overflow:hidden;}}\
             iframe{{pointer-events:none;background:transparent;}}\
             </style></head><body>{iframes}<script>\
             window.__hebnixDeliver = function(slug, data) {{\
               var el = document.getElementById('frame-' + slug);\
               if (el && el.contentWindow) el.contentWindow.postMessage(data, '*');\
             }};\
             </script></body></html>"
        );
        webview.load_html(&html, Some("hebnix-overlay:///"));
    }

    /// push `data` (already-serialized JSON) into the named plugin's iframe.
    /// `slug` must be run through the same `html_escape` used for the
    /// iframe's own id in `sync_pages`, or the lookup silently misses.
    pub fn deliver(&self, slug: &str, data: &serde_json::Value) {
        let Some(webview) = &self.webview else { return };
        let script = format!(
            "window.__hebnixDeliver && window.__hebnixDeliver({}, {})",
            json_escape(&html_escape(slug)),
            data
        );
        webview.run_javascript(&script, gtk::gio::Cancellable::NONE, |_| {});
    }

    pub fn show(&mut self) {
        if self.visible {
            return;
        }
        if let Some(window) = &self.window {
            window.show();
            self.visible = true;
        }
    }

    pub fn hide(&mut self) {
        if !self.visible {
            return;
        }
        if let Some(window) = &self.window {
            window.hide();
        }
        self.visible = false;
    }
}

fn html_escape(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

impl Default for WebviewOverlay {
    fn default() -> Self {
        Self::new()
    }
}
