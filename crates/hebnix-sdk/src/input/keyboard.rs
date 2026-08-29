//! hotkey detection via evdev, linux only.
//!
//! Key names match the python `keyboard` lib ("tab", "f2", "ctrl", etc) so
//! old configs still work.
//!
//! Requires read access to `/dev/input/event*`. On Arch that means the
//! current user must be in the `input` group (`sudo usermod -aG input $USER`,
//! then re-login) -- this module does not attempt to elevate privileges, it
//! just returns "not pressed"/empty results if a device can't be opened.

use std::time::{Duration, Instant};

use evdev::{Device, KeyCode};

// (name, evdev KeyCode) for named keys. names line up with the windows vk
// table so existing binds keep working.
fn named_keys() -> &'static [(&'static str, KeyCode)] {
    &[
        ("backspace", KeyCode::KEY_BACKSPACE),
        ("tab", KeyCode::KEY_TAB),
        ("enter", KeyCode::KEY_ENTER),
        ("return", KeyCode::KEY_ENTER),
        ("shift", KeyCode::KEY_LEFTSHIFT),
        ("ctrl", KeyCode::KEY_LEFTCTRL),
        ("control", KeyCode::KEY_LEFTCTRL),
        ("alt", KeyCode::KEY_LEFTALT),
        ("pause", KeyCode::KEY_PAUSE),
        ("caps lock", KeyCode::KEY_CAPSLOCK),
        ("esc", KeyCode::KEY_ESC),
        ("escape", KeyCode::KEY_ESC),
        ("space", KeyCode::KEY_SPACE),
        ("page up", KeyCode::KEY_PAGEUP),
        ("page down", KeyCode::KEY_PAGEDOWN),
        ("end", KeyCode::KEY_END),
        ("home", KeyCode::KEY_HOME),
        ("left", KeyCode::KEY_LEFT),
        ("up", KeyCode::KEY_UP),
        ("right", KeyCode::KEY_RIGHT),
        ("down", KeyCode::KEY_DOWN),
        ("print screen", KeyCode::KEY_SYSRQ),
        ("insert", KeyCode::KEY_INSERT),
        ("delete", KeyCode::KEY_DELETE),
        ("f1", KeyCode::KEY_F1),
        ("f2", KeyCode::KEY_F2),
        ("f3", KeyCode::KEY_F3),
        ("f4", KeyCode::KEY_F4),
        ("f5", KeyCode::KEY_F5),
        ("f6", KeyCode::KEY_F6),
        ("f7", KeyCode::KEY_F7),
        ("f8", KeyCode::KEY_F8),
        ("f9", KeyCode::KEY_F9),
        ("f10", KeyCode::KEY_F10),
        ("f11", KeyCode::KEY_F11),
        ("f12", KeyCode::KEY_F12),
        ("num lock", KeyCode::KEY_NUMLOCK),
        ("scroll lock", KeyCode::KEY_SCROLLLOCK),
        ("left shift", KeyCode::KEY_LEFTSHIFT),
        ("right shift", KeyCode::KEY_RIGHTSHIFT),
        ("left ctrl", KeyCode::KEY_LEFTCTRL),
        ("right ctrl", KeyCode::KEY_RIGHTCTRL),
        ("left alt", KeyCode::KEY_LEFTALT),
        ("right alt", KeyCode::KEY_RIGHTALT),
        ("left windows", KeyCode::KEY_LEFTMETA),
        ("right windows", KeyCode::KEY_RIGHTMETA),
        (";", KeyCode::KEY_SEMICOLON),
        ("=", KeyCode::KEY_EQUAL),
        (",", KeyCode::KEY_COMMA),
        ("-", KeyCode::KEY_MINUS),
        (".", KeyCode::KEY_DOT),
        ("/", KeyCode::KEY_SLASH),
        ("`", KeyCode::KEY_GRAVE),
        ("[", KeyCode::KEY_LEFTBRACE),
        ("\\", KeyCode::KEY_BACKSLASH),
        ("]", KeyCode::KEY_RIGHTBRACE),
        ("'", KeyCode::KEY_APOSTROPHE),
        ("+", KeyCode::KEY_EQUAL),
    ]
}

