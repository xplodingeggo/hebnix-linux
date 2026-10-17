// crates/hebnix-app/src/spoofer/mod.rs
//! name spoofer. our own CA (ca), our own mitm proxy (proxy), spoofs live in rules.

pub mod ca;
pub mod crl;
pub mod dns;
pub mod hosts;
pub mod proxy;
pub mod rules;
pub mod skill_bridge;
pub mod socket;
pub use crate::recovery_tasks::session_marker;

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU8, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;

use crate::messages::AppMsg;
use crate::spoofer::rules::{
    NameRule, OwnedProductsRule, Rule, TITLE_HOST, TitleRule, TitleSettings, TitleSpoofSettings,
};
use crate::spoofer::skill_bridge::SkillBridge;
use crate::spoofer::socket::SocketProxy;

pub const PROXY_HOST: &str = "127.0.0.1";
pub const PROXY_PORT: u16 = 8080;
pub const MAX_NAME_LENGTH: usize = 32;
// Intercept Epic's account API only for name/friends, and PsyNet config only
// for title/rank routing.  These must stay independent: redirecting the game
// config host for an account-only spoof forces Rocket League's startup/auth
// path through the local proxy when it has nothing to do there.
const ACCOUNT_HOST: &str = "api.epicgames.dev";

pub fn is_admin() -> bool {
    nix::unistd::geteuid().is_root()
}

/// Exactly two things this whole feature ever needs root for: writing
/// /etc/hosts (the MITM redirect) and trusting the CA system-wide via
/// p11-kit. Everything else - the proxy itself (an unprivileged port),
/// DNS resolution, the rules engine - runs fine as a normal user. This
/// app used to relaunch its *entire self* elevated via pkexec whenever
/// spoofer was enabled (mirroring the Windows build, which really does
/// need to run fully elevated) - on Linux that dragged every child
/// process this app ever spawns into running as root too, which broke
/// in ways that took most of a session to fully chase down: Steam/Heroic
/// refusing to start as root, pkexec stripping HOME/WAYLAND_DISPLAY/etc
/// so Steam-shortcut scanning and even the display itself silently broke,
/// and a race against the app's own single-instance lock. All of that
/// goes away by never elevating the app itself at all - instead, each of
/// these two actions runs as its own tiny one-shot `pkexec` call
/// (`run_privileged`, dispatched back into this same binary via
/// `maybe_handle_privileged_cli` - see main()), and the app stays running
/// as the real user the entire time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivilegedAction {
    SetHosts,
    ClearHosts,
    InstallCa,
    UninstallCa,
}

impl PrivilegedAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::SetHosts => "set-hosts",
            Self::ClearHosts => "clear-hosts",
            Self::InstallCa => "install-ca",
            Self::UninstallCa => "uninstall-ca",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        Some(match value {
            "set-hosts" => Self::SetHosts,
            "clear-hosts" => Self::ClearHosts,
            "install-ca" => Self::InstallCa,
            "uninstall-ca" => Self::UninstallCa,
            _ => return None,
        })
    }

    fn is_hosts(self) -> bool {
        matches!(self, Self::SetHosts | Self::ClearHosts)
    }

    /// `hosts` only matters for SetHosts. The elevated helper only accepts
    /// the two hosts the spoofer ever redirects, nothing a caller makes up.
    fn run(self, base_dir: &Path, hosts: &[String]) -> Result<(), String> {
        match self {
            Self::SetHosts => {
                let allowed = [ACCOUNT_HOST, TITLE_HOST];
                let hosts = hosts
                    .iter()
                    .map(|host| {
                        allowed
                            .iter()
                            .copied()
                            .find(|allowed| allowed == host)
                            .ok_or_else(|| format!("refusing to redirect {host}"))
                    })
                    .collect::<Result<Vec<&str>, String>>()?;
                hosts::set_redirects(&hosts)?;
                hosts::flush_dns();
                Ok(())
            }
            Self::ClearHosts => {
                hosts::clear()?;
                hosts::flush_dns();
                Ok(())
            }
            Self::InstallCa => ca::install(base_dir),
            Self::UninstallCa => ca::uninstall(base_dir),
        }
    }
}

/// hidden CLI entry point: `hebnix-app --priv-action <action> <base_dir>`,
/// or `hebnix-app --priv-hosts <set-hosts|clear-hosts> [hosts...]` for the
/// hosts file. The hosts form has its own first argument so the polkit
/// policy the packages ship (packaging/polkit) can let the local user run
/// exactly that without a password, while the CA actions still ask.
/// Called at the very top of main(), before logging/GUI setup - a plain,
/// no-GUI, near-instant helper invocation, always run via `pkexec` from
/// `run_privileged` below. Returns the process exit code to use if this
/// process was invoked this way, or None if it's a normal launch.
pub const PRIV_ARG: &str = "--priv-action";
pub const PRIV_HOSTS_ARG: &str = "--priv-hosts";

