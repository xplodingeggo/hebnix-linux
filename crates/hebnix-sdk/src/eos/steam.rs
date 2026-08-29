//! steam session ticket via libsteam_api.so (dlopen'd with `libloading`)
//!
// same flat-C-api flow as the windows version: load the lib, SteamAPI_Init,
// grab ISteamUser via the flat C api, then GetAuthSessionTicket for RL's
// appid (252950). hex ticket gets POSTed to epic oauth later.
//
// lib loaded + inited once per process and kept alive (never SteamAPI_Shutdown),
// re-initing churns steam and can trip ticket cooldowns. handles kept as ints so
// the cached state is Send+Sync, fn pointers rebuilt each call.

use std::ffi::c_void;
use std::sync::Mutex;

use libloading::Library;

/// RL's steam appid
pub const RL_STEAM_APPID: u32 = 252950;

type FnInit = unsafe extern "C" fn() -> bool;
type FnSteamClient = unsafe extern "C" fn() -> *mut c_void;
type FnGetHSteamUser = unsafe extern "C" fn() -> i32;
type FnGetHSteamPipe = unsafe extern "C" fn() -> i32;
type FnGetISteamUser = unsafe extern "C" fn(*mut c_void, i32, i32, *const u8) -> *mut c_void;
type FnGetAuthSessionTicket =
    unsafe extern "C" fn(*mut c_void, *mut u8, i32, *mut u32, *mut c_void) -> u32;
type FnGetSteamID = unsafe extern "C" fn(*mut c_void) -> u64;

// cached process-wide steam state. pointers held as ints so it's Send/Sync,
// lib + objects live for the whole process. the Library is leaked into a
// Box and never dropped (matches "never SteamAPI_Shutdown").
struct SteamState {
    steam_user: usize,
    p_get_ticket: usize,
    p_get_steam_id: usize,
    steam_id: u64,
}

unsafe impl Send for SteamState {}

static STATE: Mutex<Option<SteamState>> = Mutex::new(None);
static LIB: Mutex<Option<&'static Library>> = Mutex::new(None);

/// find libsteam_api.so. order: HEBNIX_STEAM_API_SO env (full path), next to
/// the exe / `_libs/` next to the exe, then wherever the Steam client itself
/// lives (`~/.steam/steam/linux64` etc), then a running RL install dir.
fn locate_so() -> Option<std::path::PathBuf> {
    if let Ok(p) = std::env::var("HEBNIX_STEAM_API_SO") {
        let path = std::path::PathBuf::from(p);
        if path.is_file() {
            return Some(path);
        }
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(exe_dir) = exe.parent() {
            for cand in [
                exe_dir.join("libsteam_api.so"),
                exe_dir.join("_libs").join("libsteam_api.so"),
            ] {
                if cand.is_file() {
                    return Some(cand);
                }
            }
        }
    }

    let home = dirs::home_dir().unwrap_or_else(|| std::path::PathBuf::from("."));
    let candidate_dirs = [
        home.join(".steam/steam/linux64"),
        home.join(".steam/steam"),
        home.join(".local/share/Steam/linux64"),
        home.join(".local/share/Steam"),
        home.join(".steam/root/linux64"),
        home.join(
            ".local/share/Steam/steamapps/common/rocketleague/Binaries/Linux64",
        ),
        home.join(
            ".local/share/Steam/steamapps/common/rocketleague",
        ),
    ];
    for dir in candidate_dirs {
        let cand = dir.join("libsteam_api.so");
        if cand.is_file() {
            return Some(cand);
        }
    }
    None
}