fn letter_digit_code(c: char) -> Option<KeyCode> {
    Some(match c.to_ascii_lowercase() {
        'a' => KeyCode::KEY_A,
        'b' => KeyCode::KEY_B,
        'c' => KeyCode::KEY_C,
        'd' => KeyCode::KEY_D,
        'e' => KeyCode::KEY_E,
        'f' => KeyCode::KEY_F,
        'g' => KeyCode::KEY_G,
        'h' => KeyCode::KEY_H,
        'i' => KeyCode::KEY_I,
        'j' => KeyCode::KEY_J,
        'k' => KeyCode::KEY_K,
        'l' => KeyCode::KEY_L,
        'm' => KeyCode::KEY_M,
        'n' => KeyCode::KEY_N,
        'o' => KeyCode::KEY_O,
        'p' => KeyCode::KEY_P,
        'q' => KeyCode::KEY_Q,
        'r' => KeyCode::KEY_R,
        's' => KeyCode::KEY_S,
        't' => KeyCode::KEY_T,
        'u' => KeyCode::KEY_U,
        'v' => KeyCode::KEY_V,
        'w' => KeyCode::KEY_W,
        'x' => KeyCode::KEY_X,
        'y' => KeyCode::KEY_Y,
        'z' => KeyCode::KEY_Z,
        '0' => KeyCode::KEY_0,
        '1' => KeyCode::KEY_1,
        '2' => KeyCode::KEY_2,
        '3' => KeyCode::KEY_3,
        '4' => KeyCode::KEY_4,
        '5' => KeyCode::KEY_5,
        '6' => KeyCode::KEY_6,
        '7' => KeyCode::KEY_7,
        '8' => KeyCode::KEY_8,
        '9' => KeyCode::KEY_9,
        _ => return None,
    })
}

/// key name to evdev KeyCode
pub fn name_to_code(key: &str) -> Option<KeyCode> {
    let lower = key.trim().to_lowercase();
    if let Some((_, code)) = named_keys().iter().find(|(n, _)| *n == lower) {
        return Some(*code);
    }
    let mut chars = lower.chars();
    if let (Some(c), None) = (chars.next(), chars.next()) {
        return letter_digit_code(c);
    }
    None
}

/// evdev KeyCode back to key name, used by hotkey capture
pub fn code_to_name(code: KeyCode) -> Option<String> {
    for c in 'a'..='z' {
        if letter_digit_code(c) == Some(code) {
            return Some(c.to_string());
        }
    }
    for c in '0'..='9' {
        if letter_digit_code(c) == Some(code) {
            return Some(c.to_string());
        }
    }
    named_keys()
        .iter()
        .find(|(_, cc)| *cc == code)
        .map(|(n, _)| n.to_string())
}

/// all keyboard-capable evdev devices (has EV_KEY + looks like a keyboard,
/// not a mouse/joystick-only device).
///
/// `evdev::enumerate()` walks and opens every `/dev/input/event*` node,
/// which in practice takes several hundred ms (confirmed live: a poller
/// calling this every 35ms was actually only completing a cycle every
/// ~400-450ms, and was a measurable source of app-wide sluggishness, plus
/// plugin key-bind polls competing for the same enumeration). Cache the
/// opened `Device` handles and reuse them -- `get_key_state()` on an
/// already-open fd is a cheap ioctl. Re-enumerate periodically to pick up
/// hot-plugged keyboards.
fn with_keyboard_devices<T>(f: impl FnOnce(&mut Vec<Device>) -> T) -> T {
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    static CACHE: OnceLock<Mutex<(Instant, Vec<Device>)>> = OnceLock::new();
    let cache =
        CACHE.get_or_init(|| Mutex::new((Instant::now() - Duration::from_secs(60), Vec::new())));
    let mut guard = cache.lock().unwrap();
    if guard.1.is_empty() || guard.0.elapsed() > Duration::from_secs(10) {
        let mut fresh = Vec::new();
        for (_path, dev) in evdev::enumerate() {
            if dev
                .supported_keys()
                .map(|keys| keys.contains(KeyCode::KEY_ENTER) && keys.contains(KeyCode::KEY_A))
                .unwrap_or(false)
            {
                fresh.push(dev);
            }
        }
        guard.1 = fresh;
        guard.0 = Instant::now();
    }
    f(&mut guard.1)
}

/// true if key is held down right now, across all keyboard devices.
pub fn is_key_pressed(key: &str) -> bool {
    let Some(code) = name_to_code(key) else {
        return false;
    };
    with_keyboard_devices(|devs| {
        for dev in devs {
            if let Ok(state) = dev.get_key_state() {
                if state.contains(code) {
                    return true;
                }
            }
        }
        false
    })
}

/// single non-blocking scan, name of whatever key is held or None.
pub fn scan_pressed_key() -> Option<String> {
    with_keyboard_devices(|devs| {
        for dev in devs {
            if let Ok(state) = dev.get_key_state() {
                for code in state.iter() {
                    if let Some(name) = code_to_name(code) {
                        return Some(name);
                    }
                }
            }
        }
        None
    })
}

/// blocks until a key is pressed, returns its name. None if timeout hits first.
pub fn detect_hotkey(timeout: Option<Duration>) -> Option<String> {
    let start = Instant::now();

    // wait for everything to release first so the click that opened capture
    // doesn't count
    loop {
        if scan_pressed_key().is_none() {
            break;
        }
        if let Some(t) = timeout {
            if start.elapsed() > t {
                return None;
            }
        }
        std::thread::sleep(Duration::from_millis(20));
    }

    loop {
        if let Some(name) = scan_pressed_key() {
            return Some(name);
        }
        if let Some(t) = timeout {
            if start.elapsed() > t {
                return None;
            }
        }
        std::thread::sleep(Duration::from_millis(15));
    }
}
