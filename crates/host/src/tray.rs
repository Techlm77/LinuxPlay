//! A system-tray icon so it's obvious the host is running and sharing.
//!
//! Uses the StatusNotifierItem protocol (KDE, and other panels with the
//! AppIndicator/SNI support). Where no tray host exists (e.g. plain GNOME) the
//! item simply doesn't appear; the host still logs and sends notifications.

use std::sync::Arc;

use ksni::menu::StandardItem;
use ksni::{Icon, MenuItem, Status, ToolTip, Tray};
use tokio::sync::Notify;

pub struct HostTray {
    pub host_name: String,
    /// e.g. "Port 47800 · 2EB1 5560 …"
    pub detail: String,
    /// Displays currently being streamed.
    pub clients: usize,
    /// Fired when the user picks Quit.
    pub quit: Arc<Notify>,
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
        match self.clients {
            0 => "Waiting for a connection".into(),
            1 => "1 display connected".into(),
            n => format!("{n} displays connected"),
        }
    }
}

impl Tray for HostTray {
    fn id(&self) -> String {
        "io.github.techlm77.LinuxPlay".into()
    }

    fn title(&self) -> String {
        "LinuxPlay Host".into()
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
        Status::Active
    }

    fn tool_tip(&self) -> ToolTip {
        ToolTip {
            icon_name: self.icon_name(),
            icon_pixmap: Vec::new(),
            title: "LinuxPlay Host".into(),
            description: format!("Sharing \u{201c}{}\u{201d} — {}", self.host_name, self.status_line()),
        }
    }

    fn menu(&self) -> Vec<MenuItem<Self>> {
        let info = |label: String| {
            MenuItem::Standard(StandardItem { label, enabled: false, ..Default::default() })
        };
        vec![
            info(format!("Sharing \u{201c}{}\u{201d}", self.host_name)),
            info(self.status_line()),
            info(self.detail.clone()),
            MenuItem::Separator,
            MenuItem::Standard(StandardItem {
                label: "Quit".into(),
                icon_name: "application-exit".into(),
                activate: Box::new(|this: &mut HostTray| this.quit.notify_one()),
                ..Default::default()
            }),
        ]
    }
}
