//! Bringing two devices together: which hosts are reachable right now, and
//! the tunnels that carry bytes between a caller and a host.
//!
//! A tunnel is two WebSockets joined end to end. The devices run their own
//! mutually authenticated TLS session inside it, so everything passing
//! through here is ciphertext to the server.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket};
use futures_util::{SinkExt, StreamExt};
use linuxplay_proto::rendezvous::HostNotice;
use tokio::sync::{mpsc, oneshot};
use tracing::debug;

use crate::secrets;

/// How long a caller waits for the host to pick up its tunnel.
pub const ACCEPT_TIMEOUT: Duration = Duration::from_secs(15);
/// Tunnels waiting for one host at most, so a caller can't flood it.
const MAX_WAITING_PER_HOST: usize = 8;
/// Notices queued for a host that is slow to read them.
const NOTICE_QUEUE: usize = 16;
/// Largest message a tunnel passes on. Signalling messages are small.
pub const MAX_TUNNEL_MESSAGE: usize = 256 << 10;
/// Most bytes a tunnel carries in one direction. It exists for signalling,
/// which is a few kilobytes; this stops it being used to move data.
const MAX_TUNNEL_BYTES: usize = 8 << 20;
/// A tunnel ends after this long whatever happens; streams reconnect.
const MAX_TUNNEL_LIFETIME: Duration = Duration::from_secs(24 * 3600);
/// How often each side is pinged, to notice a peer that vanished.
const KEEPALIVE: Duration = Duration::from_secs(30);

/// A tunnel whose caller is waiting for the host.
struct Waiting {
    host_device: String,
    /// Where the host's socket goes once it accepts.
    host_socket: oneshot::Sender<WebSocket>,
}

/// Hosts that are online, and tunnels on their way to them.
#[derive(Default)]
pub struct Relay {
    /// Online hosts by device id, with a serial telling connections apart.
    hosts: Mutex<HashMap<String, (u64, mpsc::Sender<HostNotice>)>>,
    waiting: Mutex<HashMap<String, Waiting>>,
    serial: Mutex<u64>,
}

/// A host's presence; dropping it takes the host offline (unless a newer
/// connection of the same device has replaced it).
pub struct Online {
    relay: Arc<Relay>,
    device_id: String,
    serial: u64,
    pub notices: mpsc::Receiver<HostNotice>,
}

impl Drop for Online {
    fn drop(&mut self) {
        let mut hosts = self.relay.hosts.lock().unwrap();
        if hosts.get(&self.device_id).is_some_and(|(serial, _)| *serial == self.serial) {
            hosts.remove(&self.device_id);
        }
    }
}

impl Relay {
    /// Mark a host as online. A device has one presence: a new connection
    /// replaces the old one, whose notices then end.
    pub fn go_online(self: &Arc<Self>, device_id: &str) -> Online {
        let (tx, notices) = mpsc::channel(NOTICE_QUEUE);
        let serial = {
            let mut serial = self.serial.lock().unwrap();
            *serial += 1;
            *serial
        };
        self.hosts.lock().unwrap().insert(device_id.to_string(), (serial, tx));
        Online { relay: self.clone(), device_id: device_id.to_string(), serial, notices }
    }

    pub fn is_online(&self, device_id: &str) -> bool {
        self.hosts.lock().unwrap().contains_key(device_id)
    }

    /// Take a host offline at once, e.g. when its device is removed.
    pub fn disconnect(&self, device_id: &str) {
        self.hosts.lock().unwrap().remove(device_id);
    }

    /// Open a tunnel towards `host_device` and tell the host, as built by
    /// `notice` from the tunnel's id. Returns the id and where the host's
    /// socket will arrive, or `None` if the host is offline or swamped.
    pub fn call(
        &self,
        host_device: &str,
        notice: impl FnOnce(String) -> HostNotice,
    ) -> Option<(String, oneshot::Receiver<WebSocket>)> {
        let host = self.hosts.lock().unwrap().get(host_device).map(|(_, tx)| tx.clone())?;
        let id = secrets::new_id();
        let (tx, rx) = oneshot::channel();
        {
            let mut waiting = self.waiting.lock().unwrap();
            if waiting.values().filter(|w| w.host_device == host_device).count() >= MAX_WAITING_PER_HOST {
                return None;
            }
            waiting.insert(id.clone(), Waiting { host_device: host_device.to_string(), host_socket: tx });
        }
        if host.try_send(notice(id.clone())).is_err() {
            self.forget(&id);
            return None;
        }
        Some((id, rx))
    }

    /// The caller gave up waiting.
    pub fn forget(&self, tunnel: &str) {
        self.waiting.lock().unwrap().remove(tunnel);
    }

