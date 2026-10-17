//! Devices on the user's Tailscale network, from `tailscale status --json`,
//! offered as connection targets. Only a convenience: being on the tailnet,
//! or the name a device reports, never makes it trusted — connecting still
//! pairs with a PIN and pins the host's certificate like any other address.

use std::net::IpAddr;
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Peer {
    pub name: String,
    pub ip: IpAddr,
    pub online: bool,
    pub os: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Status {
    #[serde(default)]
    backend_state: String,
    #[serde(default)]
    peer: std::collections::HashMap<String, Node>,
}

#[derive(Deserialize)]
#[serde(rename_all = "PascalCase")]
struct Node {
    #[serde(default)]
    host_name: String,
    #[serde(rename = "TailscaleIPs", default)]
    tailscale_ips: Vec<IpAddr>,
    #[serde(default)]
    online: bool,
    #[serde(rename = "OS", default)]
    os: String,
}

fn parse(json: &[u8]) -> Result<Vec<Peer>> {
    let status: Status = serde_json::from_slice(json).context("reading `tailscale status`")?;
    if status.backend_state != "Running" {
        bail!("Tailscale isn't connected");
    }
    let mut peers: Vec<Peer> = status
        .peer
        .into_values()
        .filter_map(|n| {
            // Prefer the IPv4 address: it is what people type.
            let ip = n.tailscale_ips.iter().find(|ip| ip.is_ipv4()).or(n.tailscale_ips.first()).copied()?;
            let name: String = n.host_name.chars().filter(|c| !c.is_control()).take(64).collect();
            Some(Peer { name, ip, online: n.online, os: n.os.chars().take(16).collect() })
        })
        .collect();
    peers.sort_by(|a, b| b.online.cmp(&a.online).then_with(|| a.name.cmp(&b.name)));
    Ok(peers)
}

/// The other devices on this tailnet; `Ok(empty)` if Tailscale isn't installed.
pub fn peers() -> Result<Vec<Peer>> {
    let out = match Command::new("tailscale").args(["status", "--json"]).output() {
        Ok(out) => out,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(e) => return Err(e).context("running tailscale"),
    };
    if !out.status.success() {
        bail!("Tailscale isn't connected");
    }
    parse(&out.stdout)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_status() {
        let json = br#"{
            "BackendState": "Running",
            "Self": {"HostName": "pc", "TailscaleIPs": ["100.99.122.56"]},
            "Peer": {
                "nodekey:1": {"HostName": "server", "TailscaleIPs": ["fd7a:115c:a1e0::1", "100.70.1.2"], "Online": true, "OS": "linux"},
                "nodekey:2": {"HostName": "phone", "TailscaleIPs": ["100.70.1.3"], "Online": false, "OS": "android"},
                "nodekey:3": {"HostName": "broken"}
            }
        }"#;
        let peers = parse(json).unwrap();
        assert_eq!(peers.len(), 2);
        assert_eq!(peers[0].name, "server");
        assert_eq!(peers[0].ip, "100.70.1.2".parse::<IpAddr>().unwrap());
        assert!(peers[0].online && !peers[1].online);
    }

    #[test]
    fn stopped_tailscale_is_an_error() {
        assert!(parse(br#"{"BackendState": "Stopped", "Peer": {}}"#).is_err());
    }
}