pub fn maybe_handle_privileged_cli() -> Option<i32> {
    let mut args = std::env::args().skip(1);
    let hosts_only = match args.next().as_deref() {
        Some(PRIV_ARG) => false,
        Some(PRIV_HOSTS_ARG) => true,
        _ => return None,
    };
    let action = args
        .next()
        .as_deref()
        .and_then(PrivilegedAction::from_str)
        .filter(|action| !hosts_only || action.is_hosts());
    let base_dir = if hosts_only {
        Some(PathBuf::new())
    } else {
        args.next().map(PathBuf::from)
    };
    let (Some(action), Some(base_dir)) = (action, base_dir) else {
        eprintln!("usage: hebnix-app {PRIV_ARG} <action> <base_dir> [hosts...]");
        return Some(1);
    };
    let hosts: Vec<String> = args.collect();
    match action.run(&base_dir, &hosts) {
        Ok(()) => Some(0),
        Err(error) => {
            eprintln!("{error}");
            Some(1)
        }
    }
}

/// runs one of the two privileged actions, elevating via a one-shot
/// `pkexec` call if not already root. Blocks until it's done (including
/// however long a real polkit auth prompt takes) - unlike the old
/// whole-app relaunch this replaced, this never exits or replaces the
/// caller, it just waits and returns, since it's a single quick action
/// rather than "become the new long-running process."
pub fn run_privileged(action: PrivilegedAction, base_dir: &Path) -> Result<(), String> {
    run_privileged_with(action, base_dir, &[])
}

/// run_privileged with the hosts SetHosts should redirect
fn run_privileged_with(
    action: PrivilegedAction,
    base_dir: &Path,
    hosts: &[&str],
) -> Result<(), String> {
    if is_admin() {
        let hosts: Vec<String> = hosts.iter().map(|host| host.to_string()).collect();
        return action.run(base_dir, &hosts);
    }
    let Ok(exe) = std::env::current_exe() else {
        return Err("could not find our own executable path".to_string());
    };
    tracing::info!("spoofer: running privileged action {:?} via pkexec", action.as_str());
    let mut command = std::process::Command::new("pkexec");
    command.arg(exe);
    if action.is_hosts() {
        command.arg(PRIV_HOSTS_ARG).arg(action.as_str());
    } else {
        command.arg(PRIV_ARG).arg(action.as_str()).arg(base_dir);
    }
    let status = command
        .args(hosts)
        .status()
        .map_err(|error| format!("could not run pkexec: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!(
            "elevated {} failed or was declined",
            action.as_str()
        ))
    }
}

/// points /etc/hosts at the proxy for exactly `wanted`. Asks for auth only
/// when the file actually has to change.
fn set_redirects_privileged(wanted: &[&str], base_dir: &Path) -> Result<(), String> {
    if hosts::redirects_current(wanted) {
        return Ok(());
    }
    if wanted.is_empty() {
        return run_privileged(PrivilegedAction::ClearHosts, base_dir);
    }
    // the helper flushes the DNS cache itself, as root
    run_privileged_with(PrivilegedAction::SetHosts, base_dir, wanted)
}

fn marker_path(base_dir: &Path) -> PathBuf {
    ca::dir(base_dir).join("proxy_backup.json")
}

// linux-port: dropped the windows-only system-proxy (Internet Settings /
// InternetSetOptionW) backup+restore path entirely. Rocket League on Linux
// runs under Proton/Steam Play and doesn't consult the host desktop's HTTP
// proxy settings the way native Windows apps do via WinInet, so there's
// nothing analogous to back up or restore here -- the mitm proxy is reached
// purely through the /etc/hosts redirect (see hosts.rs).

pub fn restore_if_crashed(base_dir: &Path) {
    // stale marker from a pre-linux-port crash; nothing to restore anymore,
    // just clean it up.
    let _ = std::fs::remove_file(marker_path(base_dir));
    if hosts::has_redirects() {
        tracing::warn!("stale hosts redirect, clearing it");
        // best-effort: this may prompt for auth right at startup for a
        // redirect left over from an unclean exit. Leaving a stale MITM
        // redirect in place silently is worse, so this is deliberately not
        // swallowed the way it briefly was - if declined, it just tries
        // again next launch.
        if let Err(error) = run_privileged(PrivilegedAction::ClearHosts, base_dir) {
            tracing::warn!("spoofer: couldn't clear the stale hosts redirect: {error}");
        }
    }
    if let Err(error) = clean_session_cache(base_dir) {
        tracing::warn!("Could not clear stale Rocket League proxy cache: {error}");
    }
}

