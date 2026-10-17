//! Which addresses the host listens on. Nothing is exposed by accident: each
//! mode binds only the addresses it names, and a mode that finds none (no LAN
//! address, Tailscale down) is an error rather than a fallback to all
//! interfaces.

use std::ffi::CStr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};
use std::process::Command;

use anyhow::{Context, Result, anyhow, bail};
use clap::ValueEnum;

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

/// One address on one network interface.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Iface {
    pub name: String,
    pub addr: IpAddr,
}

/// The addresses on interfaces that are up.
pub fn interfaces() -> Result<Vec<Iface>> {
    let mut out = Vec::new();
    // SAFETY: getifaddrs fills a linked list that we only read, and free once.
    unsafe {
        let mut head: *mut libc::ifaddrs = std::ptr::null_mut();
        if libc::getifaddrs(&mut head) != 0 {
            return Err(std::io::Error::last_os_error()).context("listing network interfaces");
        }
        let mut cur = head;
        while !cur.is_null() {
            let ifa = &*cur;
            cur = ifa.ifa_next;
            if ifa.ifa_addr.is_null() || ifa.ifa_flags & libc::IFF_UP as libc::c_uint == 0 {
                continue;
            }
            let addr = match (*ifa.ifa_addr).sa_family as libc::c_int {
                libc::AF_INET => {
                    let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in);
                    IpAddr::V4(Ipv4Addr::from(u32::from_be(sa.sin_addr.s_addr)))
                }
                libc::AF_INET6 => {
                    let sa = &*(ifa.ifa_addr as *const libc::sockaddr_in6);
                    IpAddr::V6(Ipv6Addr::from(sa.sin6_addr.s6_addr))
                }
                _ => continue,
            };
            let name = CStr::from_ptr(ifa.ifa_name).to_string_lossy().into_owned();
            out.push(Iface { name, addr });
        }
        libc::freeifaddrs(head);
    }
    Ok(out)
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

fn is_private(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => v4.is_private(),
        // Unique local addresses (fc00::/7). Link-local needs a scope id to
        // bind, so it isn't offered.
        IpAddr::V6(v6) => v6.segments()[0] & 0xfe00 == 0xfc00,
    }
}

/// Tailscale's address ranges: 100.64.0.0/10 and fd7a:115c:a1e0::/48.
pub fn is_tailscale(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            let o = v4.octets();
            o[0] == 100 && (o[1] & 0xc0) == 64
        }
        IpAddr::V6(v6) => {
            let s = v6.segments();
            s[0] == 0xfd7a && s[1] == 0x115c && s[2] == 0xa1e0
        }
    }
}

/// Private addresses on physical-looking interfaces.
pub fn lan_addresses(ifaces: &[Iface]) -> Vec<IpAddr> {
    let mut v: Vec<IpAddr> =
        ifaces.iter().filter(|i| !is_virtual(&i.name) && is_private(i.addr)).map(|i| i.addr).collect();
    v.dedup();
    v
}

/// This node's Tailscale addresses that are actually up on an interface,
/// from `tailscale ip`, or from the `tailscale*` interface if the CLI isn't
/// installed. Fails (closed) when Tailscale isn't connected.
fn tailscale_addresses(ifaces: &[Iface]) -> Result<Vec<IpAddr>> {
    let local: Vec<IpAddr> = ifaces.iter().map(|i| i.addr).collect();
    let from_cli = match Command::new("tailscale").arg("ip").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<IpAddr>().ok())
            .filter(|ip| is_tailscale(*ip))
            .collect::<Vec<_>>(),
        Ok(out) => bail!(
            "Tailscale isn't connected ({}); start it, or choose another --listen mode",
            String::from_utf8_lossy(&out.stderr).trim()
        ),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => ifaces
            .iter()
            .filter(|i| i.name.starts_with("tailscale") && is_tailscale(i.addr))
            .map(|i| i.addr)
            .collect(),
        Err(e) => return Err(e).context("running `tailscale ip`"),
    };
    let up: Vec<IpAddr> = from_cli.into_iter().filter(|ip| local.contains(ip)).collect();
    if up.is_empty() {
        bail!("this machine has no Tailscale address up; connect Tailscale, or choose another --listen mode");
    }
    Ok(up)
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
        ListenMode::Tailscale => tailscale_addresses(&interfaces()?)?,
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
    fn tailscale_ranges() {
        assert!(is_tailscale("100.99.122.56".parse().unwrap()));
        assert!(is_tailscale("100.64.0.1".parse().unwrap()));
        assert!(!is_tailscale("100.128.0.1".parse().unwrap()));
        assert!(is_tailscale("fd7a:115c:a1e0::1".parse().unwrap()));
        assert!(!is_tailscale("192.168.1.1".parse().unwrap()));
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
