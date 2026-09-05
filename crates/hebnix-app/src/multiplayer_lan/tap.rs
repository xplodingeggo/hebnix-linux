//! virtual Ethernet adapter for Workshop LAN multiplayer, Linux-native.
//!
//! Windows drives a bundled OpenVPN TAP driver via raw DeviceIoControl calls.
//! Linux has kernel-native TUN/TAP support (no driver install needed at all):
//! open `/dev/net/tun`, `ioctl(TUNSETIFF)` to attach a persistent `hebnix0`
//! device, then read/write raw Ethernet II frames directly on that fd.
//! Requires CAP_NET_ADMIN (granted via `setcap` on the binary at install
//! time - see packaging/) or root.

use std::fs::{File, OpenOptions};
use std::io::{Read, Write};
use std::os::fd::{AsRawFd, FromRawFd, RawFd};
use std::process::Command;

pub const ADAPTER_NAME: &str = "hebnix0";

/// `setcap cap_net_admin+eip` on the binary only grants CAP_NET_ADMIN to
/// *this* process - it does not propagate to child processes spawned via
/// fork+exec (e.g. `ip`, `nft`), which normally start with a clean
/// capability set regardless of what the parent has.
///
/// Promotes CAP_NET_ADMIN into our own Inheritable set (a process can always
/// add one of its own Permitted caps to its own Inheritable set - self-
/// modification, no extra privilege needed) - a prerequisite for
/// `command_with_net_admin` below, which raises the capability into each
/// `ip`/`nft` child's own Ambient set individually rather than our own.
///
/// Deliberately does NOT raise our own process's Ambient set globally (an
/// earlier version of this did, and it broke every Heroic-launched game:
/// Ambient is inherited by *every* descendant indefinitely, and bubblewrap
/// - used by Steam Linux Runtime's sandboxing, several forks down from
/// Heroic - treats an unexpected inherited capability as a sandbox-escape
/// red flag and refuses to run at all ("Unexpected capabilities but not
/// setuid, old file caps config?", verified live). Call this once at
/// startup.
pub fn raise_net_admin_ambient() {
    use caps::{CapSet, Capability};
    // best-effort: silently no-ops if CAP_NET_ADMIN isn't in this process's
    // Permitted set at all yet (e.g. a plain `cargo run` dev binary with no
    // setcap applied), or if running as root (which needs no capability
    // dance in the first place).
    let _ = caps::raise(None, CapSet::Inheritable, Capability::CAP_NET_ADMIN);
}

/// builds a `Command` for `program` that raises CAP_NET_ADMIN into *this one
/// child's* own Ambient set (via a pre_exec hook, running after fork but
/// before exec) rather than our own process's - see raise_net_admin_ambient
/// above for why that distinction matters. Requires raise_net_admin_ambient
/// to have already promoted CAP_NET_ADMIN into our own Inheritable set,
/// since fork() duplicates the parent's full capability state (Permitted,
/// Inheritable, ...) into the child before this hook runs, and the Ambient
/// raise itself needs the capability in both Permitted and Inheritable.
pub(super) fn command_with_net_admin(program: &str) -> Command {
    use std::os::unix::process::CommandExt;
    let mut cmd = Command::new(program);
    unsafe {
        cmd.pre_exec(|| {
            const PR_CAP_AMBIENT: libc::c_int = 47;
            const PR_CAP_AMBIENT_RAISE: libc::c_ulong = 2;
            const CAP_NET_ADMIN: libc::c_ulong = 12;
            libc::prctl(PR_CAP_AMBIENT, PR_CAP_AMBIENT_RAISE, CAP_NET_ADMIN, 0, 0);
            Ok(())
        });
    }
    cmd
}

/// Windows gates Workshop LAN on running elevated, since installing its TAP
/// driver needs administrator rights. Linux doesn't need root at all here -
/// `setcap cap_net_admin+eip` on the binary (done once at install time, see
/// packaging/) is enough for both the TAP device and the nftables rules.
/// Checks the process's effective capability set directly rather than
/// reusing the (root-only) admin check the spoofer feature uses.
pub fn has_net_admin_capability() -> bool {
    // root implicitly has every capability, including a plain `cargo run`
    // during development where setcap was never applied to the debug binary
    if nix::unistd::geteuid().is_root() {
        return true;
    }
    let Ok(status) = std::fs::read_to_string("/proc/self/status") else {
        return false;
    };
    let Some(line) = status.lines().find(|line| line.starts_with("CapEff:")) else {
        return false;
    };
    let Some(hex) = line.split_whitespace().nth(1) else {
        return false;
    };
    let Ok(mask) = u64::from_str_radix(hex, 16) else {
        return false;
    };
    // CAP_NET_ADMIN = 12, per linux/capability.h
    mask & (1 << 12) != 0
}
const TUN_PATH: &str = "/dev/net/tun";

