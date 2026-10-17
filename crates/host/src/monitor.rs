//! Enumerating the displays this host can share.

use anyhow::{Context, Result};
use linuxplay_proto::signal::MonitorInfo;
use x11rb::connection::Connection;
use x11rb::protocol::randr::ConnectionExt as _;
use x11rb::protocol::xproto::ConnectionExt as _;

/// The monitors of an X11 screen, via RandR. Falls back to one entry covering
/// the whole root window if RandR has nothing useful.
pub fn enumerate_x11(display: Option<&str>) -> Result<Vec<MonitorInfo>> {
    let (conn, screen_num) = x11rb::connect(display).context("connecting to the X server")?;
    let root = conn.setup().roots[screen_num].root;
    let reply = conn.randr_get_monitors(root, true)?.reply()?;

    let mut monitors = Vec::new();
    for (index, m) in reply.monitors.iter().enumerate() {
        let name = conn
            .get_atom_name(m.name)
            .ok()
            .and_then(|c| c.reply().ok())
            .map(|r| String::from_utf8_lossy(&r.name).into_owned())
            .unwrap_or_else(|| format!("Display {}", index + 1));
        monitors.push(MonitorInfo {
            index: index as u32,
            name,
            width: m.width as u32,
            height: m.height as u32,
            x: m.x as i32,
            y: m.y as i32,
            primary: m.primary,
        });
    }

    if monitors.is_empty() {
        let s = &conn.setup().roots[screen_num];
        monitors.push(MonitorInfo {
            index: 0,
            name: "Screen".into(),
            width: s.width_in_pixels as u32,
            height: s.height_in_pixels as u32,
            x: 0,
            y: 0,
            primary: true,
        });
    }
    Ok(monitors)
}
