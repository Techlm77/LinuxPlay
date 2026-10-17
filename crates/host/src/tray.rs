//! A system-tray icon so it's obvious the host is running and who is
//! connected, with controls to approve, disconnect and stop sharing.
//!
//! Uses the StatusNotifierItem protocol (KDE, and other panels with the
//! AppIndicator/SNI support). Where no tray host exists (e.g. plain GNOME) the
//! item simply doesn't appear; notifications and `linuxplay-host sessions`
//! offer the same controls.

use std::sync::Arc;

use ksni::menu::{StandardItem, SubMenu};
use ksni::{Icon, MenuItem, Status, ToolTip, Tray};
use tokio::sync::{Notify, mpsc};

use crate::approval::Decision;

/// What the menu shows; rebuilt by the host whenever it changes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrayState {
    /// `(who, PIN)` for pairings in progress.
    pub pins: Vec<(String, String)>,
    /// Connections waiting for approval.
    pub waiting: Vec<(u64, String)>,
    /// Connected sessions, with a description.
    pub sessions: Vec<(u64, String)>,
}

/// Something the user chose in the menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrayCmd {
    Decide(u64, Decision),
    Disconnect(u64),
    StopSharing,
}

pub struct HostTray {
    pub host_name: String,
    /// e.g. "Local network only · 192.168.8.184:47800"
    pub detail: String,
    pub unattended: bool,
    pub state: TrayState,
    /// Fired when the user picks Quit.
    pub quit: Arc<Notify>,
    pub cmds: mpsc::Sender<TrayCmd>,
}

/// Build an SNI icon from raw RGBA pixels, reordered to the ARGB32
/// network-byte-order the protocol wants.
fn icon(size: i32, rgba: &[u8]) -> Icon {
    let mut data = Vec::with_capacity(rgba.len());
    for px in rgba.chunks_exact(4) {
        data.extend_from_slice(&[px[3], px[0], px[1], px[2]]);
    }
    Icon { width: size, height: size, data }
}

impl HostTray {
    fn status_line(&self) -> String {
        let n = self.state.sessions.len();
        let mut s = match n {
            0 => "Nobody connected".to_string(),
            1 => "1 connection".to_string(),
            n => format!("{n} connections"),
        };
        if !self.state.waiting.is_empty() {
            s.push_str(&format!(" · {} waiting for approval", self.state.waiting.len()));
        }
        s
    }

    fn send(&self, cmd: TrayCmd) {
        let _ = self.cmds.try_send(cmd);
    }
}

fn info<T>(label: String) -> MenuItem<T> {
    MenuItem::Standard(StandardItem { label, enabled: false, ..Default::default() })
}

fn action(label: &str, icon: &str, cmd: TrayCmd) -> MenuItem<HostTray> {
    MenuItem::Standard(StandardItem {
        label: label.into(),
        icon_name: icon.into(),
        activate: Box::new(move |t: &mut HostTray| t.send(cmd)),
        ..Default::default()
    })
}

impl Tray for HostTray {
    fn id(&self) -> String {
        "io.github.techlm77.LinuxPlay".into()
    }

    fn title(&self) -> String {
        if self.unattended { "LinuxPlay Host (unattended)".into() } else { "LinuxPlay Host".into() }
    }

    fn icon_name(&self) -> String {
        // A standard name so panels that ignore pixmaps still show something.
        "video-display".into()
    }

    fn icon_pixmap(&self) -> Vec<Icon> {
        // Our own icon, embedded as pixels so it renders regardless of the
        // icon theme or panel cache (a bare icon name often shows blank).
        vec![icon(32, include_bytes!("icon32.rgba")), icon(48, include_bytes!("icon48.rgba"))]
    }

    fn status(&self) -> Status {
        if self.state.waiting.is_empty() && self.state.pins.is_empty() { Status::Active } else { Status::NeedsAttention }
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: self.icon_name(),
            icon_pixmap: Vec::new(),
            title: self.title(),
            description: format!("Sharing \u{201c}{}\u{201d} — {}", self.host_name, self.status_line()),
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let mut items = vec![
            info(format!("Sharing \u{201c}{}\u{201d}", self.host_name)),
            info(self.detail.clone()),
            info(if self.unattended {
                "Unattended: paired devices connect without asking".into()
            } else {
                "Each connection must be approved here".into()
            }),
            info(self.status_line()),
        ];

        if !self.state.pins.is_empty() || !self.state.waiting.is_empty() {
            items.push(MenuItem::Separator);
        }
        for (who, pin) in &self.state.pins {
            items.push(info(format!("Pairing {who}: PIN {pin}")));
        }
        for (id, who) in &self.state.waiting {
            let id = *id;
            items.push(MenuItem::SubMenu(SubMenu {
                label: format!("{who} wants to connect"),
                icon_name: "dialog-question".into(),
                submenu: vec![
                    action("View only", "view-visible", TrayCmd::Decide(id, Decision::ViewOnly)),
                    action("Allow control", "input-mouse", TrayCmd::Decide(id, Decision::Control)),
                    action("Deny", "process-stop", TrayCmd::Decide(id, Decision::Deny)),
                ],
                ..Default::default()
            }));
        }

        if !self.state.sessions.is_empty() {
            items.push(MenuItem::Separator);
            for (id, label) in &self.state.sessions {
                items.push(action(&format!("Disconnect {label}"), "network-offline", TrayCmd::Disconnect(*id)));
            }
            items.push(action("Stop sharing (disconnect everyone)", "media-playback-stop", TrayCmd::StopSharing));
        }

        items.push(MenuItem::Separator);
        items.push(MenuItem::Standard(StandardItem {
            label: "Quit".into(),
            icon_name: "application-exit".into(),
            activate: Box::new(|this: &mut HostTray| this.quit.notify_one()),
            ..Default::default()
        }));
        items
    }
}
