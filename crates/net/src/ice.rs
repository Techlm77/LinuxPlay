//! Finding the addresses a media session can be reached at.
//!
//! When the peer connected to us directly we already know which interface
//! leads to it, and one socket there is enough. When the connection was
//! relayed through the rendezvous server the peer could be anywhere, so we
//! offer a socket on every interface, plus the public address a STUN server
//! sees each one as, which is what gets through a home router.

use std::net::{IpAddr, SocketAddr, UdpSocket};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use linuxplay_proto::stun;
use rand::RngCore;
use str0m::Candidate;
use tracing::debug;

use crate::netif;

/// How long to wait for STUN answers. A reply normally takes one round trip
/// to the server; an interface with no way out simply never answers.
const STUN_WAIT: Duration = Duration::from_millis(400);

/// Where a session's peer is, as far as that decides which sockets to open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// The peer talks to us directly through the interface with this address.
    Interface(IpAddr),
    /// The peer's whereabouts are unknown; `stun` is a server to ask for our
    /// public addresses.
    Anywhere { stun: Option<SocketAddr> },
}

/// Bound media sockets and the candidates that describe them to the peer.
pub struct Gathered {
    pub sockets: Vec<UdpSocket>,
    pub candidates: Vec<Candidate>,
}

/// Open the sockets for `reach`, each on the first free port of `ports`
/// (inclusive), or on any port.
pub async fn gather(reach: Reach, ports: Option<(u16, u16)>) -> Result<Gathered> {
    let (addresses, stun) = match reach {
        Reach::Interface(ip) => (vec![ip], None),
        Reach::Anywhere { stun } => (netif::media_addresses()?, stun),
    };
    let mut sockets = Vec::new();
    let mut last_error = anyhow!("this computer has no network address to stream on");
    for ip in addresses {
        match bind_socket(ip, ports) {
            Ok(socket) => sockets.push(socket),
            // One interface failing (say, its port range is used up) is no
            // reason to give up on the others.
            Err(e) => last_error = e,
        }
    }
    if sockets.is_empty() {
        return Err(last_error);
    }

    let mut candidates = Vec::new();
    for socket in &sockets {
        let addr = socket.local_addr()?;
        candidates.push(Candidate::host(addr, "udp").map_err(|e| anyhow!("host candidate {addr}: {e}"))?);
    }
    if let Some(server) = stun {
        for (base, public) in public_addresses(&sockets, server).await {
            debug!("{base} is {public} from outside");
            match Candidate::server_reflexive(public, base, "udp") {
                Ok(candidate) => candidates.push(candidate),
                Err(e) => debug!("public candidate {public}: {e}"),
            }
        }
    }
    Ok(Gathered { sockets, candidates })
}

/// Bind on `local_ip`, on the first free port of `ports` (or any port).
fn bind_socket(local_ip: IpAddr, ports: Option<(u16, u16)>) -> Result<UdpSocket> {
    let Some((first, last)) = ports else {
        return UdpSocket::bind((local_ip, 0)).with_context(|| format!("binding a media socket on {local_ip}"));
    };
    for port in first..=last {
        match UdpSocket::bind((local_ip, port)) {
            Ok(s) => return Ok(s),
            Err(e) if e.kind() == std::io::ErrorKind::AddrInUse => continue,
            Err(e) => return Err(e).with_context(|| format!("binding media port {local_ip}:{port}")),
        }
    }
    bail!("all media ports {first}-{last} on {local_ip} are in use")
}

/// Ask `server` how each socket looks from outside. Returns `(local, public)`
/// for the sockets that got an answer and are in fact behind a translator.
async fn public_addresses(sockets: &[UdpSocket], server: SocketAddr) -> Vec<(SocketAddr, SocketAddr)> {
    let queries = sockets.iter().filter_map(|socket| {
        let local = socket.local_addr().ok()?;
        // A socket can only reach a server of its own address family.
        (local.is_ipv4() == server.is_ipv4()).then(|| query(socket, local, server))
    });
    let answers = futures_util::future::join_all(queries).await;
    answers.into_iter().flatten().filter(|(local, public)| local != public).collect()
}

async fn query(socket: &UdpSocket, local: SocketAddr, server: SocketAddr) -> Option<(SocketAddr, SocketAddr)> {
    let socket = socket.try_clone().ok()?;
    socket.set_nonblocking(true).ok()?;
    let socket = tokio::net::UdpSocket::from_std(socket).ok()?;
    let mut id = stun::TransactionId::default();
    rand::rng().fill_bytes(&mut id);
    socket.send_to(&stun::binding_request(&id), server).await.ok()?;
    let answer = async {
        let mut packet = [0u8; 576];
        loop {
            let (len, from) = socket.recv_from(&mut packet).await.ok()?;
            if from == server
                && let Some(public) = stun::parse_binding_response(&packet[..len], &id) {
                    return Some(public);
                }
        }
    };
    let public = tokio::time::timeout(STUN_WAIT, answer).await.ok()??;
    Some((local, public))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn a_direct_peer_gets_one_socket_on_its_interface() {
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        let g = gather(Reach::Interface(ip), None).await.unwrap();
        assert_eq!((g.sockets.len(), g.candidates.len()), (1, 1));
        assert_eq!(g.sockets[0].local_addr().unwrap().ip(), ip);
    }

    #[test]
    fn media_ports_come_from_the_range() {
        let ip: IpAddr = "127.0.0.1".parse().unwrap();
        // Find a few free adjacent ports, then use them up.
        let probe = UdpSocket::bind((ip, 0)).unwrap();
        let base = probe.local_addr().unwrap().port().clamp(1024, 65000);
        drop(probe);
        let range = (base, base + 3);
        let held: Vec<UdpSocket> = (0..4).filter_map(|_| bind_socket(ip, Some(range)).ok()).collect();
        assert!(held.len() >= 2, "most of the range should be free");
        assert!(held.iter().all(|s| (range.0..=range.1).contains(&s.local_addr().unwrap().port())));
        if held.len() == 4 {
            assert!(bind_socket(ip, Some(range)).is_err(), "a full range must fail, not fall back to any port");
        }
    }

    #[tokio::test]
    async fn stun_reports_how_a_socket_looks_from_outside() {
        // A stand-in server that claims every caller comes from 203.0.113.9.
        let server = tokio::net::UdpSocket::bind("127.0.0.1:0").await.unwrap();
        let server_addr = server.local_addr().unwrap();
        let seen_as: SocketAddr = "203.0.113.9:4444".parse().unwrap();
        tokio::spawn(async move {
            let mut packet = [0u8; 576];
            while let Ok((len, from)) = server.recv_from(&mut packet).await {
                if let Some(id) = stun::parse_binding_request(&packet[..len]) {
                    let _ = server.send_to(&stun::binding_response(&id, seen_as), from).await;
                }
            }
        });
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        let local = socket.local_addr().unwrap();
        assert_eq!(public_addresses(&[socket], server_addr).await, vec![(local, seen_as)]);

        // A server that never answers costs a short wait, not an error.
        let silent = UdpSocket::bind("127.0.0.1:0").unwrap();
        let socket = UdpSocket::bind("127.0.0.1:0").unwrap();
        assert!(public_addresses(&[socket], silent.local_addr().unwrap()).await.is_empty());
    }
}
