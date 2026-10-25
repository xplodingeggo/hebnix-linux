// crates/hebnix-app/src/spoofer/mod.rs
//! name spoofer. our own CA (ca), our own mitm proxy (proxy), spoofs live in rules.

pub mod ca;
pub mod crl;
pub mod dns;
pub mod hosts;
mod http;
pub mod proxy;
pub mod rules;
mod wine_proxy;

pub use crate::recovery_tasks::cleanup_websocket_once;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;

use crate::messages::AppMsg;
use crate::spoofer::rules::{NameRule, Rule, TitleSpoofSettings};
use http::HttpProxy;

pub const PROXY_HOST: &str = "127.0.0.1";
/// HTTP proxy ports, tried in order. Windows uses a fixed 8080, which
/// steamwebhelper (and plenty of dev servers) already hold on Linux. 143 is
/// only bindable where unprivileged ports start low enough; 0 lets the
/// kernel pick a free port as the last resort.
pub const PROXY_PORTS: &[u16] = &[6969, 143, 6970, 6971, 6972, 6973, 0];

/// bind the first free port of `ports` on PROXY_HOST
pub(crate) fn bind_local(what: &str, ports: &[u16]) -> Result<std::net::TcpListener, String> {
    let mut last_error = None;
    for &port in ports {
        match std::net::TcpListener::bind((PROXY_HOST, port)) {
            Ok(listener) => {
                if let Ok(address) = listener.local_addr() {
                    tracing::info!("{what} listening on {address}");
                }
                return Ok(listener);
            }
            Err(error) => {
                tracing::debug!("{what}: {PROXY_HOST}:{port} unavailable: {error}");
                last_error = Some(error);
            }
        }
    }
    Err(format!(
        "Cannot bind {what}: {}",
        last_error.map_or_else(|| "no ports to try".to_string(), |e| e.to_string())
    ))
}
pub const MAX_NAME_LENGTH: usize = 32;
pub const WEBSOCKET_UNAVAILABLE: &str = "WebSocket proxy features are unavailable.";

pub fn is_admin() -> bool {
    nix::unistd::geteuid().is_root()
}

pub fn disable_saved_http_features(base_dir: &Path) -> std::io::Result<()> {
    for (file, flags) in [
        ("spoofer_settings.json", &["spoofer_master", "spoofer_username_enabled"][..]),
        ("friends.json", &["spoofer_friends_enabled"][..]),
    ] {
        let path = base_dir.join(file);
        if !path.is_file() {
            continue;
        }
        let text = std::fs::read_to_string(&path)?;
        let mut settings: serde_json::Value = serde_json::from_str(&text)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        if let Some(object) = settings.as_object_mut() {
            for flag in flags {
                object.insert((*flag).to_string(), serde_json::Value::Bool(false));
            }
        }
        let output = serde_json::to_vec_pretty(&settings)
            .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))?;
        std::fs::write(path, output)?;
    }
    Ok(())
}
/// The only things this feature needs root for: trusting the CA system-wide
/// via p11-kit, and clearing the /etc/hosts redirects older Linux builds
/// made (the spoofer now uses an explicit HTTP proxy set in the Wine prefix,
/// see wine_proxy.rs). The app itself never runs elevated: each action is
/// its own one-shot `pkexec` call (`run_privileged`, dispatched back into
/// this same binary via `maybe_handle_privileged_cli`, see main()).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrivilegedAction {
    ClearHosts,
    InstallCa,
    UninstallCa,
}

