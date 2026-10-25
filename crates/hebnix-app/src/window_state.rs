//! Windows registers its main HWND here so background threads can wake the
//! UI and update the taskbar button. Linux has no HWND; a hidden window is
//! woken through the compositor instead (winutil::wake_minimized_main_window).

pub fn init<T>(_window: &T) {}

/// wake the UI thread for a control request, even while the window is
/// minimized to the tray (a minimized Wayland window gets no redraws)
pub fn wake_ui(ctx: &eframe::egui::Context) {
    crate::winutil::wake_minimized_main_window();
    ctx.request_repaint_of(eframe::egui::ViewportId::ROOT);
}
