//! Workshop LAN multiplayer (host/join over a virtual TAP adapter).
//!
// TODO(linux-port): multihome LAN not yet ported, needs TUN/TAP + nftables
// rewrite. On Windows this drives a Wintun adapter + `netsh advfirewall`
// rules to fake a LAN so Rocket League's local multiplayer/workshop-map
// browsing works between two machines. Doing the same on Linux needs a
// TUN/TAP device (via `/dev/net/tun`) plus nftables rules, which is a
// substantial rewrite out of scope for this port pass.
//
// What's kept: the pure-data request/response models (`models.rs`) and the
// portable HTTP room-matchmaking client (`room_api.rs`, just ureq calls to
// api.hebnix.com) -- both have zero Windows/OS dependency and are reused
// as-is. Everything that needs the virtual adapter (`TapSession`,
// `HostSession`, `GuestSession`, the `netsh` firewall rules) is stubbed
// below so the rest of the app (ui/workshop.rs, messages.rs) keeps
// compiling and the feature shows up in the UI as "not available" rather
// than silently vanishing.

mod models;
mod room_api;

use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use models::{
    CreateRoomRequest, JoinRoomRequest, JoinedRoom, LeaveRoomRequest, MapDescriptor, Room,
    RoomCredentials, UpdatePlayerRequest,
};
pub use room_api::RoomClient;

pub const VPN_SUBNET: &str = "192.10.192";
pub const HOST_ADDRESS: &str = "192.10.192.1";
pub const HOST_ADDRESS_BYTES: [u8; 4] = [192, 10, 192, 1];
pub const FIRST_GUEST_ADDRESS: &str = "192.10.192.2";
pub const GUEST_ADDRESS_RANGE: &str = "192.10.192.2-192.10.192.8";
pub const PACKET_PUMP_INTERVAL: Duration = Duration::from_millis(2);
pub const SESSION_HEARTBEAT_INTERVAL: Duration = Duration::from_secs(300);

const UNSUPPORTED: &str =
    "Workshop LAN multiplayer isn't available on Linux yet (needs a TUN/TAP + nftables port)";

pub fn guest_address(slot: u8) -> Result<String, String> {
    if (2..=8).contains(&slot) {
        Ok(format!("{VPN_SUBNET}.{slot}"))
    } else {
        Err("invalid Workshop LAN player slot".to_string())
    }
}

pub fn cleanup_system_state() -> Result<(), String> {
    Ok(())
}

// --- tap.rs stand-in: no virtual adapter on Linux yet ---

/// stand-in for the Wintun-backed session; always fails to open since there's
/// no TUN/TAP device wired up on Linux yet.
#[derive(Debug)]
pub struct TapSession;

impl TapSession {
    pub fn open() -> Result<Self, String> {
        Err(UNSUPPORTED.to_string())
    }

    #[allow(dead_code)]
    pub fn try_receive(&self) -> Option<Vec<u8>> {
        None
    }
}

pub fn configure_existing(_address: &str) -> Result<(), String> {
    Err(UNSUPPORTED.to_string())
}

pub fn ensure_adapter(_address: &str) -> Result<(), String> {
    Err(UNSUPPORTED.to_string())
}

pub fn is_configured(_address: &str) -> Result<bool, String> {
    Ok(false)
}

// --- firewall.rs stand-in: no netsh on Linux, and moot while TapSession
// can't open anyway. Real rewrite would shell out to nft/iptables. ---

pub fn ensure_host_rule(_executable: &std::path::Path, _port: u16) -> Result<(), String> {
    Err(UNSUPPORTED.to_string())
}

pub fn ensure_join_rule_if_needed(
    _executable: &std::path::Path,
    _host_ip: &str,
    _host_port: u16,
) -> Result<(), String> {
    Err(UNSUPPORTED.to_string())
}

pub fn ensure_rocket_league_lan_rule(
    _executable: &std::path::Path,
    _remote_ip: &str,
) -> Result<(), String> {
    Err(UNSUPPORTED.to_string())
}

// --- direct_udp.rs stand-in: stats shape kept so the UI code that reads
// `.stats` off a session compiles; sessions are never actually created
// (TapSession::open always errs first) so these values never move. ---

#[derive(Debug, Default)]
pub struct TunnelStats {
    pub connected: AtomicBool,
    pub sent: AtomicU64,
    pub received: AtomicU64,
    pub delivered: AtomicU64,
    pub last_sent_udp: Mutex<String>,
    pub last_received_udp: Mutex<String>,
    pub last_sent_broadcast_udp: Mutex<String>,
    pub last_received_broadcast_udp: Mutex<String>,
    pub last_sent_lan_udp: Mutex<String>,
    pub last_received_lan_udp: Mutex<String>,
}

// --- hosting.rs / guest.rs stand-ins ---

#[derive(Debug)]
pub struct HostSession {
    pub credentials: RoomCredentials,
    pub stats: Arc<TunnelStats>,
}

impl HostSession {
    /// always fails: reachable only if `TapSession::open` somehow succeeds
    /// (it doesn't, yet), kept for signature-compatibility with the UI code.
    pub fn start(
        _client: RoomClient,
        _request: CreateRoomRequest,
        _tunnel: TapSession,
    ) -> Result<Self, String> {
        Err(UNSUPPORTED.to_string())
    }

    pub fn stop(&mut self) -> Result<(), String> {
        Ok(())
    }
    pub fn suspend(&mut self) {}
}

#[derive(Debug)]
pub struct GuestSession {
    pub joined: JoinedRoom,
    pub stats: Arc<TunnelStats>,
}

impl GuestSession {
    pub fn start(
        _joined: JoinedRoom,
        _identity: JoinRoomRequest,
        _tunnel: TapSession,
    ) -> Result<Self, String> {
        Err(UNSUPPORTED.to_string())
    }

    pub fn stop(&mut self) {}
    pub fn leave(&mut self) {}
}