impl PrivilegedAction {
    fn as_str(self) -> &'static str {
        match self {
            Self::ClearHosts => "clear-hosts",
            Self::InstallCa => "install-ca",
            Self::UninstallCa => "uninstall-ca",
        }
    }

    fn from_str(value: &str) -> Option<Self> {
        Some(match value {
            "clear-hosts" => Self::ClearHosts,
            "install-ca" => Self::InstallCa,
            "uninstall-ca" => Self::UninstallCa,
            _ => return None,
        })
    }

    fn is_hosts(self) -> bool {
        matches!(self, Self::ClearHosts)
    }

    fn run(self, base_dir: &Path) -> Result<(), String> {
        match self {
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
/// or `hebnix-app --priv-hosts clear-hosts` for the hosts file. The hosts
/// form has its own first argument so the polkit policy the packages ship
/// (packaging/polkit) can let the local user run exactly that without a
/// password, while the CA actions still ask. Called at the very top of
/// main(), before logging/GUI setup, always via `pkexec` from
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
        eprintln!("usage: hebnix-app {PRIV_ARG} <action> <base_dir>");
        return Some(1);
    };
    match action.run(&base_dir) {
        Ok(()) => Some(0),
        Err(error) => {
            eprintln!("{error}");
            Some(1)
        }
    }
}

