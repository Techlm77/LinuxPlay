//! Where the picture comes from, and the matching way to inject input.

use std::any::Any;
use std::sync::{Arc, Mutex};

use anyhow::{Result, bail};
use clap::ValueEnum;
use linuxplay_proto::input::InputEvent;
use linuxplay_proto::signal::MonitorInfo;
use tracing::info;

use crate::input::{self, Feedback, InputTx};
use crate::monitor;
use crate::portal::PortalStream;

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum CaptureKind {
    /// ximagesrc + XTEST on an X11 session.
    X11,
    /// ScreenCast + RemoteDesktop portals on a Wayland session.
    Portal,
    /// A generated test picture whose input goes nowhere: for trying a
    /// connection without sharing a screen, and for automated tests.
    #[value(hide = true)]
    Test,
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
    /// Where to send input; `None` for a view-only session.
    pub input: Option<InputTx>,
    /// Where rumble from the session's virtual gamepads goes.
    pub feedback: Feedback,
    _keep: Box<dyn Any + Send>,
}

/// The monitors the X11 screen exposes.
pub fn monitors_x11() -> Result<Vec<MonitorInfo>> {
    monitor::enumerate_x11(std::env::var("DISPLAY").ok().as_deref())
}

/// Capture one X11 monitor by cropping `ximagesrc` to its bounds. XTEST is
/// only opened when the session may `control` the desktop.
pub fn open_x11(fps: u32, mon: &MonitorInfo, control: bool) -> Result<Capture> {
    let display = std::env::var("DISPLAY").ok();
    let area = (mon.x, mon.y, mon.width, mon.height);
    let feedback = Feedback::default();
    let input = if control {
        Some(input::spawn(input::x11::X11Injector::connect(display.as_deref(), area)?, feedback.clone()))
    } else {
        None
    };
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
        input,
        feedback,
        _keep: Box::new(()),
    })
}

/// What the built-in test capture shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TestPicture {
    pub width: u32,
    pub height: u32,
    pub pattern: TestPattern,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
pub enum TestPattern {
    /// A ball bouncing on black: almost free to encode.
    Ball,
    /// Colour bars.
    Bars,
    /// Noise that changes every frame: the hardest picture there is, for
    /// trying an encoder and a link at full bitrate.
    Snow,
}

impl TestPattern {
    fn videotestsrc(self) -> &'static str {
        match self {
            Self::Ball => "ball",
            Self::Bars => "smpte",
            Self::Snow => "snow",
        }
    }
}

impl Default for TestPicture {
    fn default() -> Self {
        Self { width: 1280, height: 720, pattern: TestPattern::Ball }
    }
}

/// The displays of the built-in test capture: two, side by side.
pub fn monitors_test(picture: TestPicture) -> Vec<MonitorInfo> {
    (0..2)
        .map(|i| MonitorInfo {
            index: i,
            name: format!("Test {}", i + 1),
            width: picture.width,
            height: picture.height,
            x: (picture.width * i) as i32,
            y: 0,
            primary: i == 0,
        })
        .collect()
}

/// A moving test pattern; input goes to `log`.
pub fn open_test(
    fps: u32,
    mon: &MonitorInfo,
    pattern: TestPattern,
    control: bool,
    log: Arc<Mutex<Vec<InputEvent>>>,
) -> Capture {
    let source = format!(
        "videotestsrc is-live=true pattern={} ! video/x-raw,width={},height={},framerate={fps}/1",
        pattern.videotestsrc(),
        mon.width,
        mon.height
    );
    let feedback = Feedback::default();
    Capture {
        source,
        width: mon.width,
        height: mon.height,
        input: control.then(|| input::spawn(input::Recorder(log), feedback.clone())),
        feedback,
        _keep: Box::new(()),
    }
}

/// Wrap a portal stream as a [`Capture`].
pub fn from_portal(stream: PortalStream) -> Capture {
    Capture {
        source: stream.source,
        width: stream.width,
        height: stream.height,
        input: stream.input,
        feedback: stream.feedback,
        _keep: Box::new(stream._keep),
    }
}
