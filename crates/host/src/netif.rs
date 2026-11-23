//! Which addresses the host listens on. Nothing is exposed by accident: each
//! mode binds only the addresses it names, and a mode that finds none (no LAN
//! address, Tailscale down) is an error rather than a fallback to all
//! interfaces.

use std::net::{IpAddr, Ipv4Addr};

use anyhow::{Context, Result, anyhow};
use clap::ValueEnum;
pub use linuxplay_net::netif::is_tailscale;
use linuxplay_net::netif::{Iface, interfaces, is_private, tailscale_addresses};

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum ListenMode {
    /// Private addresses on this machine's local network interfaces.
    Lan,
    /// This machine's Tailscale addresses only.
    Tailscale,
    /// This machine only (for tunnels or testing).
    Loopback,
    /// Every interface, including public ones. Not recommended.
    All,
}

impl ListenMode {
    pub fn describe(self) -> &'static str {
        match self {
            Self::Lan => "local network only",
            Self::Tailscale => "Tailscale only",
            Self::Loopback => "this computer only",
            Self::All => "ALL interfaces",
        }
    }
}

/// Interfaces that are virtual plumbing or someone else's network rather than
/// the LAN: containers, VMs and VPNs.
fn is_virtual(name: &str) -> bool {
    const PREFIXES: &[&str] = &[
        "lo", "docker", "br-", "veth", "virbr", "vnet", "lxc", "lxd", "cni", "flannel", "podman", "tailscale", "tun",
        "tap", "wg", "zt",
    ];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Private addresses on physical-looking interfaces.
pub fn lan_addresses(ifaces: &[Iface]) -> Vec<IpAddr> {
    let mut v: Vec<IpAddr> =
        ifaces.iter().filter(|i| !is_virtual(&i.name) && is_private(i.addr)).map(|i| i.addr).collect();
    v.dedup();
    v
}

/// The addresses to listen on: `explicit` if given, else those of `mode`.
pub fn resolve(mode: ListenMode, explicit: &[IpAddr]) -> Result<Vec<IpAddr>> {
    if !explicit.is_empty() {
        return Ok(explicit.to_vec());
    }
    Ok(match mode {
        ListenMode::Loopback => vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
        ListenMode::All => vec![IpAddr::V4(Ipv4Addr::UNSPECIFIED)],
        ListenMode::Lan => {
            let v = lan_addresses(&interfaces()?);
            if v.is_empty() {
                return Err(anyhow!(
                    "no private local-network address found; connect to a network, or use --bind ADDRESS"
                ));
            }
            v
        }
        ListenMode::Tailscale => tailscale_addresses().context("choose another --listen mode, or connect Tailscale")?,
    })
}

/// Parse `FIRST-LAST` (or a single port).
pub fn parse_port_range(s: &str) -> Result<(u16, u16), String> {
    let (a, b) = s.split_once('-').unwrap_or((s, s));
    let a: u16 = a.trim().parse().map_err(|_| format!("bad port {a}"))?;
    let b: u16 = b.trim().parse().map_err(|_| format!("bad port {b}"))?;
    if a == 0 || b < a {
        return Err(format!("{s} is not a range like 47801-47816"));
    }
    Ok((a, b))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(name: &str, addr: &str) -> Iface {
        Iface { name: name.into(), addr: addr.parse().unwrap() }
    }

    #[test]
    fn lan_mode_skips_public_loopback_and_virtual_interfaces() {
        let ifaces = [
            iface("lo", "127.0.0.1"),
            iface("eno1", "192.168.8.124"),
            iface("wlp11s0", "192.168.8.120"),
            iface("eth1", "203.0.113.7"),
            iface("tailscale0", "100.99.122.56"),
            iface("docker0", "172.17.0.1"),
            iface("wg0", "10.8.0.2"),
            iface("eno1", "fd00::5"),
            iface("eno1", "fe80::1"),
            iface("eno1", "2001:db8::5"),
        ];
        let got = lan_addresses(&ifaces);
        let want: Vec<IpAddr> = ["192.168.8.124", "192.168.8.120", "fd00::5"].iter().map(|s| s.parse().unwrap()).collect();
        assert_eq!(got, want);
    }

    #[test]
    fn explicit_addresses_win() {
        let ip: IpAddr = "10.1.2.3".parse().unwrap();
        assert_eq!(resolve(ListenMode::All, &[ip]).unwrap(), vec![ip]);
        assert_eq!(resolve(ListenMode::Loopback, &[]).unwrap(), vec!["127.0.0.1".parse::<IpAddr>().unwrap()]);
    }

    #[test]
    fn port_ranges() {
        assert_eq!(parse_port_range("47801-47816"), Ok((47801, 47816)));
        assert_eq!(parse_port_range("5000"), Ok((5000, 5000)));
        assert!(parse_port_range("10-5").is_err());
        assert!(parse_port_range("0-5").is_err());
    }

    #[test]
    fn this_machine_has_interfaces() {
        assert!(interfaces().unwrap().iter().any(|i| i.addr.is_loopback()));
    }
}
