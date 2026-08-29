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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use crossbeam_channel::Sender;

use crate::messages::AppMsg;
use crate::spoofer::rules::{
    NameRule, OwnedProductsRule, Rule, TITLE_HOST, TitleRule, TitleSettings,
};
use crate::spoofer::skill_bridge::SkillBridge;
use crate::spoofer::socket::SocketProxy;

pub const PROXY_HOST: &str = "127.0.0.1";
pub const PROXY_PORT: u16 = 8080;
pub const MAX_NAME_LENGTH: usize = 32;
const REDIRECT_HOSTS: [&str; 3] = ["api.epicgames.dev", "api.rlpp.psynet.gg", TITLE_HOST];

pub fn is_admin() -> bool {
    nix::unistd::geteuid().is_root()
}

pub const SKIP_ELEVATE_ARG: &str = "--no-elevate";

/// relaunch the whole process elevated via pkexec (the Linux/PolicyKit
/// equivalent of a Windows UAC prompt) instead of asking for admin only for
/// the hosts-file write; the whole app was already designed around
/// "the process itself runs elevated" so this mirrors that shape.
///
/// Returns true once the relaunch is *spawned* (matching the windows
/// version's semantics) -- if the user declines the polkit auth dialog,
/// pkexec just exits non-zero and the (still non-root) caller falls through
/// to whatever `--no-elevate` handling `main()` already has.
pub fn spawn_elevated_relaunch() -> bool {
    let Ok(exe) = std::env::current_exe() else {
        return false;
    };
    let args: Vec<String> = std::env::args().skip(1).collect();
    std::process::Command::new("pkexec")
        .arg(exe)
        .args(args)
        .arg(SKIP_ELEVATE_ARG)
        .spawn()
        .is_ok()
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
        let _ = hosts::clear();
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
    http_active: AtomicBool,
    socket_active: AtomicBool,
    title_settings: Arc<Mutex<TitleSettings>>,
    skill_bridge: Mutex<Option<SkillBridge>>,
    crl: Mutex<Option<crl::CrlServer>>,
}

