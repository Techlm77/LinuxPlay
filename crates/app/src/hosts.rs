//! `linuxplay hosts` and `linuxplay forget`: the hosts this device has
//! paired with, and dropping one (after a host was reinstalled, say).

use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use linuxplay_client::known_hosts::KnownHosts;

fn load() -> Result<KnownHosts> {
    let path = KnownHosts::default_path();
    KnownHosts::load(path.clone()).with_context(|| format!("reading {}", path.display()))
}

/// Print the paired hosts, most recently used first.
pub fn list() -> Result<()> {
    let hosts = load()?.recent();
    if hosts.is_empty() {
        println!("no paired hosts yet: `linuxplay ADDRESS` connects and pairs");
        return Ok(());
    }
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    for h in hosts {
        println!(
            "{:<24} {:<22} {}  used {}",
            h.address,
            h.name,
            &h.fingerprint[..16.min(h.fingerprint.len())],
            ago(now.saturating_sub(h.last_used)),
        );
    }
    Ok(())
}

/// Forget one host by its address or name.
pub fn forget(target: &str) -> Result<()> {
    let host = load()?.forget_matching(target)?;
    println!("forgot {} ({}); connecting to it again pairs from scratch", host.name, host.address);
    Ok(())
}

fn ago(secs: u64) -> String {
    match secs {
        0..60 => "just now".into(),
        60..3_600 => format!("{} min ago", secs / 60),
        3_600..86_400 => format!("{} h ago", secs / 3_600),
        _ => format!("{} days ago", secs / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::ago;

    #[test]
    fn ages_read_naturally() {
        assert_eq!(ago(5), "just now");
        assert_eq!(ago(120), "2 min ago");
        assert_eq!(ago(7_200), "2 h ago");
        assert_eq!(ago(3 * 86_400), "3 days ago");
    }
}