pub struct SpooferManager {
    base_dir: PathBuf,
    tx: Sender<AppMsg>,
    spoofed_name: Arc<Mutex<String>>,
    pub spoofed_friends: Arc<Mutex<HashMap<String, String>>>,
    pub discovered_friends: Arc<Mutex<HashMap<String, String>>>,
    pub spoofed_ranks: Arc<Mutex<HashMap<i32, (i32, f64)>>>,
    owned_products: Arc<Mutex<HashSet<i64>>>,
    reverse_proxy: Mutex<Option<SocketProxy>>,
    http_active: Arc<AtomicBool>,
    socket_active: Arc<AtomicBool>,
    game_present: Arc<AtomicBool>,
    title_settings: Arc<Mutex<TitleSettings>>,
    skill_bridge: Mutex<Option<SkillBridge>>,
    item_cleanup: crate::item_cleanup::Cleaner,
    crl: Mutex<Option<crl::CrlServer>>,
    rlapi_retained: Arc<AtomicBool>,
    // last failed privileged hosts change: (when, wanted hosts, error), so the
    // lifecycle worker retrying every tick shows one pkexec prompt, not a
    // stream of them, after the user declines one
    hosts_failure: Mutex<Option<(std::time::Instant, Vec<String>, String)>>,
    account_retained: AtomicBool,
    // Serialize redirect publication with shutdown and the lifecycle worker.
    redirect_lock: Mutex<()>,
    certificate_blocked: AtomicBool,
    closing: AtomicBool,
    lifecycle_started: AtomicBool,
    requested_options: AtomicU8,
}

/// Consumers of the shared proxy. Transport switches are derived from features.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProxyOptions {
    pub account: bool,
    pub psynet: bool,
    pub replays: bool,
}

impl ProxyOptions {
    fn bits(self) -> u8 {
        u8::from(self.account) | (u8::from(self.psynet) << 1) | (u8::from(self.replays) << 2)
    }

    fn from_bits(bits: u8) -> Self {
        Self {
            account: bits & 1 != 0,
            psynet: bits & 2 != 0,
            replays: bits & 4 != 0,
        }
    }
}

impl SpooferManager {
    pub fn game_running(&self) -> bool {
        self.game_present.load(Ordering::Acquire)
    }

    pub fn configure(&self, options: ProxyOptions) {
        self.requested_options
            .store(options.bits(), Ordering::Release);
    }

    // Only the lifecycle worker starts listeners. It also runs while the UI is hidden.
    fn sync_proxy_options(&self) -> Result<(), String> {
        let active = self.game_present.load(Ordering::Acquire)
            && !self.closing.load(Ordering::Acquire)
            && !self.certificate_blocked.load(Ordering::Acquire);
        let options = if active {
            ProxyOptions::from_bits(self.requested_options.load(Ordering::Acquire))
        } else {
            ProxyOptions::default()
        };
        // Disable consumers first; retained connections continue to relay without spoofs.
        if !options.replays && hebnix_sdk::rlapi::session::shared_game_session().enabled() {
            self.disable_rlapi();
        }
        if !options.account && self.http_active.load(Ordering::Acquire) {
            self.stop_http();
        }
        if !options.psynet && self.socket_active.load(Ordering::Acquire) {
            self.stop_socket();
        }
        if options.account && !self.http_running() {
            self.start_http()?;
        }
        if options.psynet && !self.socket_running() {
            self.start_socket()?;
        }
        if options.replays
            && (!hebnix_sdk::rlapi::session::shared_game_session().enabled()
                || !self
                    .reverse_proxy
                    .lock()
                    .map(|p| p.as_ref().is_some_and(SocketProxy::is_running))
                    .unwrap_or(false))
        {
            self.enable_rlapi()?;
        }
        self.cleanup_idle_rlapi();
        Ok(())
    }

    fn ensure_crl(&self, ca: &ca::Ca) -> Result<(), String> {
        let mut slot = self.crl.lock().map_err(|_| "CRL lock poisoned")?;
        if slot.is_some() {
            return Ok(());
        }
        *slot = Some(crl::CrlServer::start(ca.crl_der()?, self.tx.clone())?);
        Ok(())
    }

    fn maybe_stop_crl(&self) {
        let proxy_up = self
            .reverse_proxy
            .lock()
            .map(|p| p.is_some())
            .unwrap_or(false);
        if proxy_up {
            return;
        }
        if let Ok(mut slot) = self.crl.lock() {
            if let Some(s) = slot.take() {
                s.stop();
            }
        }
    }

