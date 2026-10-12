//! linux-port: there is no capture backend yet. Wayland only allows screen
//! capture through the xdg-desktop-portal (a consent dialog) or
//! compositor-specific protocols, so `hebnix.screen` reports no frames.

use super::Service;

/// says once that screen reading is unavailable, then exits. the worker
/// flag stays set so nothing respawns this thread.
pub(super) fn run_worker(svc: &'static Service) {
    svc.log("[Screen] Screen colour reading is not supported on Linux yet; hebnix.screen returns nothing.");
}
