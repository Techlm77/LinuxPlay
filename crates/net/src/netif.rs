//! This machine's network addresses.

use std::ffi::CStr;
use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

use std::process::Command;

use anyhow::{Context, Result, bail};

/// Addresses offered for media at most; machines with more are rare and the
/// other side has to try each one.
const MAX_MEDIA_ADDRESSES: usize = 8;

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

/// Tailscale's address ranges: 100.64.0.0/10 and fd7a:115c:a1e0::/48.
pub fn is_tailscale(ip: IpAddr) -> bool {
    match ip.to_canonical() {
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

/// Private to a home or office network: RFC 1918 and unique local IPv6.
pub fn is_private(ip: IpAddr) -> bool {
    match ip.to_canonical() {
        IpAddr::V4(v4) => v4.is_private(),
        IpAddr::V6(v6) => v6.segments()[0] & 0xfe00 == 0xfc00,
    }
}

/// Could this address be someone's on the open internet? Everything is,
/// except this machine, private and link-local networks, Tailscale (and the
/// shared carrier range it lives in) and the unspecified address.
pub fn is_public(ip: IpAddr) -> bool {
    let ip = ip.to_canonical();
    if ip.is_loopback() || ip.is_unspecified() || is_private(ip) || is_tailscale(ip) {
        return false;
    }
    match ip {
        IpAddr::V4(v4) => !v4.is_link_local() && !v4.is_broadcast(),
        IpAddr::V6(v6) => v6.segments()[0] & 0xffc0 != 0xfe80,
    }
}

/// What to tell someone whose LinuxPlay is reachable from the internet.
pub const EXPOSED_ADVICE: &str = "LinuxPlay is reachable from the internet. Close the ports you opened on your \
    router (or stop listening on public addresses) and connect over Tailscale instead: it is private, \
    encrypted, and needs no open ports.";

/// This machine's Tailscale addresses that are up, from `tailscale ip`, or
/// from the `tailscale*` interface when the command isn't installed. Fails
/// when Tailscale isn't connected.
pub fn tailscale_addresses() -> Result<Vec<IpAddr>> {
    let ifaces = interfaces()?;
    let local: Vec<IpAddr> = ifaces.iter().map(|i| i.addr).collect();
    let found = match Command::new("tailscale").arg("ip").output() {
        Ok(out) if out.status.success() => String::from_utf8_lossy(&out.stdout)
            .lines()
            .filter_map(|l| l.trim().parse::<IpAddr>().ok())
            .filter(|ip| is_tailscale(*ip))
            .collect::<Vec<_>>(),
        Ok(out) => bail!("Tailscale isn't connected ({})", String::from_utf8_lossy(&out.stderr).trim()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
            ifaces.iter().filter(|i| i.name.starts_with("tailscale") && is_tailscale(i.addr)).map(|i| i.addr).collect()
        }
        Err(e) => return Err(e).context("running `tailscale ip`"),
    };
    let up: Vec<IpAddr> = found.into_iter().filter(|ip| local.contains(ip)).collect();
    if up.is_empty() {
        bail!("this machine has no Tailscale address up; install Tailscale and connect it");
    }
    Ok(up)
}

/// Bridges to containers and virtual machines on this computer: nobody
/// elsewhere can reach us through those.
fn is_local_bridge(name: &str) -> bool {
    const PREFIXES: &[&str] = &["docker", "br-", "veth", "virbr", "vnet", "lxc", "lxd", "cni", "flannel", "podman"];
    PREFIXES.iter().any(|p| name.starts_with(p))
}

/// Can a peer somewhere else send media to this address?
fn reachable(iface: &Iface) -> bool {
    if is_local_bridge(&iface.name) || iface.addr.is_loopback() {
        return false;
    }
    match iface.addr {
        IpAddr::V4(v4) => !v4.is_link_local(),
        // Link-local IPv6 only works with a scope, which candidates can't carry.
        IpAddr::V6(v6) => v6.segments()[0] & 0xffc0 != 0xfe80,
    }
}

/// The addresses to offer a peer whose whereabouts are unknown: every real
/// interface, VPNs included, IPv4 first.
pub fn media_addresses() -> Result<Vec<IpAddr>> {
    let mut addrs: Vec<IpAddr> = interfaces()?.iter().filter(|i| reachable(i)).map(|i| i.addr).collect();
    addrs.sort_by_key(|a| a.is_ipv6());
    addrs.dedup();
    addrs.truncate(MAX_MEDIA_ADDRESSES);
    Ok(addrs)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn iface(name: &str, addr: &str) -> Iface {
        Iface { name: name.into(), addr: addr.parse().unwrap() }
    }

    #[test]
    fn only_addresses_a_peer_could_use_are_offered() {
        assert!(reachable(&iface("eno1", "192.168.8.124")));
        assert!(reachable(&iface("tailscale0", "100.101.102.103")));
        assert!(reachable(&iface("wlan0", "2a00:23c8::1")));
        assert!(!reachable(&iface("lo", "127.0.0.1")));
        assert!(!reachable(&iface("docker0", "172.17.0.1")));
        assert!(!reachable(&iface("eno1", "fe80::1")));
        assert!(!reachable(&iface("eno1", "169.254.3.4")));
    }

    #[test]
    fn public_means_reachable_from_the_internet() {
        for public in ["8.8.8.8", "203.0.113.7", "2001:4860::8888", "::ffff:8.8.8.8"] {
            assert!(is_public(public.parse().unwrap()), "{public}");
        }
        for not_public in [
            "127.0.0.1", "0.0.0.0", "192.168.8.124", "10.1.2.3", "172.16.0.1", "169.254.1.1", "100.99.122.56",
            "::1", "::", "fd00::5", "fe80::1", "fd7a:115c:a1e0::1", "::ffff:192.168.1.1",
        ] {
            assert!(!is_public(not_public.parse().unwrap()), "{not_public}");
        }
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
    fn this_machine_has_interfaces() {
        assert!(interfaces().unwrap().iter().any(|i| i.addr.is_loopback()));
        assert!(media_addresses().unwrap().len() <= MAX_MEDIA_ADDRESSES);
    }
}