impl SpooferManager {
    fn ensure_crl(&self, ca: &ca::Ca) {
        let mut slot = match self.crl.lock() {
            Ok(s) => s,
            Err(_) => return,
        };
        if slot.is_some() {
            return;
        }
        let der = match ca.crl_der() {
            Ok(d) => d,
            Err(e) => {
                let _ = self.tx.send(AppMsg::Log(format!("[Spoofer] crl gen: {e}")));
                return;
            }
        };
        match crl::CrlServer::start(der, self.tx.clone()) {
            Ok(s) => *slot = Some(s),
            Err(e) => {
                let _ = self
                    .tx
                    .send(AppMsg::Log(format!("[Spoofer] crl server: {e}")));
            }
        }
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
        Self {
            base_dir,
            tx,
            spoofed_name: Arc::new(Mutex::new(String::new())),
            spoofed_friends: Arc::new(Mutex::new(HashMap::new())),
            discovered_friends: Arc::new(Mutex::new(HashMap::new())),
            spoofed_ranks: Arc::new(Mutex::new(HashMap::new())),
            owned_products: Arc::new(Mutex::new(owned_products)),
            reverse_proxy: Mutex::new(None),
            http_active: AtomicBool::new(false),
            socket_active: AtomicBool::new(false),
            title_settings: Arc::new(Mutex::new(TitleSettings::default())),
            skill_bridge: Mutex::new(None),
            crl: Mutex::new(None),
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
        if self
            .spoofed_ranks
            .lock()
            .map(|ranks| !ranks.is_empty())
            .unwrap_or(false)
        {
            if let Err(error) = self.start_skill_bridge() {
                let detail = format!("[Spoofer] Rank bridge failed to start: {error}");
                let _ = std::fs::write(self.base_dir.join("rank_spoofer_status.log"), &detail);
                let _ = self.tx.send(AppMsg::Log(detail));
            }
        } else {
            self.stop_skill_bridge();
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
                self.tx.clone(),
                self.base_dir.join("rank_spoofer_frames.log"),
            )?);
        }
        Ok(())
    }

    fn stop_skill_bridge(&self) {
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
                .map(|proxy| proxy.is_some())
                .unwrap_or(false)
    }

    pub fn socket_running(&self) -> bool {
        self.socket_active.load(Ordering::Relaxed)
            && self
                .reverse_proxy
                .lock()
                .map(|proxy| proxy.is_some())
                .unwrap_or(false)
    }

    pub fn start_http(&self) -> Result<(), String> {
        if self.http_active.load(Ordering::Relaxed) {
            return Ok(());
        }
        self.http_active.store(true, Ordering::Relaxed);
        if let Err(error) = self.ensure_reverse_proxy() {
            self.http_active.store(false, Ordering::Relaxed);
            return Err(error);
        }
        Ok(())
    }

    pub fn stop_http(&self) {
        self.http_active.store(false, Ordering::Relaxed);
        self.stop_reverse_if_unused();
        self.maybe_stop_crl();
    }

    pub fn set_title(&self, text: &str) {
        if let Ok(mut settings) = self.title_settings.lock() {
            settings.text = text.chars().take(64).collect();
        }
    }

    pub fn set_title_enabled(&self, enabled: bool) {
        if let Ok(mut settings) = self.title_settings.lock() {
            settings.enabled = enabled;
        }
    }

    pub fn set_title_options(&self, color: String, glow: bool, target_id: Option<String>) {
        if let Ok(mut settings) = self.title_settings.lock() {
            settings.color = color;
            settings.glow = glow;
            settings.target_id = target_id;
        }
    }

    pub fn start_socket(&self) -> Result<(), String> {
        if self.socket_active.load(Ordering::Relaxed) {
            return Ok(());
        }
        self.socket_active.store(true, Ordering::Relaxed);
        if let Err(error) = self.ensure_reverse_proxy() {
            self.socket_active.store(false, Ordering::Relaxed);
            return Err(error);
        }
        Ok(())
    }

    pub fn stop_socket(&self) {
        self.socket_active.store(false, Ordering::Relaxed);
        self.stop_reverse_if_unused();
        self.stop_skill_bridge();
        self.maybe_stop_crl();
    }

    fn ensure_reverse_proxy(&self) -> Result<(), String> {
        let mut slot = self
            .reverse_proxy
            .lock()
            .map_err(|_| "reverse proxy lock poisoned")?;
        if slot.is_some() {
            return Ok(());
        }
        let ca = Arc::new(ca::ensure(&self.base_dir)?);
        if !ca::is_current_installed(&self.base_dir) {
            return Err(
                "Certificate not installed. open Spoofer settings and click Install Certificate"
                    .into(),
            );
        }
        if !hosts::is_writable() {
            return Err("The hosts file needs root, restart Hebnix elevated".into());
        }
        let mut real_ips = HashMap::new();
        for host in REDIRECT_HOSTS {
            real_ips.insert(host.to_string(), dns::resolve_a(host)?);
        }
        let rules: Arc<Vec<Box<dyn Rule>>> = Arc::new(vec![
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
        ]);
        self.ensure_crl(&ca);
        let proxy = SocketProxy::start(ca, rules, self.tx.clone(), real_ips)?;
        if let Err(error) = hosts::set_redirects(&REDIRECT_HOSTS) {
            proxy.stop();
            return Err(error);
        }
        *slot = Some(proxy);
        Ok(())
    }

    fn stop_reverse_if_unused(&self) {
        if self.http_active.load(Ordering::Relaxed) || self.socket_active.load(Ordering::Relaxed) {
            return;
        }
        let _ = hosts::clear();
        if let Ok(mut slot) = self.reverse_proxy.lock() {
            if let Some(proxy) = slot.take() {
                proxy.stop();
            }
        }
    }

    /// Stops only runtime interception. It deliberately does not modify saved
    /// spoof settings, so the user's enabled toggles survive the next launch.
    pub fn shutdown(&self) {
        self.stop_socket();
        self.stop_http();
        // Clear a redirect even if the socket failed to start or its state was lost.
        let _ = hosts::clear();
        hosts::flush_dns();
    }
}