// linux/if_tun.h - _IOW('T', 202/203, c_int)
const TUNSETIFF: u64 = 0x4004_54ca;
const TUNSETPERSIST: u64 = 0x4004_54cb;
const IFF_TAP: i16 = 0x0002;
const IFF_NO_PI: i16 = 0x1000;

// sizeof(struct ifreq) on Linux is 40 bytes: a 16-byte ifr_name followed by
// a union whose largest member (struct ifmap) is 24 bytes once padded to
// 8-byte alignment. We only ever populate ifr_name + the ifr_flags short
// that overlaps the start of that union; the rest stays zeroed.
#[repr(C)]
struct IfReq {
    name: [u8; 16],
    flags: i16,
    _pad: [u8; 22],
}

impl IfReq {
    fn new(name: &str, flags: i16) -> Self {
        let mut req = IfReq {
            name: [0; 16],
            flags,
            _pad: [0; 22],
        };
        let bytes = name.as_bytes();
        let len = bytes.len().min(15);
        req.name[..len].copy_from_slice(&bytes[..len]);
        req
    }
}

fn open_tun_fd(persist: bool) -> Result<RawFd, String> {
    let file = OpenOptions::new()
        .read(true)
        .write(true)
        .open(TUN_PATH)
        .map_err(|e| format!("could not open {TUN_PATH}: {e} (needs CAP_NET_ADMIN)"))?;
    let fd = file.as_raw_fd();
    let mut req = IfReq::new(ADAPTER_NAME, IFF_TAP | IFF_NO_PI);
    let ret = unsafe { libc::ioctl(fd, TUNSETIFF, &mut req as *mut IfReq) };
    if ret < 0 {
        return Err(format!(
            "TUNSETIFF failed: {}",
            std::io::Error::last_os_error()
        ));
    }
    if persist {
        let ret = unsafe { libc::ioctl(fd, TUNSETPERSIST, 1i32) };
        if ret < 0 {
            return Err(format!(
                "TUNSETPERSIST failed: {}",
                std::io::Error::last_os_error()
            ));
        }
    }
    // keep the fd alive past `file`'s drop
    std::mem::forget(file);
    Ok(fd)
}

pub struct TapSession {
    file: File,
}

impl std::fmt::Debug for TapSession {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("TapSession").finish_non_exhaustive()
    }
}

unsafe impl Send for TapSession {}
unsafe impl Sync for TapSession {}

impl TapSession {
    /// opens the already-installed persistent `hebnix0` device (see
    /// ensure_adapter). Non-blocking so try_receive() can poll it from the
    /// same packet-pump loop that also polls the UDP tunnel socket.
    pub fn open() -> Result<Self, String> {
        let fd = open_tun_fd(false)?;
        let flags = unsafe { libc::fcntl(fd, libc::F_GETFL, 0) };
        if flags >= 0 {
            unsafe {
                libc::fcntl(fd, libc::F_SETFL, flags | libc::O_NONBLOCK);
            }
        }
        Ok(Self {
            file: unsafe { File::from_raw_fd(fd) },
        })
    }

    pub fn try_receive(&self) -> Option<Vec<u8>> {
        let mut buf = vec![0u8; 2048];
        match (&self.file).read(&mut buf) {
            Ok(n) if n > 0 => {
                buf.truncate(n);
                Some(buf)
            }
            _ => None,
        }
    }

    pub fn send(&self, frame: &[u8]) -> Result<(), String> {
        (&self.file).write_all(frame).map_err(|e| e.to_string())
    }
}

pub fn ensure_adapter(address: &str) -> Result<(), String> {
    if !adapter_exists() {
        let fd = open_tun_fd(true)?;
        unsafe {
            libc::close(fd);
        }
    }
    configure(address)
}

pub fn configure_existing(address: &str) -> Result<(), String> {
    if !adapter_exists() {
        return Err(format!("{ADAPTER_NAME} is not set up; use the optional setup button first"));
    }
    if adapter_has_address(address) {
        return Ok(());
    }
    configure(address)
}

pub fn is_configured(address: &str) -> Result<bool, String> {
    Ok(adapter_exists() && adapter_has_address(address))
}

pub fn clear_configuration() -> Result<(), String> {
    if !adapter_exists() {
        return Ok(());
    }
    let _ = run("ip", &["addr", "flush", "dev", ADAPTER_NAME]);
    Ok(())
}

pub fn mac_address() -> Result<String, String> {
    let path = format!("/sys/class/net/{ADAPTER_NAME}/address");
    let mac = std::fs::read_to_string(&path)
        .map_err(|e| format!("could not read {ADAPTER_NAME}'s MAC address: {e}"))?;
    let mac = mac.trim().replace(':', "");
    if mac.len() == 12 && mac.chars().all(|c| c.is_ascii_hexdigit()) {
        Ok(mac)
    } else {
        Err(format!("could not read {ADAPTER_NAME}'s MAC address"))
    }
}

