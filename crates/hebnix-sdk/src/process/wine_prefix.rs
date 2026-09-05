//! Locate Rocket League's Wine/Proton prefix(es) on Linux.
//!
//! RL has no native Linux client anymore (F2P/Epic dropped it) - everyone
//! runs it through some Wine prefix, and its `.save` files live under that
//! prefix's emulated "My Documents", not the host's real `~/Documents`.
//! There's no single fixed prefix location either: people commonly run it
//! as a non-Steam game through Steam Play (a numeric "shortcut" appid under
//! steamapps/compatdata, different per machine) or through Heroic (Epic
//! games get their own prefix under ~/Games/Heroic/Prefixes). So rather
//! than guessing one path, scan every prefix we can find and let the
//! caller pick whichever one actually has the newest save file.

use std::path::{Path, PathBuf};

/// every Steam library's steamapps dir (the default one, plus any extra
/// libraries listed in libraryfolders.vdf - e.g. a game installed on a
/// second drive still gets its compatdata under that library, not the
/// default one).
fn steam_library_steamapps_dirs() -> Vec<PathBuf> {
    let Some(home) = dirs::home_dir() else {
        return Vec::new();
    };
    let default_steam = home.join(".local/share/Steam");
    let mut roots = vec![default_steam.join("steamapps")];

    let vdf = default_steam.join("steamapps/libraryfolders.vdf");
    if let Ok(text) = std::fs::read_to_string(&vdf) {
        let re = regex::Regex::new(r#""path"\s*"([^"]+)""#).expect("valid libraryfolders regex");
        for capture in re.captures_iter(&text) {
            let path = PathBuf::from(&capture[1]).join("steamapps");
            if !roots.contains(&path) {
                roots.push(path);
            }
        }
    }
    roots
}

/// every Wine/Proton prefix that might be running Rocket League: Steam
/// Proton prefixes (any compatdata id - RL is usually added as a non-Steam
/// game, so the id is an arbitrary per-machine "shortcut" appid, not a
/// fixed one) and Heroic's per-game prefixes. `HEBNIX_WINE_PREFIX` (a
/// prefix root, i.e. the dir containing `drive_c`) can be set to add
/// anything unusual (Lutris, Bottles, a custom prefix, etc) without
/// needing code changes.
pub fn wine_prefixes() -> Vec<PathBuf> {
    let mut prefixes = Vec::new();

    for steamapps in steam_library_steamapps_dirs() {
        let compatdata = steamapps.join("compatdata");
        if let Ok(entries) = std::fs::read_dir(&compatdata) {
            for entry in entries.flatten() {
                prefixes.push(entry.path().join("pfx"));
            }
        }
    }

    if let Some(home) = dirs::home_dir() {
        let heroic = home.join("Games/Heroic/Prefixes");
        if let Ok(entries) = std::fs::read_dir(&heroic) {
            for entry in entries.flatten() {
                prefixes.push(entry.path().join("pfx"));
            }
        }
    }

    if let Ok(custom) = std::env::var("HEBNIX_WINE_PREFIX") {
        prefixes.push(PathBuf::from(custom));
    }

    prefixes.retain(|p| p.is_dir());
    prefixes
}

/// every `drive_c/users/*/Documents` dir across every known prefix, in no
/// particular order - caller decides which one actually has what it wants.
pub fn candidate_documents_dirs() -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for prefix in wine_prefixes() {
        let users = prefix.join("drive_c/users");
        let Ok(entries) = std::fs::read_dir(&users) else {
            continue;
        };
        for entry in entries.flatten() {
            let docs = entry.path().join("Documents");
            if docs.is_dir() {
                dirs.push(docs);
            }
        }
    }
    dirs
}

/// the `drive_c/users/*/Documents` dir belonging to the same prefix a
/// running process's exe lives under, if any - used when we already know
/// exactly which prefix is live (a running RL process) so we don't have to
/// guess between multiple installed prefixes.
pub fn documents_dir_for_exe(exe_path: &Path) -> Option<PathBuf> {
    let drive_c = exe_path
        .ancestors()
        .find(|ancestor| ancestor.file_name().is_some_and(|name| name == "drive_c"))?;
    let users = drive_c.join("users");
    let entries = std::fs::read_dir(&users).ok()?;
    let mut candidates: Vec<PathBuf> = entries
        .flatten()
        .map(|entry| entry.path().join("Documents"))
        .filter(|docs| docs.is_dir())
        .collect();
    // Proton always uses "steamuser"; prefer it if present, otherwise
    // whichever single user profile exists (Heroic/Lutris/etc name theirs
    // after the real Linux username).
    let steamuser = users.join("steamuser").join("Documents");
    if let Some(pos) = candidates.iter().position(|c| *c == steamuser) {
        return Some(candidates.remove(pos));
    }
    candidates.into_iter().next()
}