/// runs one privileged action, elevating via a one-shot `pkexec` call if
/// not already root. Blocks until it's done (including however long the
/// polkit prompt takes).
pub fn run_privileged(action: PrivilegedAction, base_dir: &Path) -> Result<(), String> {
    if is_admin() {
        return action.run(base_dir);
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

/// clears the hosts redirects an older Linux build left behind
pub fn clear_hosts_privileged(base_dir: &Path) -> Result<(), String> {
    run_privileged(PrivilegedAction::ClearHosts, base_dir)
}

fn marker_path(base_dir: &Path) -> PathBuf {
    ca::dir(base_dir).join("proxy_backup.json")
}

fn our_proxy_server(port: u16) -> String {
    wine_proxy::proxy_server(PROXY_HOST, port)
}

const WAITING_FOR_PREFIX: &str =
    "Rocket League is already running. The HTTP proxy starts with its next launch.";

#[derive(Debug, PartialEq, Eq)]
enum Restore {
    /// nothing left to restore
    Done,
    /// something still runs in the prefix; its wineserver would overwrite
    /// user.reg, so try again once it has exited
    PrefixInUse,
    Failed,
}

/// put back the prefix proxy settings saved in the marker. The marker is
/// kept until that has worked.
fn restore_prefix_proxy(base_dir: &Path) -> Restore {
    let marker = marker_path(base_dir);
    if !marker.is_file() {
        return Restore::Done;
    }
    let Some(backup) = std::fs::read(&marker)
        .ok()
        .and_then(|bytes| serde_json::from_slice::<wine_proxy::Backup>(&bytes).ok())
    else {
        tracing::error!("Cannot read the Wine proxy backup; keeping it for recovery");
        return Restore::Failed;
    };
    if hebnix_sdk::process::prefix_in_use(&backup.prefix) {
        return Restore::PrefixInUse;
    }
    // only undo our own setting; leave one the user changed since
    if wine_proxy::applied_port(&backup.prefix, PROXY_HOST).is_some() {
        if let Err(error) = wine_proxy::restore(&backup) {
            tracing::error!("Could not restore the Wine prefix proxy settings: {error}");
            return Restore::Failed;
        }
    }
    let _ = std::fs::remove_file(&marker);
    Restore::Done
}

pub fn restore_if_crashed(base_dir: &Path) {
    if marker_path(base_dir).is_file() {
        tracing::warn!("stale HTTP proxy marker, restoring the Wine prefix proxy");
        if restore_prefix_proxy(base_dir) == Restore::PrefixInUse {
            tracing::warn!("Rocket League's prefix is in use; the proxy is restored after it closes");
        }
    }
}

/// for the cleanup watchdog: restore the prefix proxy once nothing runs in
/// the prefix any more
pub fn restore_when_prefix_free(base_dir: &Path) {
    while restore_prefix_proxy(base_dir) == Restore::PrefixInUse {
        std::thread::sleep(std::time::Duration::from_secs(1));
    }
}

/// is our proxy set in a prefix that is running right now? Rocket League
/// then depends on the HTTP proxy until it exits.
pub fn prefix_proxy_in_use(base_dir: &Path) -> bool {
    restore_prefix_proxy_pending(base_dir)
        .is_some_and(|prefix| hebnix_sdk::process::prefix_in_use(&prefix))
}

fn restore_prefix_proxy_pending(base_dir: &Path) -> Option<PathBuf> {
    let bytes = std::fs::read(marker_path(base_dir)).ok()?;
    Some(serde_json::from_slice::<wine_proxy::Backup>(&bytes).ok()?.prefix)
}

pub struct SpooferManager {
    base_dir: PathBuf,
    tx: Sender<AppMsg>,
    spoofed_name: Arc<Mutex<String>>,
    pub spoofed_friends: Arc<Mutex<HashMap<String, String>>>,
    pub discovered_friends: Arc<Mutex<HashMap<String, String>>>,
    http_proxy: Mutex<Option<HttpProxy>>,
    http_active: Arc<AtomicBool>,
    game_present: Arc<AtomicBool>,
    item_cleanup: crate::item_cleanup::Cleaner,
    crl: Mutex<Option<crl::CrlServer>>,
    runtime_lock: Mutex<()>,
    certificate_blocked: AtomicBool,
    closing: AtomicBool,
    lifecycle_started: AtomicBool,
    requested_http: AtomicBool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProxyOptions {
    pub account: bool,
    pub psynet: bool,
    pub replays: bool,
}

impl ProxyOptions {
    pub fn http_only(self) -> Self {
        Self {
            account: self.account,
            psynet: false,
            replays: false,
        }
    }

    fn for_runtime(self, game: bool, closing: bool, blocked: bool) -> Self {
        if game && !closing && !blocked {
            self.http_only()
        } else {
            Self::default()
        }
    }
}

impl SpooferManager {
    pub fn new(base_dir: PathBuf, tx: Sender<AppMsg>) -> Self {
        let item_cleanup = crate::item_cleanup::Cleaner::new(&base_dir);
        Self {
            base_dir,
            tx,
            spoofed_name: Arc::new(Mutex::new(String::new())),
            spoofed_friends: Arc::new(Mutex::new(HashMap::new())),
            discovered_friends: Arc::new(Mutex::new(HashMap::new())),
            http_proxy: Mutex::new(None),
            http_active: Arc::new(AtomicBool::new(false)),
            game_present: Arc::new(AtomicBool::new(
                hebnix_sdk::process::is_rocket_league_running(),
            )),
            item_cleanup,
            crl: Mutex::new(None),
            runtime_lock: Mutex::new(()),
            certificate_blocked: AtomicBool::new(false),
            closing: AtomicBool::new(false),
            lifecycle_started: AtomicBool::new(false),
            requested_http: AtomicBool::new(false),
        }
    }

    pub fn game_running(&self) -> bool {
        self.game_present.load(Ordering::Acquire)
    }
    pub fn configure(&self, options: ProxyOptions) {
        self.requested_http
            .store(options.http_only().account, Ordering::Release);
    }
    pub fn http_running(&self) -> bool {
        self.http_active.load(Ordering::Acquire)
            && self
                .http_proxy
                .lock()
                .is_ok_and(|proxy| proxy.as_ref().is_some_and(HttpProxy::is_running))
    }
    pub fn socket_running(&self) -> bool {
        false
    }
    pub fn rlapi_running(&self) -> bool {
        false
    }
    pub fn relay_running(&self) -> bool {
        false
    }
    pub fn enable_rlapi(&self) -> Result<(), String> {
        Err(WEBSOCKET_UNAVAILABLE.into())
    }
    pub fn disable_rlapi(&self) {
        hebnix_sdk::rlapi::session::shared_game_session().reset();
    }
    pub fn cleanup_idle_rlapi(&self) {
        self.disable_rlapi();
    }
    pub fn update_ranks(&self, _ranks: HashMap<i32, (i32, f64)>) {}
    pub fn set_titles(&self, _titles: Vec<TitleSpoofSettings>) {}
    pub fn set_title_enabled(&self, _enabled: bool) {}

    pub fn set_username(&self, name: &str) {
        if let Ok(mut guard) = self.spoofed_name.lock() {
            *guard = name.chars().take(MAX_NAME_LENGTH).collect();
        }
    }
    pub fn update_friends(&self, spoofs: HashMap<String, String>) {
        if let Ok(mut guard) = self.spoofed_friends.lock() {
            *guard = spoofs;
        }
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

    fn start_http(&self) -> Result<(), String> {
        if self.listener_running() {
            if !crate::watchdog::is_ready() && !crate::watchdog::spawn() {
                return Err("HTTP proxy recovery watchdog stopped.".into());
            }
            self.http_active.store(true, Ordering::Release);
            return Ok(());
        }
        if !ca::is_current_installed(&self.base_dir) {
            return Err(
                "Install the Hebnix certificate in Settings > System to enable HTTP proxying."
                    .into(),
            );
        }
        let ca = Arc::new(ca::ensure(&self.base_dir)?);
        let mut crl = self.crl.lock().map_err(|_| "CRL lock poisoned")?;
        if crl.is_none() {
            *crl = Some(crl::CrlServer::start(ca.crl_der()?, self.tx.clone())?);
        }
        drop(crl);
        let rules: Vec<Box<dyn Rule>> = vec![
            Box::new(NameRule::new(Arc::clone(&self.spoofed_name))),
            Box::new(rules::FriendsRule::new(
                Arc::clone(&self.spoofed_friends),
                Arc::clone(&self.discovered_friends),
            )),
        ];
        let rules = Arc::new(
            rules
                .into_iter()
                .map(|inner| {
                    Box::new(rules::EnabledRule {
                        inner,
                        http: Arc::clone(&self.http_active),
                        socket: Arc::new(AtomicBool::new(false)),
                        game: Arc::clone(&self.game_present),
                    }) as Box<dyn Rule>
                })
                .collect(),
        );
        // A prefix that still points at an earlier listener (a running game,
        // or a restart while it runs) must keep that port when it is free.
        let mut ports = Vec::new();
        if let Some(prefix) = restore_prefix_proxy_pending(&self.base_dir) {
            ports.extend(wine_proxy::applied_port(&prefix, PROXY_HOST).filter(|&p| p != 0));
        }
        ports.extend_from_slice(PROXY_PORTS);
        let proxy = HttpProxy::start(ca, rules, self.tx.clone(), &ports)?;
        self.http_active.store(true, Ordering::Release);
        *self
            .http_proxy
            .lock()
            .map_err(|_| "HTTP proxy lock poisoned")? = Some(proxy);
        let _ = self.tx.send(AppMsg::Log(format!(
            "[Spoofer] HTTP proxy started on {PROXY_HOST}:{port}. WebSocket proxy is unavailable.",
            port = self.listener_port().unwrap_or_default()
        )));
        Ok(())
    }

    fn listener_port(&self) -> Option<u16> {
        self.http_proxy
            .lock()
            .ok()?
            .as_ref()
            .filter(|proxy| proxy.is_running())
            .map(HttpProxy::port)
    }

    fn listener_running(&self) -> bool {
        self.http_proxy
            .lock()
            .is_ok_and(|proxy| proxy.as_ref().is_some_and(HttpProxy::is_running))
    }

    /// point Rocket League's prefix at the proxy. Ok(false) while the game
    /// already runs without it: user.reg can only change while the prefix is
    /// idle, so it takes effect with the next launch.
    fn apply_prefix_proxy(&self) -> Result<bool, String> {
        let port = self.listener_port().ok_or("HTTP proxy is not running.")?;
        let server = our_proxy_server(port);
        let backup = marker_path(&self.base_dir);
        if let Some(prefix) = restore_prefix_proxy_pending(&self.base_dir) {
            if wine_proxy::applied_port(&prefix, PROXY_HOST) == Some(port) {
                return Ok(true);
            }
            // set by an earlier listener on another port: repoint it while
            // the prefix is idle, keeping the original backup
            if hebnix_sdk::process::prefix_in_use(&prefix) {
                return Err(format!(
                    "Rocket League still uses the HTTP proxy on another port. Restart Rocket League to use {PROXY_HOST}:{port}."
                ));
            }
            wine_proxy::apply(&prefix, &server)?;
            return Ok(true);
        }
        let prefix = hebnix_sdk::process::rocket_league_prefix().ok_or(
            "Could not find Rocket League's Wine prefix. Pick its Documents folder in Settings > Directories.",
        )?;
        if hebnix_sdk::process::prefix_in_use(&prefix) {
            return Ok(false);
        }
        let saved = serde_json::to_vec(&wine_proxy::read_backup(&prefix)?)
            .map_err(|e| e.to_string())?;
        std::fs::write(&backup, saved)
            .map_err(|error| format!("Could not save the Wine proxy settings: {error}"))?;
        // Arm crash recovery before changing the prefix's proxy.
        if !crate::watchdog::spawn() {
            let _ = std::fs::remove_file(&backup);
            return Err("Could not start HTTP proxy recovery watchdog.".into());
        }
        if let Err(error) = wine_proxy::apply(&prefix, &server) {
            let _ = std::fs::remove_file(&backup);
            return Err(error);
        }
        let _ = self.tx.send(AppMsg::Log(format!(
            "[Spoofer] HTTP proxy set in the Wine prefix {}",
            prefix.display()
        )));
        Ok(true)
    }

    /// stop spoofing. The listener keeps forwarding while a running game
    /// still has the prefix proxy (it would lose its connection otherwise);
    /// it stops once the setting could be restored.
    fn stop_http(&self) {
        self.http_active.store(false, Ordering::Release);
        if restore_prefix_proxy(&self.base_dir) == Restore::PrefixInUse {
            return;
        }
        if let Ok(mut proxy) = self.http_proxy.lock() {
            if let Some(proxy) = proxy.take() {
                proxy.stop();
            }
        }
        if let Ok(mut crl) = self.crl.lock() {
            if let Some(crl) = crl.take() {
                crl.stop();
            }
        }
    }

    fn sync_proxy_options(&self) -> Result<(), String> {
        let _guard = self.runtime_lock.lock().unwrap_or_else(|e| e.into_inner());
        // Windows only runs the proxy while the game runs, since WinINet
        // picks up a changed system proxy live. The prefix proxy has to be in
        // user.reg before the game starts, so it stays on whenever requested.
        let options = ProxyOptions {
            account: self.requested_http.load(Ordering::Acquire),
            ..Default::default()
        }
        .for_runtime(
            true,
            self.closing.load(Ordering::Acquire),
            self.certificate_blocked.load(Ordering::Acquire),
        );
        if !options.account {
            self.stop_http();
            return Ok(());
        }
        if let Err(error) = self.start_http() {
            self.stop_http();
            return Err(error);
        }
        match self.apply_prefix_proxy() {
            Ok(true) => Ok(()),
            Ok(false) => Err(WAITING_FOR_PREFIX.into()),
            Err(error) => {
                self.stop_http();
                Err(error)
            }
        }
    }
    pub fn disable_spoofing(&self) {
        self.closing.store(true, Ordering::Release);
        self.shutdown();
    }
    pub fn shutdown(&self) {
        let _guard = self.runtime_lock.lock().unwrap_or_else(|e| e.into_inner());
        self.requested_http.store(false, Ordering::Release);
        self.disable_rlapi();
        self.stop_http();
    }
    pub fn block_for_certificate_removal(&self) {
        self.certificate_blocked.store(true, Ordering::Release);
        self.shutdown();
    }
    pub fn certificate_setup_ready(&self) {
        self.certificate_blocked.store(false, Ordering::Release);
    }
    pub fn start_lifecycle_monitor(self: &Arc<Self>) -> Result<(), String> {
        if self.lifecycle_started.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let manager = Arc::downgrade(self);
        let result = std::thread::Builder::new()
            .name("proxy-lifecycle".into())
            .spawn(move || {
                let mut last_error = None;
                loop {
                    let Some(manager) = manager.upgrade() else {
                        break;
                    };
                    manager.game_present.store(
                        hebnix_sdk::process::is_rocket_league_running(),
                        Ordering::Release,
                    );
                    if manager.requested_http.load(Ordering::Acquire)
                        && !manager.certificate_blocked.load(Ordering::Acquire)
                        && !ca::is_current_installed(&manager.base_dir)
                    {
                        manager.block_for_certificate_removal();
                        let _ = manager.tx.send(AppMsg::SpooferCertificateLost);
                    }
                    match manager.sync_proxy_options() {
                        Err(error) => {
                            if last_error.as_ref() != Some(&error) {
                                let _ = manager.tx.send(AppMsg::Log(format!("[Spoofer] {error}")));
                            }
                            last_error = Some(error);
                        }
                        Ok(()) => last_error = None,
                    }
                    drop(manager);
                    std::thread::sleep(std::time::Duration::from_millis(250));
                }
            });
        if let Err(error) = result {
            self.lifecycle_started.store(false, Ordering::Release);
            return Err(format!("Could not start HTTP proxy monitor: {error}"));
        }
        Ok(())
    }
}

#[cfg(test)]
mod proxy_tests {
    use super::ProxyOptions;
    #[test]
    fn declined_elevation_disables_http_features_but_preserves_customisation() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH).unwrap().as_nanos();
        let directory = std::env::temp_dir().join(format!("hebnix-uac-{}-{unique}", std::process::id()));
        std::fs::create_dir(&directory).unwrap();
        let settings = serde_json::json!({
            "spoofer_master": false,
            "spoofer_username_enabled": true,
            "spoofer_username": "名前🚀",
            "spoofer_username_history": ["名前🚀", "Previous"],
            "spoofer_rank_enabled": true,
            "spoofer_titles": [{"text": "Local title"}],
        });
        let friends = serde_json::json!({
            "spoofer_friends_enabled": true,
            "friends": {"id": {"enabled": true, "spoofed_name": "Friend"}},
        });
        std::fs::write(directory.join("spoofer_settings.json"), serde_json::to_vec(&settings).unwrap()).unwrap();
        std::fs::write(directory.join("friends.json"), serde_json::to_vec(&friends).unwrap()).unwrap();
        super::disable_saved_http_features(&directory).unwrap();
        let saved: serde_json::Value = serde_json::from_slice(&std::fs::read(directory.join("spoofer_settings.json")).unwrap()).unwrap();
        let saved_friends: serde_json::Value = serde_json::from_slice(&std::fs::read(directory.join("friends.json")).unwrap()).unwrap();
        assert_eq!(saved["spoofer_username_enabled"], false);
        assert_eq!(saved["spoofer_master"], false);
        assert_eq!(saved_friends["spoofer_friends_enabled"], false);
        for key in ["spoofer_username", "spoofer_username_history", "spoofer_rank_enabled", "spoofer_titles"] {
            assert_eq!(saved[key], settings[key]);
        }
        assert_eq!(saved_friends["friends"], friends["friends"]);
        // The fallback is safe to repeat, including on an empty installation.
        super::disable_saved_http_features(&directory).unwrap();
        std::fs::remove_file(directory.join("spoofer_settings.json")).unwrap();
        std::fs::remove_file(directory.join("friends.json")).unwrap();
        super::disable_saved_http_features(&directory).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }
    #[test]
    fn websocket_consumers_cannot_start_any_proxy() {
        for bits in 0..8 {
            let requested = ProxyOptions {
                account: bits & 1 != 0,
                psynet: bits & 2 != 0,
                replays: bits & 4 != 0,
            };
            let options = requested.http_only();
            assert_eq!(options.account, requested.account);
            assert!(!options.psynet && !options.replays);
            for state in 0..8 {
                let effective =
                    requested.for_runtime(state & 1 != 0, state & 2 != 0, state & 4 != 0);
                assert_eq!(effective.account, requested.account && state == 1);
                assert!(!effective.psynet && !effective.replays);
            }
        }
    }
}
