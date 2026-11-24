//! What the host shows on its own desktop: the tray icon and the
//! notifications that say it is running and who connected.

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use linuxplay_net::tls;
use tokio::sync::{Notify, mpsc};
use tracing::{debug, info};

use super::Host;
use crate::notify::Note;
use crate::sessions::State;
use crate::tray::{HostTray, TrayCmd, TrayState};

impl Host {
    pub(super) fn tray_state(&self) -> TrayState {
        let sessions = self
            .sessions
            .snapshot()
            .into_iter()
            .filter_map(|s| match s.state {
                State::Approved(g) => Some((
                    s.id,
                    format!(
                        "{} ({}){}{}",
                        s.name,
                        s.ip,
                        s.display.map(|d| format!(" · display {d}")).unwrap_or_default(),
                        if g.control { "" } else { " · view only" }
                    ),
                )),
                State::Waiting => None,
            })
            .collect();
        TrayState { pins: self.pins.list(), waiting: self.approver.waiting(), sessions }
    }

    /// Show a tray icon (where a StatusNotifier host exists), keep it fresh,
    /// and act on its menu. Harmless where no tray is available.
    pub(super) async fn start_tray(self: &Arc<Self>, quit: Arc<Notify>, shown: &str) {
        use ksni::TrayMethods;
        let fp = tls::fingerprint_display(&self.identity.fingerprint);
        let short: String = fp.chars().take(14).collect();
        let (cmds, mut cmd_rx) = mpsc::channel(16);
        let tray = HostTray {
            host_name: self.cfg.name.clone(),
            detail: format!("{} · {shown} · {short}…", self.cfg.exposure),
            unattended: self.cfg.unattended,
            state: TrayState::default(),
            quit,
            cmds,
        };
        let handle = match tokio::time::timeout(Duration::from_secs(3), tray.spawn()).await {
            Ok(Ok(handle)) => handle,
            Ok(Err(e)) => {
                debug!("no system tray: {e}");
                return;
            }
            Err(_) => {
                debug!("system tray setup timed out");
                return;
            }
        };
        let this = Arc::downgrade(self);
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                let Some(host) = this.upgrade() else { break };
                match cmd {
                    TrayCmd::Decide(id, d) => drop(host.approver.resolve(id, d)),
                    TrayCmd::Disconnect(id) => drop(host.sessions.kill(id)),
                    TrayCmd::StopSharing => {
                        let n = host.sessions.kill_all();
                        info!("sharing stopped from the tray; ended {n} session(s)");
                    }
                }
            }
        });
        let this = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut last = TrayState::default();
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let Some(host) = this.upgrade() else { break };
                if handle.is_closed() {
                    break;
                }
                let state = host.tray_state();
                if state != last {
                    last = state.clone();
                    handle.update(|t| t.state = state).await;
                }
            }
        });
    }

    /// Let the user know the host is up, even with no terminal or tray.
    pub(super) async fn announce_started(&self, shown: &str) {
        let Some(n) = &self.notifier else { return };
        let body = format!(
            "Listening on {shown} ({}). {}",
            self.cfg.exposure,
            if self.cfg.unattended {
                "Unattended: paired devices connect without asking."
            } else {
                "You'll be asked before anyone connects."
            }
        );
        let note = Note { summary: "LinuxPlay Host is running", body: &body, actions: &[], urgent: false, transient: true };
        n.show(note, None).await;
    }

    /// The unattended-mode indicator: say who just connected.
    /// Tell the person at this computer that the internet can reach it.
    pub(super) async fn announce_exposed(&self, ip: IpAddr) {
        let Some(n) = &self.notifier else { return };
        let body = format!("{ip} reached this computer from the internet. {}", linuxplay_net::netif::EXPOSED_ADVICE);
        let note = Note { summary: "LinuxPlay: open to the internet", body: &body, actions: &[], urgent: true, transient: false };
        n.show(note, None).await;
    }

    pub(super) async fn announce_connected(&self, name: &str, ip: IpAddr) {
        info!("{name} ({ip}) connected without approval (unattended)");
        let Some(n) = &self.notifier else { return };
        let body = format!("{name} ({ip}) connected (unattended access). Disconnect from the tray or with `linuxplay-host disconnect all`.");
        let note = Note { summary: "LinuxPlay: device connected", body: &body, actions: &[], urgent: false, transient: false };
        n.show(note, None).await;
    }
}
