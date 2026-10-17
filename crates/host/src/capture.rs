//! Where the picture comes from, and the matching way to inject input.

use std::any::Any;

use anyhow::{Result, bail};
use clap::ValueEnum;
use tracing::info;

use crate::input::{self, InputTx};

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

/// An open capture: a gst-launch fragment producing raw video, its size, and
/// where to send input. Dropping it ends the capture session.
pub struct Capture {
    pub source: String,
    pub width: u32,
    pub height: u32,
    pub input: InputTx,
    _keep: Box<dyn Any + Send>,
}

pub async fn open(kind: CaptureKind, fps: u32) -> Result<Capture> {
    match kind {
        CaptureKind::X11 => open_x11(fps),
        CaptureKind::Portal => bail!("portal capture is not implemented yet"),
    }
}

fn open_x11(fps: u32) -> Result<Capture> {
    let display = std::env::var("DISPLAY").ok();
    let (width, height) = input::x11::root_size(display.as_deref())?;
    let injector = input::x11::X11Injector::connect(display.as_deref(), (0, 0, width, height))?;
    let display_prop = display.map(|d| format!(" display-name={d}")).unwrap_or_default();
    let source = format!(
        "ximagesrc{display_prop} use-damage=false show-pointer=true \
         ! video/x-raw,framerate={fps}/1"
    );
    info!("capturing the X11 screen, {width}x{height}");
    Ok(Capture { source, width, height, input: input::spawn(injector), _keep: Box::new(()) })
}
