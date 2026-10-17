//! Asking the person at the host: "may this device see the screen?", and
//! showing pairing PINs. Both appear as desktop notifications and in the tray
//! menu, and can be answered or read with `linuxplay-host sessions|approve|deny`.
//! The PIN is never written to the log.

use std::collections::HashMap;
use std::io::IsTerminal;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use tokio::sync::oneshot;
use tracing::info;

use crate::notify::{Note, Notifier, Outcome};

/// How long a connection waits for an answer before it is declined.
pub const APPROVAL_TIMEOUT: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Decision {
    /// Video and audio, plus keyboard and mouse.
    Control,
    /// Video and audio only.
    ViewOnly,
    Deny,
}

impl Decision {
    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "control" => Some(Self::Control),
            "view" => Some(Self::ViewOnly),
            "deny" => Some(Self::Deny),
            _ => None,
        }
    }
}

fn close_later(notifier: &Option<Arc<Notifier>>, id: Option<u32>) {
    if let (Some(n), Some(id), Ok(rt)) = (notifier.clone(), id, tokio::runtime::Handle::try_current()) {
        rt.spawn(async move { n.close(id).await });
    }
}

pub struct Approver {
    pending: Mutex<HashMap<u64, (String, oneshot::Sender<Decision>)>>,
    notifier: Option<Arc<Notifier>>,
}

/// Removes an unanswered request (and its notification) however `ask` ends.
struct Asked<'a> {
    approver: &'a Approver,
    id: u64,
    note: Option<u32>,
}

impl Drop for Asked<'_> {
    fn drop(&mut self) {
        self.approver.pending.lock().unwrap().remove(&self.id);
        close_later(&self.approver.notifier, self.note);
    }
}

impl Approver {
    pub fn new(notifier: Option<Arc<Notifier>>) -> Arc<Self> {
        Arc::new(Self { pending: Mutex::default(), notifier })
    }

    /// Ask about session `id`; declines after [`APPROVAL_TIMEOUT`].
    pub async fn ask(self: &Arc<Self>, id: u64, name: &str, ip: IpAddr, device: &str) -> Decision {
        let (tx, rx) = oneshot::channel();
        let label = format!("{name} ({ip})");
        self.pending.lock().unwrap().insert(id, (label.clone(), tx));
        let mut asked = Asked { approver: self, id, note: None };
        info!("{label} is asking to connect; answer in the notification, the tray, or with `linuxplay-host approve {id}`");

        if let Some(n) = &self.notifier {
            let body = format!("{label} wants to see this screen.\nDevice {device}");
            let me = Arc::downgrade(self);
            let on_outcome = Box::new(move |o: Outcome| {
                let d = match o {
                    Outcome::Action(k) => Decision::parse(&k).unwrap_or(Decision::Deny),
                    Outcome::Dismissed => Decision::Deny,
                };
                if let Some(me) = me.upgrade() {
                    me.resolve(id, d);
                }
            });
            let note = Note {
                summary: "Allow a LinuxPlay connection?",
                body: &body,
                actions: &[("view", "View only"), ("control", "Allow control"), ("deny", "Deny")],
                urgent: true,
                transient: false,
            };
            asked.note = n.show(note, Some(on_outcome)).await;
        }
        let decision = match tokio::time::timeout(APPROVAL_TIMEOUT, rx).await {
            Ok(Ok(d)) => d,
            _ => Decision::Deny,
        };
        drop(asked);
        info!("{label}: {decision:?}");
        decision
    }

    /// Answer a waiting request. False if there is no such request.
    pub fn resolve(&self, id: u64, decision: Decision) -> bool {
        match self.pending.lock().unwrap().remove(&id) {
            Some((_, tx)) => tx.send(decision).is_ok(),
            None => false,
        }
    }

    /// Requests waiting for an answer.
    pub fn waiting(&self) -> Vec<(u64, String)> {
        let mut v: Vec<_> = self.pending.lock().unwrap().iter().map(|(id, (l, _))| (*id, l.clone())).collect();
        v.sort();
        v
    }
}

/// The pairing PINs currently on show.
pub struct PinBoard {
    pins: Mutex<HashMap<u64, (String, String)>>,
    next: AtomicU64,
    notifier: Option<Arc<Notifier>>,
}

/// Takes the PIN off every screen when pairing ends.
pub struct PinShown {
    board: Arc<PinBoard>,
    id: u64,
    note: Option<u32>,
}

impl Drop for PinShown {
    fn drop(&mut self) {
        self.board.pins.lock().unwrap().remove(&self.id);
        close_later(&self.board.notifier, self.note);
    }
}

impl PinBoard {
    pub fn new(notifier: Option<Arc<Notifier>>) -> Arc<Self> {
        Arc::new(Self { pins: Mutex::default(), next: AtomicU64::new(1), notifier })
    }

    /// Show `pin` to whoever is at the host: a transient notification (kept
    /// out of history), the tray, `linuxplay-host sessions`, and the terminal
    /// if there is one. Never the log.
    pub async fn show(self: &Arc<Self>, name: &str, ip: IpAddr, pin: &str) -> PinShown {
        let id = self.next.fetch_add(1, Ordering::Relaxed);
        let label = format!("{name} ({ip})");
        self.pins.lock().unwrap().insert(id, (label.clone(), pin.to_string()));
        info!("pairing request from {label}; the PIN is shown on this screen");
        if std::io::stderr().is_terminal() {
            eprintln!("\n    Pairing request from {label}\n    PIN: {pin}\n");
        }
        let mut shown = PinShown { board: self.clone(), id, note: None };
        if let Some(n) = &self.notifier {
            let body = format!("{label} wants to pair.\nPIN: {pin}");
            let note = Note { summary: "LinuxPlay pairing request", body: &body, actions: &[], urgent: true, transient: true };
            shown.note = n.show(note, None).await;
        }
        shown
    }

    /// `(who, pin)` for every pairing in progress.
    pub fn list(&self) -> Vec<(String, String)> {
        let mut v: Vec<_> = self.pins.lock().unwrap().values().cloned().collect();
        v.sort();
        v
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn an_answer_reaches_the_waiting_connection() {
        let a = Approver::new(None);
        let ip: IpAddr = "10.0.0.1".parse().unwrap();
        let ask = {
            let a = a.clone();
            tokio::spawn(async move { a.ask(7, "laptop", ip, "ABCD").await })
        };
        while a.waiting().is_empty() {
            tokio::task::yield_now().await;
        }
        assert_eq!(a.waiting(), vec![(7, "laptop (10.0.0.1)".to_string())]);
        assert!(!a.resolve(8, Decision::Control), "no such request");
        assert!(a.resolve(7, Decision::ViewOnly));
        assert_eq!(ask.await.unwrap(), Decision::ViewOnly);
        assert!(a.waiting().is_empty());
    }

    #[tokio::test(start_paused = true)]
    async fn silence_is_a_no() {
        let a = Approver::new(None);
        let d = a.ask(1, "laptop", "10.0.0.1".parse().unwrap(), "ABCD").await;
        assert_eq!(d, Decision::Deny);
        assert!(a.waiting().is_empty());
    }

    #[tokio::test]
    async fn the_pin_leaves_the_board_when_pairing_ends() {
        let b = PinBoard::new(None);
        let shown = b.show("laptop", "10.0.0.1".parse().unwrap(), "123456").await;
        assert_eq!(b.list(), vec![("laptop (10.0.0.1)".to_string(), "123456".to_string())]);
        drop(shown);
        assert!(b.list().is_empty());
    }
}