// init steam once, cache the ISteamUser handle + accessors
fn ensure_init(state: &mut Option<SteamState>) -> Result<(), String> {
    if state.is_some() {
        return Ok(());
    }

    let so = locate_so().ok_or_else(|| {
        "libsteam_api.so not found (set HEBNIX_STEAM_API_SO, or make sure Steam is installed \
         under ~/.steam or ~/.local/share/Steam)"
            .to_string()
    })?;

    // SteamAPI_Init reads the appid from the SteamAppId env var (checked
    // before steam_appid.txt).
    unsafe {
        std::env::set_var("SteamAppId", RL_STEAM_APPID.to_string());
        std::env::set_var("SteamGameId", RL_STEAM_APPID.to_string());
    }

    unsafe {
        let lib = Library::new(&so)
            .map_err(|e| format!("dlopen({}) failed: {e}", so.display()))?;
        // leak so the fn pointers stay valid for the process lifetime
        let lib: &'static Library = Box::leak(Box::new(lib));
        *LIB.lock().unwrap() = Some(lib);

        let p_init: libloading::Symbol<FnInit> = lib
            .get(b"SteamAPI_Init\0")
            .map_err(|_| "missing export SteamAPI_Init".to_string())?;
        let p_client: libloading::Symbol<FnSteamClient> = lib
            .get(b"SteamClient\0")
            .map_err(|_| "missing export SteamClient".to_string())?;
        let p_get_user: libloading::Symbol<FnGetISteamUser> = lib
            .get(b"SteamAPI_ISteamClient_GetISteamUser\0")
            .map_err(|_| "missing export SteamAPI_ISteamClient_GetISteamUser".to_string())?;
        let p_get_ticket: libloading::Symbol<FnGetAuthSessionTicket> = lib
            .get(b"SteamAPI_ISteamUser_GetAuthSessionTicket\0")
            .map_err(|_| "missing export SteamAPI_ISteamUser_GetAuthSessionTicket".to_string())?;
        let p_get_ticket_addr = *p_get_ticket as usize;

        let p_get_steam_id_addr: usize = lib
            .get::<FnGetSteamID>(b"SteamAPI_ISteamUser_GetSteamID\0")
            .map(|s| *s as usize)
            .unwrap_or(0);

        if !p_init() {
            return Err(
                "SteamAPI_Init failed, is Steam running with an RL-owning account?".to_string(),
            );
        }

        let p_steam_client = p_client();
        if p_steam_client.is_null() {
            return Err("SteamClient() returned null".to_string());
        }

        let h_user = lib
            .get::<FnGetHSteamUser>(b"SteamAPI_GetHSteamUser\0")
            .map(|f| f())
            .unwrap_or(1);
        let h_pipe = lib
            .get::<FnGetHSteamPipe>(b"SteamAPI_GetHSteamPipe\0")
            .map(|f| f())
            .unwrap_or(1);

        let mut steam_user = std::ptr::null_mut();
        for ver in [
            b"SteamUser021\0".as_slice(),
            b"SteamUser020\0",
            b"SteamUser019\0",
            b"SteamUser018\0",
            b"SteamUser017\0",
            b"SteamUser016\0",
        ] {
            steam_user = p_get_user(p_steam_client, h_user, h_pipe, ver.as_ptr());
            if !steam_user.is_null() {
                break;
            }
        }
        if steam_user.is_null() {
            return Err("could not obtain ISteamUser (no matching interface version)".to_string());
        }

        let steam_id = if p_get_steam_id_addr != 0 {
            let get_id: FnGetSteamID = std::mem::transmute(p_get_steam_id_addr);
            get_id(steam_user)
        } else {
            0
        };

        tracing::info!(steam_id, "eos/steam: Steam API initialised");
        *state = Some(SteamState {
            steam_user: steam_user as usize,
            p_get_ticket: p_get_ticket_addr,
            p_get_steam_id: p_get_steam_id_addr,
            steam_id,
        });
    }
    Ok(())
}

/// fresh steam auth session ticket (uppercase hex) + the steamid
// GetAuthSessionTicket has a short cooldown so we retry with 1s/2s backoff like
// the python impl did.
pub fn get_ticket() -> Result<(String, String), String> {
    let mut guard = STATE.lock().unwrap();
    ensure_init(&mut guard)?;
    let st = guard.as_mut().expect("state initialised above");

    if st.steam_id == 0 && st.p_get_steam_id != 0 {
        unsafe {
            let get_id: FnGetSteamID = std::mem::transmute(st.p_get_steam_id);
            st.steam_id = get_id(st.steam_user as *mut c_void);
        }
    }
    let steam_id = st.steam_id.to_string();

    let mut buf = [0u8; 4096];
    unsafe {
        let get_ticket: FnGetAuthSessionTicket = std::mem::transmute(st.p_get_ticket);
        for attempt in 0..3 {
            let mut ticket_size: u32 = 0;
            let handle = get_ticket(
                st.steam_user as *mut c_void,
                buf.as_mut_ptr(),
                buf.len() as i32,
                &mut ticket_size,
                std::ptr::null_mut(),
            );
            if handle != 0 && ticket_size > 0 {
                let hex = hex_upper(&buf[..ticket_size as usize]);
                return Ok((hex, steam_id));
            }
            if attempt < 2 {
                std::thread::sleep(std::time::Duration::from_secs(attempt + 1));
            }
        }
    }
    Err("GetAuthSessionTicket returned no ticket (cooldown or not logged in)".to_string())
}

fn hex_upper(data: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(data.len() * 2);
    for &b in data {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0xF) as usize] as char);
    }
    out
}
