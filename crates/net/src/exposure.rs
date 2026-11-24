//! Noticing that something meant for Tailscale can be reached from the
//! internet: it listens on a public address, or a public address got in
//! touch, which means a port was opened on the router.

use std::collections::HashSet;
use std::net::{IpAddr, SocketAddr};
use std::sync::Mutex;

use tracing::warn;

use crate::netif::{EXPOSED_ADVICE, is_public};

/// Public callers remembered, so each is reported once.
const MAX_REMEMBERED: usize = 1024;

pub struct Exposure {
    listens_publicly: bool,
    public_callers: Mutex<HashSet<IpAddr>>,
}

impl Exposure {
    /// `listening` is every address being listened on.
    pub fn new(listening: &[SocketAddr]) -> Self {
        let listens_publicly = listening.iter().any(|a| a.ip().is_unspecified() || is_public(a.ip()));
        if listens_publicly {
            warn!("listening where the internet can reach it. {EXPOSED_ADVICE}");
        }
        Self { listens_publicly, public_callers: Mutex::default() }
    }

    /// Note who got in touch. Returns true the first time a given public
    /// address does, for the caller to tell the person at this computer.
    pub fn contact(&self, caller: IpAddr) -> bool {
        if !is_public(caller) {
            return false;
        }
        let mut seen = self.public_callers.lock().unwrap();
        let first = seen.len() < MAX_REMEMBERED && seen.insert(caller.to_canonical());
        if first {
            warn!("{caller} reached this computer from the internet. {EXPOSED_ADVICE}");
        }
        first
    }

    /// What to show, while there is anything to warn about.
    pub fn warning(&self) -> Option<String> {
        let heard_from_internet = !self.public_callers.lock().unwrap().is_empty();
        (self.listens_publicly || heard_from_internet).then(|| EXPOSED_ADVICE.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn addr(s: &str) -> SocketAddr {
        s.parse().unwrap()
    }

    #[test]
    fn only_the_internet_raises_the_warning() {
        let exposure = Exposure::new(&[addr("100.101.102.104:47800"), addr("192.168.1.20:47800")]);
        for quiet in ["127.0.0.1", "100.101.102.104", "192.168.1.20", "fd7a:115c:a1e0::1"] {
            assert!(!exposure.contact(quiet.parse().unwrap()));
        }
        assert_eq!(exposure.warning(), None);
        assert!(exposure.contact("203.0.113.9".parse().unwrap()));
        assert!(!exposure.contact("203.0.113.9".parse().unwrap()), "reported once");
        assert!(exposure.warning().unwrap().contains("Tailscale"));
    }

    #[test]
    fn listening_everywhere_or_publicly_is_a_warning_from_the_start() {
        assert!(Exposure::new(&[addr("0.0.0.0:47800")]).warning().is_some());
        assert!(Exposure::new(&[addr("203.0.113.9:47800")]).warning().is_some());
        assert!(Exposure::new(&[addr("127.0.0.1:47800")]).warning().is_none());
    }
}