    /// A host accepts a tunnel: hand its socket to the waiting caller.
    /// Returns false if no such tunnel is waiting for this host.
    pub fn accept(&self, tunnel: &str, host_device: &str, socket: WebSocket) -> bool {
        let waiting = {
            let mut all = self.waiting.lock().unwrap();
            match all.get(tunnel) {
                Some(w) if w.host_device == host_device => all.remove(tunnel),
                _ => None,
            }
        };
        waiting.is_some_and(|w| w.host_socket.send(socket).is_ok())
    }

    /// Can `host_device` accept this tunnel?
    pub fn is_waiting_for(&self, tunnel: &str, host_device: &str) -> bool {
        self.waiting.lock().unwrap().get(tunnel).is_some_and(|w| w.host_device == host_device)
    }
}

/// Pass messages between the two ends until either closes, misbehaves or the
/// tunnel's limits are reached.
pub async fn splice(caller: WebSocket, host: WebSocket) {
    let (mut caller_tx, mut caller_rx) = caller.split();
    let (mut host_tx, mut host_rx) = host.split();
    let mut carried = (0usize, 0usize);
    // The first ping is due one interval from now, not at once.
    let mut keepalive = tokio::time::interval_at(tokio::time::Instant::now() + KEEPALIVE, KEEPALIVE);
    let lifetime = tokio::time::sleep(MAX_TUNNEL_LIFETIME);
    tokio::pin!(lifetime);

    let reason = loop {
        tokio::select! {
            msg = caller_rx.next() => match forward(msg, &mut host_tx, &mut carried.0).await {
                Ok(()) => {}
                Err(why) => break why,
            },
            msg = host_rx.next() => match forward(msg, &mut caller_tx, &mut carried.1).await {
                Ok(()) => {}
                Err(why) => break why,
            },
            _ = keepalive.tick() => {
                let ping = || Message::Ping(Default::default());
                if caller_tx.send(ping()).await.is_err() || host_tx.send(ping()).await.is_err() {
                    break "a side went away";
                }
            }
            _ = &mut lifetime => break "it reached its maximum lifetime",
        }
    };
    debug!("tunnel closed: {reason} ({} + {} bytes)", carried.0, carried.1);
    let _ = caller_tx.close().await;
    let _ = host_tx.close().await;
}

/// Pass one message on. `Err` says why the tunnel should end.
async fn forward(
    message: Option<Result<Message, axum::Error>>,
    to: &mut (impl SinkExt<Message> + Unpin),
    carried: &mut usize,
) -> Result<(), &'static str> {
    match message {
        Some(Ok(Message::Binary(bytes))) => {
            *carried += bytes.len();
            if *carried > MAX_TUNNEL_BYTES {
                return Err("it carried more than a tunnel may");
            }
            to.send(Message::Binary(bytes)).await.map_err(|_| "a side went away")
        }
        // Keepalives are answered by the socket itself.
        Some(Ok(Message::Ping(_) | Message::Pong(_))) => Ok(()),
        Some(Ok(Message::Text(_))) => Err("a side sent text; tunnels carry bytes"),
        Some(Ok(Message::Close(_))) | None => Err("a side closed it"),
        Some(Err(_)) => Err("a side went away"),
    }
}

#[cfg(test)]
mod tests {
    use linuxplay_proto::rendezvous::Caller;

    use super::*;

    fn notice(tunnel: String) -> HostNotice {
        HostNotice::Connect { tunnel, from: Caller::Invite { id: "invite".into() } }
    }

    #[tokio::test]
    async fn a_host_is_online_only_while_its_presence_lives() {
        let relay = Arc::new(Relay::default());
        assert!(relay.call("host", notice).is_none(), "offline hosts can't be called");
        let mut online = relay.go_online("host");
        assert!(relay.is_online("host"));

        let (tunnel, _socket) = relay.call("host", notice).unwrap();
        assert_eq!(online.notices.recv().await, Some(notice(tunnel.clone())));
        assert!(relay.is_waiting_for(&tunnel, "host"));
        assert!(!relay.is_waiting_for(&tunnel, "someone else"), "only the called host may accept");

        drop(online);
        assert!(!relay.is_online("host"));
    }

    #[tokio::test]
    async fn a_newer_connection_replaces_an_older_one() {
        let relay = Arc::new(Relay::default());
        let old = relay.go_online("host");
        let _new = relay.go_online("host");
        drop(old);
        assert!(relay.is_online("host"), "dropping the replaced connection must not take the host offline");
    }

    #[tokio::test]
    async fn one_host_can_only_have_so_many_callers_waiting() {
        let relay = Arc::new(Relay::default());
        let _online = relay.go_online("host");
        let waiting: Vec<_> = (0..MAX_WAITING_PER_HOST).map(|_| relay.call("host", notice).unwrap()).collect();
        assert!(relay.call("host", notice).is_none());
        relay.forget(&waiting[0].0);
        assert!(relay.call("host", notice).is_some());
    }
}
