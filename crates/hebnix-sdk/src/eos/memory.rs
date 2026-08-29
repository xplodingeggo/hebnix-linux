//! read-only process memory scanner (linux), port of hebnix.eos._memory
//!
//! finds the eg1~eyJ... bearer token inside a running EpicGamesLauncher.exe
//! (typically under Proton/Wine via Heroic or Legendary) without injection:
//! `/proc/<pid>/maps` for the region list + `/proc/<pid>/mem` (pread) for the
//! bytes, the procfs equivalent of VirtualQueryEx/ReadProcessMemory.
//!
//! Purely read-only and external to Rocket League itself -- not touching the
//! game process, no EAC concern. This is a nice-to-have: if no launcher
//! process is found (most Linux users auth via Steam instead), everything
//! here just returns empty/None.

use std::fs::File;
use std::io::{BufRead, BufReader, Read, Seek, SeekFrom};
use std::os::unix::fs::FileExt;

use sysinfo::{ProcessRefreshKind, ProcessesToUpdate, System};

/// pid of the first process whose name/exe contains `name` (case-insensitive).
/// works for a native linux binary or a wine/proton-hosted .exe.
pub fn find_process(name: &str) -> Option<u32> {
    let needle = name.to_ascii_lowercase().trim_end_matches(".exe").to_string();
    let mut sys = System::new();
    sys.refresh_processes_specifics(
        ProcessesToUpdate::All,
        true,
        ProcessRefreshKind::nothing().with_exe(sysinfo::UpdateKind::Always),
    );
    for (pid, proc_) in sys.processes() {
        let pname = proc_.name().to_string_lossy().to_lowercase();
        if pname.contains(&needle) {
            return Some(pid.as_u32());
        }
        if let Some(exe) = proc_.exe() {
            if exe
                .file_name()
                .map(|f| f.to_string_lossy().to_lowercase().contains(&needle))
                .unwrap_or(false)
            {
                return Some(pid.as_u32());
            }
        }
    }
    None
}

struct MapRegion {
    start: usize,
    end: usize,
}

/// parse `/proc/<pid>/maps`, keep only regions with read permission ('r' in
/// the perms column). matches the "committed + readable" filter the windows
/// version applied via VirtualQueryEx.
fn read_regions(pid: u32) -> Vec<MapRegion> {
    let mut regions = Vec::new();
    let Ok(file) = File::open(format!("/proc/{pid}/maps")) else {
        return regions;
    };
    for line in BufReader::new(file).lines().map_while(Result::ok) {
        // "<start>-<end> <perms> ..."
        let mut parts = line.splitn(3, ' ');
        let Some(range) = parts.next() else { continue };
        let Some(perms) = parts.next() else { continue };
        if !perms.starts_with('r') {
            continue;
        }
        let Some((start_s, end_s)) = range.split_once('-') else {
            continue;
        };
        let (Ok(start), Ok(end)) = (
            usize::from_str_radix(start_s, 16),
            usize::from_str_radix(end_s, 16),
        ) else {
            continue;
        };
        if end > start {
            regions.push(MapRegion { start, end });
        }
    }
    regions
}

/// scan pid's readable memory for `needle`, return the surrounding tokens
/// (deduped, len > 50). same token-boundary extraction as the windows/py
/// scanner.
pub fn scan_memory(pid: u32, needle: &[u8]) -> Vec<String> {
    let mut tokens: Vec<String> = Vec::new();
    let Ok(mut mem) = File::open(format!("/proc/{pid}/mem")) else {
        return tokens;
    };

    let regions = read_regions(pid);
    let mut buf = vec![0u8; 65536];

    for region in regions {
        let mut addr = region.start;
        while addr < region.end {
            let chunk = (region.end - addr).min(buf.len());
            match mem.read_at(&mut buf[..chunk], addr as u64) {
                Ok(n) if n > 0 => extract_tokens(&buf[..n], needle, &mut tokens),
                // unreadable page (unmapped mid-region, permission race, etc) --
                // skip it and keep scanning rather than aborting the whole pid.
                _ => {
                    let _ = mem.seek(SeekFrom::Start(addr as u64));
                    let mut probe = [0u8; 1];
                    if mem.read(&mut probe).is_err() {
                        // whole region unreadable, bail out of it
                        break;
                    }
                }
            }
            addr += chunk;
        }
    }
    tokens
}

/// chars that make up a bearer/jwt token: base64url alphabet + the eg1~ marker's
/// ~, jwt separators ., and base64 +/=.
fn is_token_char(b: u8) -> bool {
    b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.' | b'~' | b'+' | b'/' | b'=')
}

/// pull out token-char runs containing `needle`, dedupe into `tokens` (len > 50).
fn extract_tokens(data: &[u8], needle: &[u8], tokens: &mut Vec<String>) {
    if needle.is_empty() {
        return;
    }
    let mut off = 0usize;
    while off < data.len() {
        let Some(rel) = find_sub(&data[off..], needle) else {
            break;
        };
        let hit = off + rel;

        let mut start = hit;
        while start > 0 && is_token_char(data[start - 1]) {
            start -= 1;
        }
        let mut end = hit;
        while end < data.len() && is_token_char(data[end]) {
            end += 1;
        }

        let raw = String::from_utf8_lossy(&data[start..end]).into_owned();
        if raw.len() > 50 && !tokens.contains(&raw) {
            tokens.push(raw);
        }
        off = end.max(hit + 1);
    }
}

fn find_sub(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
