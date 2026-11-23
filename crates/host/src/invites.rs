//! Invites this host has handed out.
//!
//! An invite lets one guest in for a while without making them a trusted
//! device: the link carries a secret, the guest proves it to us with the same
//! exchange that checks a pairing PIN, and what they may do is fixed when the
//! invite is made. The rendezvous server only knows the invite's id.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand::RngCore;

/// Bytes of randomness in an invite's secret.
const SECRET_BYTES: usize = 16;
/// Invites one host keeps at a time.
const MAX_INVITES: usize = 32;

/// What an invite allows.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteGrant {
    pub secret: String,
    /// Keyboard, mouse and gamepads; without it the guest only watches.
    pub control: bool,
    pub expires: Instant,
}

#[derive(Default)]
pub struct Invites {
    live: Mutex<HashMap<String, InviteGrant>>,
}

/// A fresh secret, as text that is safe in a link.
pub fn new_secret() -> String {
    let mut bytes = [0u8; SECRET_BYTES];
    rand::rng().fill_bytes(&mut bytes);
    URL_SAFE_NO_PAD.encode(bytes)
}

impl Invites {
    /// Remember an invite under the id the server gave it. Returns false if
    /// this host already has as many invites as it may.
    pub fn add(&self, id: &str, control: bool, valid_for: Duration) -> Option<InviteGrant> {
        let mut live = self.live.lock().unwrap();
        let now = Instant::now();
        live.retain(|_, g| g.expires > now);
        if live.len() >= MAX_INVITES {
            return None;
        }
        let grant = InviteGrant { secret: new_secret(), control, expires: now + valid_for };
        live.insert(id.to_string(), grant.clone());
        Some(grant)
    }

    /// The invite with this id, unless it has expired or been withdrawn.
    pub fn get(&self, id: &str) -> Option<InviteGrant> {
        self.live.lock().unwrap().get(id).filter(|g| g.expires > Instant::now()).cloned()
    }

    /// Withdraw an invite. Returns whether it existed.
    pub fn revoke(&self, id: &str) -> bool {
        self.live.lock().unwrap().remove(id).is_some()
    }

    /// Ids of the invites still valid, with what they allow and how long
    /// they have left.
    pub fn list(&self) -> Vec<(String, bool, Duration)> {
        let now = Instant::now();
        let live = self.live.lock().unwrap();
        let mut all: Vec<_> = live
            .iter()
            .filter(|(_, g)| g.expires > now)
            .map(|(id, g)| (id.clone(), g.control, g.expires - now))
            .collect();
        all.sort();
        all
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn invites_expire_and_can_be_withdrawn() {
        let invites = Invites::default();
        let grant = invites.add("a", true, Duration::from_secs(60)).unwrap();
        assert_eq!(invites.get("a"), Some(grant));
        assert_eq!(invites.get("b"), None);

        invites.add("gone", false, Duration::ZERO).unwrap();
        assert_eq!(invites.get("gone"), None, "an expired invite lets nobody in");
        assert_eq!(invites.list().len(), 1);

        assert!(invites.revoke("a"));
        assert!(!invites.revoke("a"));
        assert_eq!(invites.get("a"), None);
    }

    #[test]
    fn secrets_are_long_random_and_link_safe() {
        let (a, b) = (new_secret(), new_secret());
        assert_ne!(a, b);
        assert!(a.len() >= 22);
        assert!(a.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'));
    }

    #[test]
    fn a_host_keeps_a_bounded_number_of_invites() {
        let invites = Invites::default();
        for i in 0..MAX_INVITES {
            assert!(invites.add(&i.to_string(), true, Duration::from_secs(60)).is_some());
        }
        assert!(invites.add("one too many", true, Duration::from_secs(60)).is_none());
    }
}
