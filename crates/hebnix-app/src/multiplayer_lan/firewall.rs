//! Workshop LAN multiplayer firewall rules, Linux-native via `nftables`.
//!
//! Windows uses the single authoritative Windows Firewall API (`netsh
//! advfirewall`). Linux has no equivalent single authority - nftables,
//! iptables, firewalld and ufw all coexist, and a separate restrictive base
//! chain from one of those can still drop our traffic regardless of what we
//! add here. This adds an allow rule to our own `inet hebnix` table (the
//! common case: no separate restrictive firewall active), but isn't a
//! substitute for the user allowing the ports themselves if they run one.
//! Requires CAP_NET_ADMIN (same capability the TAP device needs).

use std::path::Path;
use std::process::Command;

const CHAIN_IN: &str = "input";
const LAN_PORTS: &str = "7777-7778, 14000-14010";

pub fn ensure_host_rule(executable: &Path, port: u16) -> Result<(), String> {
    ensure_table()?;
    let comment = format!("hebnix-workshop-lan-host-{port}");
    ensure_udp_rule(&comment, &format!("udp dport {port}"), executable)
}

pub fn ensure_join_rule_if_needed(
    executable: &Path,
    host_ip: &str,
    host_port: u16,
) -> Result<(), String> {
    ensure_table()?;
    let comment = format!("hebnix-workshop-lan-guest-{host_ip}-{host_port}");
    ensure_udp_rule(
        &comment,
        &format!("ip saddr {host_ip} udp sport {host_port}"),
        executable,
    )
}

pub fn ensure_rocket_league_lan_rule(executable: &Path, remote: &str) -> Result<(), String> {
    ensure_table()?;
    let comment = format!("hebnix-workshop-lan-rl-{remote}");
    ensure_udp_rule(
        &comment,
        &format!("ip saddr {remote} udp dport {{ {LAN_PORTS} }}"),
        executable,
    )
}

pub fn remove_rules() -> Result<(), String> {
    // dropping the whole table removes every rule we ever added in one shot
    let _ = Command::new("nft").args(["delete", "table", "inet", "hebnix"]).output();
    Ok(())
}

fn ensure_table() -> Result<(), String> {
    let exists = Command::new("nft")
        .args(["list", "table", "inet", "hebnix"])
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false);
    if exists {
        return Ok(());
    }
    run(&["add", "table", "inet", "hebnix"])?;
    run(&[
        "add", "chain", "inet", "hebnix", CHAIN_IN, "{", "type", "filter", "hook", "input",
        "priority", "0", ";", "policy", "accept", ";", "}",
    ])
}

fn ensure_udp_rule(comment: &str, matcher: &str, executable: &Path) -> Result<(), String> {
    if rule_exists(comment)? {
        return Ok(());
    }
    let _ = executable; // Linux firewall rules match on traffic shape, not the calling binary
    let mut args: Vec<String> = vec!["add", "rule", "inet", "hebnix", CHAIN_IN]
        .into_iter()
        .map(String::from)
        .collect();
    args.extend(matcher.split_whitespace().map(String::from));
    args.push("accept".to_string());
    args.push("comment".to_string());
    args.push(format!("\"{comment}\""));
    run(&args.iter().map(String::as_str).collect::<Vec<_>>())
}

fn rule_exists(comment: &str) -> Result<bool, String> {
    let output = Command::new("nft")
        .args(["list", "table", "inet", "hebnix"])
        .output()
        .map_err(|e| format!("could not query nftables: {e}"))?;
    Ok(String::from_utf8_lossy(&output.stdout).contains(comment))
}

fn run(args: &[&str]) -> Result<(), String> {
    let output = Command::new("nft")
        .args(args)
        .output()
        .map_err(|e| format!("could not update nftables: {e}"))?;
    if output.status.success() {
        Ok(())
    } else {
        Err(String::from_utf8_lossy(&output.stderr).trim().to_string())
    }
}