    pub fn new(base_dir: PathBuf, tx: Sender<AppMsg>) -> Self {
        let owned_products = std::fs::read(base_dir.join("owned_products.json"))
            .ok()
            .and_then(|bytes| serde_json::from_slice::<Vec<i64>>(&bytes).ok())
            .unwrap_or_default()
            .into_iter()
            .collect();
        let item_cleanup = crate::item_cleanup::Cleaner::new(&base_dir);
        Self {
            base_dir,
            tx,
            spoofed_name: Arc::new(Mutex::new(String::new())),
            spoofed_friends: Arc::new(Mutex::new(HashMap::new())),
            discovered_friends: Arc::new(Mutex::new(HashMap::new())),
            spoofed_ranks: Arc::new(Mutex::new(HashMap::new())),
            owned_products: Arc::new(Mutex::new(owned_products)),
            reverse_proxy: Mutex::new(None),
            http_active: Arc::new(AtomicBool::new(false)),
            socket_active: Arc::new(AtomicBool::new(false)),
            game_present: Arc::new(AtomicBool::new(
                hebnix_sdk::process::is_rocket_league_running(),
            )),
            title_settings: Arc::new(Mutex::new(TitleSettings::default())),
            skill_bridge: Mutex::new(None),
            item_cleanup,
            crl: Mutex::new(None),
            rlapi_retained: Arc::new(AtomicBool::new(false)),
            account_retained: AtomicBool::new(false),
            redirect_lock: Mutex::new(()),
            certificate_blocked: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            lifecycle_started: AtomicBool::new(false),
            requested_options: AtomicU8::new(0),
            hosts_failure: Mutex::new(None),
        }
    }

    pub fn rlapi_running(&self) -> bool {
        let session = hebnix_sdk::rlapi::session::shared_game_session();
        session.enabled()
            || session.has_connection()
            || (self.rlapi_retained.load(Ordering::Acquire)
                && hebnix_sdk::process::is_rocket_league_running())
    }

    /// A game may cache localhost DNS/config endpoints even between requests.
    /// Keep the original authenticated transport available until it exits.
    pub fn relay_running(&self) -> bool {
        self.rlapi_running()
            || (self.account_retained.load(Ordering::Acquire)
                && hebnix_sdk::process::is_rocket_league_running())
    }

