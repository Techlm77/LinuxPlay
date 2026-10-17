//! Where the picture comes from, and the matching way to inject input.

use std::any::Any;

use anyhow::{Result, bail};
use clap::ValueEnum;
use linuxplay_proto::signal::MonitorInfo;
use tracing::info;

use crate::input::{self, InputTx};
use crate::monitor;
use crate::portal::PortalStream;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CaptureKind {
    /// ximagesrc + XTEST on an X11 session.
    X11,
    /// ScreenCast + RemoteDesktop portals on a Wayland session.
    Portal,
}

impl CaptureKind {
    pub fn detect() -> Result<Self> {
        let wayland = std::env::var_os("WAYLAND_DISPLAY").is_some()
            || std::env::var("XDG_SESSION_TYPE").is_ok_and(|t| t == "wayland");
        if wayland {
            Ok(Self::Portal)
        } else if std::env::var_os("DISPLAY").is_some() {
            Ok(Self::X11)
        } else {
            bail!("no graphical session found (neither WAYLAND_DISPLAY nor DISPLAY is set)")
        }
    }
}

/// An open capture of one monitor: a gst-launch fragment producing raw video,
/// its size, and where to send input. Dropping it ends the capture.
pub struct Capture {
    pub source: String,
    pub width: u32,
    pub height: u32,
    pub input: InputTx,
    _keep: Box<dyn Any + Send>,
}

/// The monitors the X11 screen exposes.
pub fn monitors_x11() -> Result<Vec<MonitorInfo>> {
    monitor::enumerate_x11(std::env::var("DISPLAY").ok().as_deref())
}

/// Capture one X11 monitor by cropping `ximagesrc` to its bounds.
pub fn open_x11(fps: u32, mon: &MonitorInfo) -> Result<Capture> {
    let display = std::env::var("DISPLAY").ok();
    let area = (mon.x, mon.y, mon.width, mon.height);
    let injector = input::x11::X11Injector::connect(display.as_deref(), area)?;
    let display_prop = display.map(|d| format!(" display-name={d}")).unwrap_or_default();
    let (endx, endy) = (mon.x + mon.width as i32 - 1, mon.y + mon.height as i32 - 1);
    let source = format!(
        "ximagesrc{display_prop} use-damage=false show-pointer=true \
         startx={} starty={} endx={endx} endy={endy} ! video/x-raw,framerate={fps}/1",
        mon.x, mon.y,
    );
    info!("capturing X11 monitor {} ({}x{} at {},{})", mon.name, mon.width, mon.height, mon.x, mon.y);
    Ok(Capture {
        source,
        width: mon.width,
        height: mon.height,
        input: input::spawn(injector),
        _keep: Box::new(()),
    })
}

/// Wrap a portal stream as a [`Capture`].
pub fn from_portal(stream: PortalStream) -> Capture {
    Capture {
        source: stream.source,
        width: stream.width,
        height: stream.height,
        input: stream.input,
        _keep: Box::new(stream._keep),
    }
}
