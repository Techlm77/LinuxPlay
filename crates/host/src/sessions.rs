//! Every authenticated connection, from `Welcome` until it ends, with what it
//! is allowed to do. Indexed so the host can end sessions the moment a device
//! is revoked or the user stops sharing.

use std::collections::{BTreeSet, HashMap};
use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use linuxplay_net::tls::Fingerprint;
use tokio::sync::watch;

/// What one session may do, decided by the host (never by the client).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    /// Keyboard and mouse. Without it every input event is dropped.
    pub control: bool,
    /// Desktop audio.
    pub audio: bool,
    /// Displays it may stream; `None` means all that are shared.
    pub displays: Option<BTreeSet<u32>>,
}

impl Grant {
    pub fn allows_display(&self, index: u32) -> bool {
        self.displays.as_ref().is_none_or(|d| d.contains(&index))
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// Waiting for the person at the host to decide.
    Waiting,
    Approved(Grant),
}

/// A snapshot of one session, for the tray and `linuxplay-host sessions`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Info {
    pub id: u64,
    pub name: String,
    pub ip: IpAddr,
    pub fingerprint: String,
    pub state: State,
    pub display: Option<u32>,
}

struct Entry {
    fp: Fingerprint,
    name: String,
    ip: IpAddr,
    state: State,
    display: Option<u32>,
    kill: watch::Sender<bool>,
    active: Arc<AtomicBool>,
}

#[derive(Default)]
pub struct Sessions {
    entries: Mutex<HashMap<u64, Entry>>,
    next: AtomicU64,
}

/// One registered session; dropping it removes the session.
pub struct Ticket {
    pub id: u64,
    owner: Arc<Sessions>,
    kill: watch::Receiver<bool>,
    /// Cleared the moment the session is ended from the host side.
    pub active: Arc<AtomicBool>,
}

impl Ticket {
    /// Resolves when the host ends this session.
    pub fn killed(&self) -> impl std::future::Future<Output = ()> + Send + 'static {
        let mut rx = self.kill.clone();
        async move {
            let _ = rx.wait_for(|k| *k).await;
        }
    }

    pub fn approve(&self, grant: Grant) {
        if let Some(e) = self.owner.entries.lock().unwrap().get_mut(&self.id) {
            e.state = State::Approved(grant);
        }
    }

    pub fn set_display(&self, index: u32) {
        if let Some(e) = self.owner.entries.lock().unwrap().get_mut(&self.id) {
            e.display = Some(index);
        }
    }
}

impl Drop for Ticket {
    fn drop(&mut self) {
        self.active.store(false, Ordering::Release);
        self.owner.entries.lock().unwrap().remove(&self.id);
    }
}

impl Sessions {
    pub fn register(self: &Arc<Self>, fp: Fingerprint, name: &str, ip: IpAddr) -> Ticket {
        let id = self.next.fetch_add(1, Ordering::Relaxed) + 1;
        let (kill, rx) = watch::channel(false);
        let active = Arc::new(AtomicBool::new(true));
        let entry = Entry { fp, name: name.to_string(), ip, state: State::Waiting, display: None, kill, active: active.clone() };
        self.entries.lock().unwrap().insert(id, entry);
        Ticket { id, owner: self.clone(), kill: rx, active }
    }

    /// The grant of another approved session of the same device, so a
    /// second display window joins the visit already approved instead of
    /// asking again.
    pub fn grant_of(&self, fp: &Fingerprint, except: u64) -> Option<Grant> {
        self.entries.lock().unwrap().iter().find_map(|(id, e)| match &e.state {
            State::Approved(g) if *id != except && e.fp == *fp => Some(g.clone()),
            _ => None,
        })
    }

    /// End every session matching `pred`; returns how many.
    pub fn kill_where(&self, pred: impl Fn(u64, &Fingerprint) -> bool) -> usize {
        let entries = self.entries.lock().unwrap();
        let mut n = 0;
        for (id, e) in entries.iter() {
            if pred(*id, &e.fp) && e.active.swap(false, Ordering::AcqRel) {
                let _ = e.kill.send(true);
                n += 1;
            }
        }
        n
    }

    pub fn kill(&self, id: u64) -> bool {
        self.kill_where(|i, _| i == id) > 0
    }

    pub fn kill_all(&self) -> usize {
        self.kill_where(|_, _| true)
    }

    pub fn is_empty(&self) -> bool {
        self.entries.lock().unwrap().is_empty()
    }

    pub fn snapshot(&self) -> Vec<Info> {
        let mut v: Vec<Info> = self
            .entries
            .lock()
            .unwrap()
            .iter()
            .map(|(id, e)| Info {
                id: *id,
                name: e.name.clone(),
                ip: e.ip,
                fingerprint: hex::encode(e.fp),
                state: e.state.clone(),
                display: e.display,
            })
            .collect();
        v.sort_by_key(|i| i.id);
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn grant(control: bool) -> Grant {
        Grant { control, audio: true, displays: None }
    }

    #[tokio::test]
    async fn killing_a_device_ends_only_its_sessions() {
        let s = Arc::new(Sessions::default());
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let a = s.register([1; 32], "a", ip);
        let b = s.register([2; 32], "b", ip);
        assert_eq!(s.kill_where(|_, fp| *fp == [1; 32]), 1);
        assert!(!a.active.load(Ordering::Acquire));
        assert!(b.active.load(Ordering::Acquire));
        tokio::time::timeout(std::time::Duration::from_secs(1), a.killed()).await.unwrap();
        drop(a);
        assert_eq!(s.snapshot().len(), 1);
    }

    #[test]
    fn a_second_window_reuses_the_visits_grant() {
        let s = Arc::new(Sessions::default());
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let first = s.register([1; 32], "a", ip);
        let second = s.register([1; 32], "a", ip);
        assert_eq!(s.grant_of(&[1; 32], second.id), None, "nothing approved yet");
        first.approve(grant(false));
        assert_eq!(s.grant_of(&[1; 32], second.id), Some(grant(false)));
        assert_eq!(s.grant_of(&[2; 32], second.id), None, "other devices get nothing");
        drop(first);
        assert_eq!(s.grant_of(&[1; 32], second.id), None, "the visit ended with its last window");
    }

    #[test]
    fn display_grants() {
        let g = Grant { control: true, audio: false, displays: Some([0].into()) };
        assert!(g.allows_display(0));
        assert!(!g.allows_display(1));
        assert!(grant(true).allows_display(7));
    }
}