pub fn add_neighbor(address: &str, mac: &str) -> Result<(), String> {
    let formatted = format_mac(mac).ok_or_else(|| "invalid TAP MAC address".to_string())?;
    run(
        "ip",
        &[
            "neigh", "replace", address, "lladdr", &formatted, "dev", ADAPTER_NAME, "nud",
            "permanent",
        ],
    )
}

pub fn arp_announcement(address: &str) -> Result<Vec<u8>, String> {
    let mac = parse_mac(&mac_address()?).ok_or_else(|| "invalid TAP MAC address".to_string())?;
    let ip = parse_ip(address)?;
    let mut frame = vec![0; 42];
    frame[..6].fill(0xff);
    frame[6..12].copy_from_slice(&mac);
    frame[12..14].copy_from_slice(&0x0806u16.to_be_bytes());
    frame[14..16].copy_from_slice(&1u16.to_be_bytes());
    frame[16..18].copy_from_slice(&0x0800u16.to_be_bytes());
    frame[18] = 6;
    frame[19] = 4;
    frame[20..22].copy_from_slice(&1u16.to_be_bytes());
    frame[22..28].copy_from_slice(&mac);
    frame[28..32].copy_from_slice(&ip);
    frame[38..42].copy_from_slice(&ip);
    Ok(frame)
}

pub fn arp_reply_for_local(frame: &[u8], local_address: &str) -> Result<Option<Vec<u8>>, String> {
    if frame.len() < 42
        || frame[12..14] != 0x0806u16.to_be_bytes()
        || frame[20..22] != 1u16.to_be_bytes()
    {
        return Ok(None);
    }
    let local_ip = parse_ip(local_address)?;
    if frame[38..42] != local_ip {
        return Ok(None);
    }
    let local_mac =
        parse_mac(&mac_address()?).ok_or_else(|| "invalid TAP MAC address".to_string())?;
    let mut reply = vec![0; 42];
    reply[..6].copy_from_slice(&frame[22..28]);
    reply[6..12].copy_from_slice(&local_mac);
    reply[12..14].copy_from_slice(&0x0806u16.to_be_bytes());
    reply[14..16].copy_from_slice(&1u16.to_be_bytes());
    reply[16..18].copy_from_slice(&0x0800u16.to_be_bytes());
    reply[18] = 6;
    reply[19] = 4;
    reply[20..22].copy_from_slice(&2u16.to_be_bytes());
    reply[22..28].copy_from_slice(&local_mac);
    reply[28..32].copy_from_slice(&local_ip);
    reply[32..38].copy_from_slice(&frame[22..28]);
    reply[38..42].copy_from_slice(&frame[28..32]);
    Ok(Some(reply))
}

fn configure(address: &str) -> Result<(), String> {
    // a single /24 on the interface is enough - unlike Windows' netsh, the
    // kernel derives the whole-subnet connected route automatically, no
    // per-peer /32 route needed.
    run("ip", &["addr", "flush", "dev", ADAPTER_NAME])?;
    run(
        "ip",
        &["addr", "add", &format!("{address}/24"), "dev", ADAPTER_NAME],
    )?;
    run("ip", &["link", "set", ADAPTER_NAME, "up"])
}

fn adapter_exists() -> bool {
    std::path::Path::new(&format!("/sys/class/net/{ADAPTER_NAME}")).is_dir()
}

fn adapter_has_address(address: &str) -> bool {
    let output = Command::new("ip")
        .args(["-4", "addr", "show", "dev", ADAPTER_NAME])
        .output();
    match output {
        Ok(o) => String::from_utf8_lossy(&o.stdout).contains(&format!("inet {address}/")),
        Err(_) => false,
    }
}

fn run(program: &str, args: &[&str]) -> Result<(), String> {
    let output = command_with_net_admin(program)
        .args(args)
        .output()
        .map_err(|e| e.to_string())?;
    if output.status.success() {
        return Ok(());
    }
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
    Err(format!("{program} {} failed: {stderr}", args.join(" ")))
}

fn parse_ip(address: &str) -> Result<[u8; 4], String> {
    address
        .parse::<std::net::Ipv4Addr>()
        .map(|v| v.octets())
        .map_err(|_| format!("invalid virtual IP address: {address}"))
}

fn parse_mac(value: &str) -> Option<[u8; 6]> {
    let hex: String = value.chars().filter(|c| c.is_ascii_hexdigit()).collect();
    if hex.len() != 12 {
        return None;
    }
    let mut mac = [0; 6];
    for (i, byte) in mac.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hex[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(mac)
}

fn format_mac(value: &str) -> Option<String> {
    let mac = parse_mac(value)?;
    Some(
        mac.iter()
            .map(|b| format!("{b:02x}"))
            .collect::<Vec<_>>()
            .join(":"),
    )
}