    fn redirect_hosts(&self) -> Vec<&'static str> {
        if self.closing.load(Ordering::Acquire) || !self.game_present.load(Ordering::Acquire) {
            return Vec::new();
        }
        let mut hosts = Vec::with_capacity(2);
        if self.http_active.load(Ordering::Relaxed)
            || (self.account_retained.load(Ordering::Acquire)
                && self.game_present.load(Ordering::Acquire))
        {
            hosts.push(ACCOUNT_HOST);
        }
        if self.socket_active.load(Ordering::Relaxed) || self.rlapi_running() {
            hosts.push(TITLE_HOST);
        }
        hosts
    }

    pub fn enable_rlapi(&self) -> Result<(), String> {
        let can_bind = is_admin() || crate::multiplayer_lan::has_net_bind_service_capability();
        if !can_bind || !ca::is_current_installed(&self.base_dir) {
            return Err("RLAPI capture requires the port 443 permission and the existing Hebnix certificate setup.".into());
        }
        let session = hebnix_sdk::rlapi::session::shared_game_session();
        session.set_enabled(true);
        let result = self
            .start_skill_bridge()
            .and_then(|_| self.ensure_reverse_proxy());
        if let Err(error) = result {
            session.set_enabled(false);
            self.stop_skill_bridge();
            self.stop_reverse_if_unused();
            return Err(error);
        }
        self.rlapi_retained.store(true, Ordering::Release);
        Ok(())
    }

    pub fn disable_rlapi(&self) {
        hebnix_sdk::rlapi::session::shared_game_session().set_enabled(false);
        // A connected game retains its relay until it disconnects. Disabling
        // API requests must not kick the game out of its current session.
        self.cleanup_idle_rlapi();
    }

    pub fn cleanup_idle_rlapi(&self) {
        if !self.http_active.load(Ordering::Relaxed)
            && !hebnix_sdk::process::is_rocket_league_running()
        {
            self.account_retained.store(false, Ordering::Release);
        }
        if !self.rlapi_running() {
            self.stop_reverse_if_unused();
            if !self.http_active.load(Ordering::Relaxed)
                && !self.socket_active.load(Ordering::Relaxed)
            {
                self.stop_skill_bridge();
            }
            self.maybe_stop_crl();
            // Cached localhost endpoints must not survive the final relay.
            // Retry on later monitor ticks if Windows still holds cache files.
            if self.rlapi_retained.load(Ordering::Acquire)
                && !self.socket_active.load(Ordering::Relaxed)
                && !hebnix_sdk::process::is_rocket_league_running()
                && crate::winutil::clear_rocket_league_web_cache().is_ok()
            {
                self.rlapi_retained.store(false, Ordering::Release);
            }
        }
    }
    pub fn owned_product_ids(&self) -> HashSet<i64> {
        self.owned_products
            .lock()
            .map(|owned| owned.clone())
            .unwrap_or_default()
    }

    pub fn set_username(&self, name: &str) {
        if let Ok(mut guard) = self.spoofed_name.lock() {
            let truncated: String = name.chars().take(MAX_NAME_LENGTH).collect();
            *guard = truncated;
        }
    }

    pub fn update_friends(&self, spoofs: HashMap<String, String>) {
        if let Ok(mut guard) = self.spoofed_friends.lock() {
            *guard = spoofs;
        }
    }

    pub fn update_ranks(&self, ranks: HashMap<i32, (i32, f64)>) {
        if let Ok(mut guard) = self.spoofed_ranks.lock() {
            *guard = ranks;
        }
    }

    fn start_skill_bridge(&self) -> Result<(), String> {
        let mut slot = self
            .skill_bridge
            .lock()
            .map_err(|_| "rank bridge lock poisoned")?;
        if slot.is_none() {
            *slot = Some(SkillBridge::start(
                Arc::clone(&self.spoofed_ranks),
                Arc::clone(&self.http_active),
                Arc::clone(&self.socket_active),
                Arc::clone(&self.game_present),
                self.tx.clone(),
                self.base_dir.join("rank_spoofer_frames.log"),
            )?);
        }
        Ok(())
    }

    fn stop_skill_bridge(&self) {
        if self.http_active.load(Ordering::Relaxed)
            || self.socket_active.load(Ordering::Relaxed)
            || self.relay_running()
        {
            return;
        }
        if let Ok(mut slot) = self.skill_bridge.lock() {
            if let Some(bridge) = slot.take() {
                bridge.stop();
            }
        }
    }

    pub fn http_running(&self) -> bool {
        self.http_active.load(Ordering::Relaxed)
            && self
                .reverse_proxy
                .lock()
                .map(|proxy| proxy.as_ref().is_some_and(SocketProxy::is_running))
                .unwrap_or(false)
    }

    pub fn socket_running(&self) -> bool {
        self.socket_active.load(Ordering::Relaxed)
            && self
                .reverse_proxy
                .lock()
                .map(|proxy| proxy.as_ref().is_some_and(SocketProxy::is_running))
                .unwrap_or(false)
    }

    pub fn start_http(&self) -> Result<(), String> {
        if self.http_running() {
            return self.reconcile_hosts();
        }
        self.http_active.store(true, Ordering::Relaxed);
        if let Err(error) = self
            .start_skill_bridge()
            .and_then(|_| self.ensure_reverse_proxy())
        {
            self.http_active.store(false, Ordering::Relaxed);
            self.stop_reverse_if_unused();
            self.stop_skill_bridge();
            self.maybe_stop_crl();
            return Err(error);
        }
        self.account_retained.store(true, Ordering::Release);
        Ok(())
    }

    pub fn stop_http(&self) {
        self.http_active.store(false, Ordering::Relaxed);
        if !self.socket_active.load(Ordering::Relaxed) {
            self.stop_skill_bridge();
        }
        self.stop_reverse_if_unused();
        self.maybe_stop_crl();
    }

    pub fn set_titles(&self, titles: Vec<TitleSpoofSettings>) {
        if let Ok(mut settings) = self.title_settings.lock() {
            let mut normalized: Vec<TitleSpoofSettings> = titles
                .into_iter()
                .filter_map(|mut title| {
                    title.text = title.text.trim().chars().take(64).collect();
                    (!title.text.is_empty()).then_some(title)
                })
                .collect();
            let mut seen = HashSet::new();
            normalized.retain(|title| seen.insert(title.target_id.clone()));
            settings.titles = normalized;
        }
    }

    pub fn set_title_enabled(&self, enabled: bool) {
        if let Ok(mut settings) = self.title_settings.lock() {
            settings.enabled = enabled;
        }
    }

    pub fn start_socket(&self) -> Result<(), String> {
        if self.socket_running() {
            return self.reconcile_hosts();
        }
        self.socket_active.store(true, Ordering::Relaxed);
        if let Err(error) = self
            .start_skill_bridge()
            .and_then(|_| self.ensure_reverse_proxy())
        {
            self.socket_active.store(false, Ordering::Relaxed);
            hebnix_sdk::rlapi::session::shared_game_session().set_websocket_enabled(false);
            self.stop_reverse_if_unused();
            self.stop_skill_bridge();
            self.maybe_stop_crl();
            return Err(error);
        }
        hebnix_sdk::rlapi::session::shared_game_session().set_websocket_enabled(true);
        self.rlapi_retained.store(true, Ordering::Release);
        Ok(())
    }

    pub fn item_status(&self) -> crate::item_cleanup::ItemStatus {
        self.item_cleanup.status()
    }
    pub fn begin_item_clear(&self) -> Result<(), String> {
        self.item_cleanup.begin_clear()
    }
    pub fn cancel_item_clear(&self) -> Result<(), String> {
        self.item_cleanup.cancel_clear()
    }
    pub fn finish_item_clear(&self) -> Result<bool, String> {
        self.item_cleanup.finish_clear()
    }

    pub fn stop_socket(&self) {
        self.socket_active.store(false, Ordering::Relaxed);
        hebnix_sdk::rlapi::session::shared_game_session().set_websocket_enabled(false);
        self.stop_reverse_if_unused();
        self.stop_skill_bridge();
        self.maybe_stop_crl();
    }

    fn ensure_reverse_proxy(&self) -> Result<(), String> {
        if !self.lifecycle_started.load(Ordering::Acquire) {
            return Err(
                "The proxy lifecycle monitor is unavailable; interception was left off.".into(),
            );
        }
        if self.certificate_blocked.load(Ordering::Acquire) {
            return Err(
                "Certificate removal disabled the proxy. Enable Spoofer or a replay option to set it up again."
                    .into(),
            );
        }
        if !ca::is_current_installed(&self.base_dir) {
            return Err(
                "The proxy certificate is not trusted. Enable Spoofer or a replay option to install it.".into(),
            );
        }
        let mut slot = self
            .reverse_proxy
            .lock()
            .map_err(|_| "reverse proxy lock poisoned")?;
        if slot.as_ref().is_some_and(|proxy| !proxy.is_running()) {
            if let Some(proxy) = slot.take() {
                proxy.stop();
            }
        }
        if slot.is_some() {
            drop(slot);
            crate::watchdog::ensure_protected(&self.base_dir)?;
            return self.reconcile_hosts();
        }
        let ca = Arc::new(ca::ensure(&self.base_dir)?);
        if !ca::is_current_installed(&self.base_dir) {
            return Err(
                "Certificate not installed. open Spoofer settings and click Install Certificate"
                    .into(),
            );
        }
        // the local MITM proxy binds 127.0.0.1:443 directly (a privileged
        // port) - needs CAP_NET_BIND_SERVICE, granted the same way as
        // Workshop LAN's CAP_NET_ADMIN (one setcap covers both). Checked
        // up front for a clear message instead of a raw OS bind error.
        if !crate::multiplayer_lan::has_net_bind_service_capability() && !is_admin() {
            return Err(
                "Needs one extra permission to bind port 443 - open Spoofer settings and click \
                 Grant Permission"
                    .into(),
            );
        }
        let mut real_ips = HashMap::new();
        // The account proxy may start before Titles turn on. Pin both possible
        // redirect targets now so a later hosts-file change cannot make an
        // unpinned upstream request resolve back to our own listener.
        for host in [ACCOUNT_HOST, TITLE_HOST] {
            real_ips.insert(host.to_string(), dns::resolve_a(host)?);
        }
        real_ips.insert(
            "api.rlpp.psynet.gg".to_string(),
            dns::resolve_a("api.rlpp.psynet.gg")?,
        );
        let spoof_rules: Vec<Box<dyn Rule>> = vec![
            Box::new(NameRule::new(Arc::clone(&self.spoofed_name))),
            Box::new(crate::spoofer::rules::FriendsRule::new(
                Arc::clone(&self.spoofed_friends),
                Arc::clone(&self.discovered_friends),
            )),
            Box::new(OwnedProductsRule::new(
                Arc::clone(&self.owned_products),
                self.base_dir.join("owned_products.json"),
            )),
            Box::new(TitleRule::new(Arc::clone(&self.title_settings))),
            Box::new(crate::spoofer::rules::RankRule::new(Arc::clone(
                &self.spoofed_ranks,
            ))),
        ];
        let mut rules: Vec<Box<dyn Rule>> = spoof_rules
            .into_iter()
            .map(|rule| {
                Box::new(crate::spoofer::rules::EnabledRule {
                    inner: rule,
                    http: Arc::clone(&self.http_active),
                    socket: Arc::clone(&self.socket_active),
                    game: Arc::clone(&self.game_present),
                }) as Box<dyn Rule>
            })
            .collect();
        rules.push(Box::new(crate::spoofer::rules::RlApiRouteRule {
            retained: Arc::clone(&self.rlapi_retained),
            game: Arc::clone(&self.game_present),
        }));
        let rules = Arc::new(rules);
        self.ensure_crl(&ca)?;
        let proxy = match SocketProxy::start(ca, rules, self.tx.clone(), real_ips) {
            Ok(proxy) => proxy,
            Err(error) => {
                drop(slot);
                self.maybe_stop_crl();
                return Err(error);
            }
        };
        // Claim the listener before registering recovery ownership, so a
        // second Windows session cannot steal a live proxy's cleanup task.
        if let Err(error) = crate::watchdog::ensure_protected(&self.base_dir) {
            proxy.stop();
            drop(slot);
            self.maybe_stop_crl();
            return Err(error);
        }
        *slot = Some(proxy);
        drop(slot);
        if let Err(error) = self.reconcile_hosts() {
            self.http_active.store(false, Ordering::Release);
            self.socket_active.store(false, Ordering::Release);
            hebnix_sdk::rlapi::session::shared_game_session().set_websocket_enabled(false);
            self.stop_reverse_if_unused();
            self.maybe_stop_crl();
            return Err(error);
        }
        Ok(())
    }

    /// /etc/hosts redirects for exactly `wanted`, through pkexec when the file
    /// has to change. A failure (usually a declined prompt) is remembered for a
    /// while so the lifecycle worker doesn't immediately prompt again.
    fn set_hosts_privileged(&self, wanted: &[&str]) -> Result<(), String> {
        const RETRY_COOLDOWN: std::time::Duration = std::time::Duration::from_secs(60);
        if hosts::redirects_current(wanted) {
            return Ok(());
        }
        let key: Vec<String> = wanted.iter().map(|host| host.to_string()).collect();
        if let Ok(guard) = self.hosts_failure.lock() {
            if let Some((at, failed, error)) = guard.as_ref() {
                if *failed == key && at.elapsed() < RETRY_COOLDOWN {
                    return Err(error.clone());
                }
            }
        }
        let result = set_redirects_privileged(wanted, &self.base_dir);
        if let Ok(mut guard) = self.hosts_failure.lock() {
            *guard = result
                .as_ref()
                .err()
                .map(|error| (std::time::Instant::now(), key, error.clone()));
        }
        result
    }

    fn clear_hosts_privileged(&self) -> Result<(), String> {
        if !hosts::has_redirects() {
            return Ok(());
        }
        self.set_hosts_privileged(&[])
    }

    /// Reconcile hosts with the shared proxy's actual runtime state.
    pub fn reconcile_hosts(&self) -> Result<(), String> {
        let _guard = self
            .redirect_lock
            .lock()
            .map_err(|_| "redirect lock poisoned")?;
        if self.certificate_blocked.load(Ordering::Acquire) {
            self.clear_hosts_privileged()?;
            if self.http_active.load(Ordering::Acquire)
                || self.socket_active.load(Ordering::Acquire)
                || hebnix_sdk::rlapi::session::shared_game_session().enabled()
            {
                return Err("Certificate removal cancelled proxy startup.".into());
            }
            return Ok(());
        }
        if self.http_running() || self.socket_running() || self.relay_running() {
            let targets = self.redirect_hosts();
            if !targets.is_empty() {
                if !self
                    .reverse_proxy
                    .lock()
                    .map_err(|_| "proxy lock poisoned")?
                    .as_ref()
                    .is_some_and(SocketProxy::is_running)
                {
                    self.clear_hosts_privileged()?;
                    return Err("Proxy listener stopped; hosts redirects were removed.".into());
                }
                if !crate::watchdog::is_ready() {
                    self.clear_hosts_privileged()?;
                    return Err(
                        "The cleanup watchdog stopped; interception has been paused.".into(),
                    );
                }
                mark_session_dirty(&self.base_dir)?;
            }
            self.set_hosts_privileged(&targets)
        } else {
            self.clear_hosts_privileged()
        }
    }

    fn stop_reverse_if_unused(&self) {
        let Ok(_guard) = self.redirect_lock.lock() else {
            return;
        };
        if self.http_active.load(Ordering::Relaxed)
            || self.socket_active.load(Ordering::Relaxed)
            || self.relay_running()
        {
            return;
        }
        if let Err(error) = self.clear_hosts_privileged() {
            let _ = self.tx.send(AppMsg::Log(format!(
                "[Spoofer] Could not remove hosts redirects: {error}"
            )));
            // Leave a working listener available if the cleanup was declined.
            return;
        }
        if let Ok(mut slot) = self.reverse_proxy.lock() {
            if let Some(proxy) = slot.take() {
                proxy.stop();
            }
        }
    }

    /// Disable mutations and API requests while preserving the game's relay.
    /// Saved spoof settings are left intact for the next Hebnix launch.
    pub fn disable_spoofing(&self) {
        // Preserve already-established/cached local game connections. New
        // DNS lookups go directly to Epic as soon as Hebnix's UI closes.
        self.closing.store(true, Ordering::Release);
        hebnix_sdk::rlapi::session::shared_game_session().set_enabled(false);
        self.stop_socket();
        self.stop_http();
        if let Err(error) = self.reconcile_hosts() {
            tracing::error!("Could not reconcile retained game relay: {error}");
        }
    }

    /// Stops only runtime interception. It deliberately does not modify saved
    /// spoof settings, so the user's enabled toggles survive the next launch.
    pub fn shutdown(&self) {
        self.configure(ProxyOptions::default());
        self.rlapi_retained.store(false, Ordering::Release);
        self.account_retained.store(false, Ordering::Release);
        hebnix_sdk::rlapi::session::shared_game_session().reset();
        self.stop_socket();
        self.stop_http();
        // Clear a redirect even if the socket failed to start or its state
        // was lost - but stop_socket()/stop_http() above already did this
        // via stop_reverse_if_unused() in the normal case, so skip the
        // redundant (and potentially auth-prompting) third call. The helper
        // flushes the DNS cache itself, as root: `resolvectl flush-caches`
        // as the user needs its own polkit authorization on some setups.
        if hosts::has_redirects() {
            let _ = run_privileged(PrivilegedAction::ClearHosts, &self.base_dir);
        }
    }

    pub fn block_for_certificate_removal(&self) {
        // Prevent an in-flight RLAPI/proxy start from publishing redirects
        // after the UI has shut down the runtime and removed trust.
        self.certificate_blocked.store(true, Ordering::Release);
        self.shutdown();
    }

    pub fn certificate_setup_ready(&self) {
        self.certificate_blocked.store(false, Ordering::Release);
    }

    /// Keep redirect lifetimes independent of UI repaints and StatsAPI probes.
    /// Enabled consumers start when the game runs and stop when it exits.
    pub fn start_lifecycle_monitor(self: &Arc<Self>) -> Result<(), String> {
        let manager = Arc::downgrade(self);
        std::thread::Builder::new()
            .name("proxy-lifecycle".into())
            .spawn(move || {
                let mut last_error = None;
                let mut trust_check = std::time::Instant::now();
                loop {
                    let Some(manager) = manager.upgrade() else {
                        break;
                    };
                    let game_present = hebnix_sdk::process::is_rocket_league_running();
                    manager.game_present.store(game_present, Ordering::Release);
                    if trust_check.elapsed() >= std::time::Duration::from_secs(5) {
                        trust_check = std::time::Instant::now();
                        if (manager.http_running()
                            || manager.socket_running()
                            || manager.relay_running())
                            && !ca::is_current_installed(&manager.base_dir)
                        {
                            manager.block_for_certificate_removal();
                            let _ = manager.tx.send(AppMsg::SpooferCertificateLost);
                        }
                    }
                    let result = manager
                        .sync_proxy_options()
                        .and_then(|_| manager.reconcile_hosts());
                    if let Err(error) = result {
                        if last_error.as_ref() != Some(&error) {
                            let _ = manager.tx.send(AppMsg::Log(format!("[Spoofer] {error}")));
                        }
                        last_error = Some(error);
                    } else {
                        last_error = None;
                    }
                    if !crate::watchdog::is_ready()
                        && (manager.http_running()
                            || manager.socket_running()
                            || manager.relay_running())
                    {
                        // Reconcile above removes redirects before replacing a
                        // killed watchdog. The next tick can safely resume.
                        let _ = crate::watchdog::spawn();
                    }
                    // A retained localhost config must not survive into the next
                    // game session. Keep the marker until cleanup succeeds.
                    if !game_present {
                        let _ = clean_session_cache(&manager.base_dir);
                    }
                    drop(manager);
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            })
            .map_err(|error| format!("Could not start proxy lifecycle monitor: {error}"))?;
        self.lifecycle_started.store(true, Ordering::Release);
        Ok(())
    }
}

fn mark_session_dirty(base_dir: &Path) -> Result<(), String> {
    let marker = session_marker(base_dir);
    if !marker.exists() {
        std::fs::create_dir_all(marker.parent().unwrap()).map_err(|e| e.to_string())?;
        std::fs::write(marker, b"Rocket League may have cached localhost endpoints")
            .map_err(|e| format!("Could not save proxy recovery state: {e}"))?;
    }
    Ok(())
}

pub fn clean_session_cache(base_dir: &Path) -> std::io::Result<()> {
    let marker = session_marker(base_dir);
    if marker.exists() && !hebnix_sdk::process::is_rocket_league_running() {
        crate::winutil::clear_rocket_league_web_cache()?;
        std::fs::remove_file(marker)?;
    }
    Ok(())
}
