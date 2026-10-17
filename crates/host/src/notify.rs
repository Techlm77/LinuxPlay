//! Desktop notifications (org.freedesktop.Notifications) with buttons.
//!
//! Used for the pairing PIN and for approval requests. Unlike `notify-send`,
//! talking to the server directly lets the host mark the PIN transient (kept
//! out of notification history), close it as soon as pairing ends, and hear
//! which button was pressed.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use futures_util::StreamExt;
use tracing::debug;
use zbus::zvariant::Value;

const SERVICE: &str = "org.freedesktop.Notifications";
const PATH: &str = "/org/freedesktop/Notifications";
const APP_NAME: &str = "LinuxPlay";
const ICON: &str = "io.github.techlm77.LinuxPlay";

/// Why a notification went away, as the server reports it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    /// A button was pressed (its key).
    Action(String),
    /// The user dismissed it.
    Dismissed,
}

type Handler = Box<dyn FnOnce(Outcome) + Send>;

pub struct Notifier {
    proxy: zbus::Proxy<'static>,
    handlers: Arc<Mutex<HashMap<u32, Handler>>>,
    /// Informational notes still on screen. Some servers (GNOME) show one
    /// banner at a time and keep it while the user is idle, so these are
    /// closed before an urgent note, which would otherwise queue behind them.
    info: Mutex<Vec<u32>>,
}

/// What to show.
pub struct Note<'a> {
    pub summary: &'a str,
    pub body: &'a str,
    /// `(key, label)` pairs, shown as buttons.
    pub actions: &'a [(&'a str, &'a str)],
    /// Stays until acted on or closed, and isn't kept in the history.
    pub urgent: bool,
    pub transient: bool,
}

impl Notifier {
    /// Connect to the session bus. `None` if there is no notification server.
    pub async fn connect() -> Option<Arc<Self>> {
        let conn = zbus::Connection::session().await.inspect_err(|e| debug!("no session bus: {e}")).ok()?;
        let proxy = zbus::Proxy::new(&conn, SERVICE, PATH, SERVICE)
            .await
            .inspect_err(|e| debug!("no notification service: {e}"))
            .ok()?;
        let handlers: Arc<Mutex<HashMap<u32, Handler>>> = Arc::default();
        let mut actions = proxy.receive_signal("ActionInvoked").await.ok()?;
        let mut closed = proxy.receive_signal("NotificationClosed").await.ok()?;
        let h = handlers.clone();
        tokio::spawn(async move {
            loop {
                let (id, outcome) = tokio::select! {
                    Some(m) = actions.next() => match m.body().deserialize::<(u32, String)>() {
                        Ok((id, key)) => (id, Outcome::Action(key)),
                        Err(_) => continue,
                    },
                    Some(m) = closed.next() => match m.body().deserialize::<(u32, u32)>() {
                        // 2 = dismissed by the user; expiry (1) and our own
                        // close (3) aren't answers.
                        Ok((id, 2)) => (id, Outcome::Dismissed),
                        Ok(_) | Err(_) => continue,
                    },
                    else => break,
                };
                let handler = h.lock().unwrap().remove(&id);
                if let Some(f) = handler {
                    f(outcome);
                }
            }
        });
        Some(Arc::new(Self { proxy, handlers, info: Mutex::default() }))
    }

    /// Show `note`; `on_outcome` runs once if a button is pressed or it is
    /// dismissed. Returns the id to [`close`](Self::close) it with.
    pub async fn show(&self, note: Note<'_>, on_outcome: Option<Handler>) -> Option<u32> {
        let mut actions = Vec::new();
        for (key, label) in note.actions {
            actions.push(key.to_string());
            actions.push(label.to_string());
        }
        let mut hints: HashMap<&str, Value> = HashMap::new();
        hints.insert("desktop-entry", Value::from(ICON));
        if note.urgent {
            hints.insert("urgency", Value::U8(2));
            hints.insert("resident", Value::Bool(true));
        }
        if note.transient {
            hints.insert("transient", Value::Bool(true));
        }
        if note.urgent {
            let stale: Vec<u32> = std::mem::take(&mut *self.info.lock().unwrap());
            for id in stale {
                let _: Result<(), _> = self.proxy.call("CloseNotification", &(id,)).await;
            }
        }
        let timeout: i32 = if note.urgent { 0 } else { -1 };
        let id: u32 = match self
            .proxy
            .call("Notify", &(APP_NAME, 0u32, ICON, note.summary, note.body, actions, hints, timeout))
            .await
        {
            Ok(id) => id,
            Err(e) => {
                debug!("notification failed: {e}");
                return None;
            }
        };
        if let Some(f) = on_outcome {
            self.handlers.lock().unwrap().insert(id, f);
        } else if !note.urgent {
            self.info.lock().unwrap().push(id);
        }
        Some(id)
    }

    pub async fn close(&self, id: u32) {
        self.handlers.lock().unwrap().remove(&id);
        let _: Result<(), _> = self.proxy.call("CloseNotification", &(id,)).await;
    }
}
