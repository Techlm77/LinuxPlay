//! Wayland capture and input through the desktop portals.
//!
//! One RemoteDesktop session carries the screen cast and input injection.
//! The user approves which monitors to share in the compositor's dialog; each
//! shared monitor becomes a PipeWire stream the client can pick. Opening a
//! stream hands back a `pipewiresrc` fragment and an input channel bound to
//! that monitor.
//!
//! The session is only created once a client has been approved, and in
//! attended mode it is closed again when the last client leaves, and the
//! grant is never saved: the next client brings up the compositor's dialog
//! again. Only `--unattended` asks the compositor to remember the grant.

use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result, anyhow};
use ashpd::desktop::PersistMode;
use ashpd::desktop::Session;
use ashpd::desktop::remote_desktop::{Axis, DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions, StartOptions};
use ashpd::desktop::screencast::{CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType};
use ashpd::enumflags2::BitFlags;
use linuxplay_net::fsutil;
use linuxplay_proto::input::{InputEvent, MouseButton, POINTER_MAX};
use linuxplay_proto::signal::MonitorInfo;
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::input::{self, InputTx};

/// A monitor shared through the portal.
struct PortalMonitor {
    info: MonitorInfo,
    node: u32,
}

/// The host-wide portal session, created once and shared by every stream.
pub struct PortalHost {
    remote: Arc<RemoteDesktop>,
    screencast: Arc<Screencast>,
    session: Arc<Session<RemoteDesktop>>,
    monitors: Vec<PortalMonitor>,
}

/// One running screen-cast stream for a single monitor.
pub struct PortalStream {
    pub source: String,
    pub width: u32,
    pub height: u32,
    /// `None` for a view-only session: no input task runs at all.
    pub input: Option<InputTx>,
    /// Keeps the PipeWire fd open and the input task running; dropping it
    /// stops the stream (the session itself stays on the [`PortalHost`]).
    pub _keep: StreamKeep,
}

pub struct StreamKeep {
    _fd: OwnedFd,
    _remote: Arc<RemoteDesktop>,
    _session: Arc<Session<RemoteDesktop>>,
}

fn token_path() -> PathBuf {
    linuxplay_net::tls::config_dir("host").join("portal-restore-token")
}

/// Forget the saved grant, so the next session asks the user again.
pub fn remove_token() -> Result<()> {
    fsutil::remove(&token_path())
}

impl PortalHost {
    /// Set up the portal and ask the user which monitors to share. With
    /// `persist` (unattended mode) a saved restore token may let it start
    /// silently and the grant is saved for next time; without it the
    /// compositor dialog always appears and nothing is saved.
    pub async fn create(persist: bool) -> Result<Arc<Self>> {
        let remote = RemoteDesktop::new().await.context("connecting to the RemoteDesktop portal")?;
        let screencast = Screencast::new().await.context("connecting to the ScreenCast portal")?;
        let session = remote.create_session(Default::default()).await.context("creating a portal session")?;

        if !persist {
            let r = request(&remote, &screencast, &session, None, false).await?;
            return Self::from_response(remote, screencast, session, r);
        }
        let saved = match fsutil::read_private_text(&token_path()) {
            Ok(t) => t.filter(|s| !s.trim().is_empty()),
            Err(e) => {
                warn!("ignoring the saved portal grant: {e:#}");
                None
            }
        };
        let response = match request(&remote, &screencast, &session, saved.as_deref(), true).await {
            Ok(r) => r,
            Err(e) => {
                debug!("portal persist attempt failed, retrying without persistence: {e:#}");
                let session = remote.create_session(Default::default()).await.context("recreating a portal session")?;
                let r = request(&remote, &screencast, &session, None, false).await?;
                return Self::from_response(remote, screencast, session, r);
            }
        };
        if let Some(token) = response.restore_token() {
            save_token(token);
        }
        Self::from_response(remote, screencast, session, response)
    }

    fn from_response(
        remote: RemoteDesktop,
        screencast: Screencast,
        session: Session<RemoteDesktop>,
        response: ashpd::desktop::remote_desktop::SelectedDevices,
    ) -> Result<Arc<Self>> {
        let mut monitors = Vec::new();
        for (index, stream) in response.streams().iter().enumerate() {
            let (width, height) = stream.size().map(|(w, h)| (w as u32, h as u32)).unwrap_or((0, 0));
            let (x, y) = stream.position().unwrap_or((0, 0));
            monitors.push(PortalMonitor {
                info: MonitorInfo {
                    index: index as u32,
                    name: format!("Display {}", index + 1),
                    width,
                    height,
                    x,
                    y,
                    primary: index == 0,
                },
                node: stream.pipe_wire_node_id(),
            });
        }
        if monitors.is_empty() {
            return Err(anyhow!("the portal shared no monitors"));
        }
        info!(
            "portal sharing {} monitor(s): {}",
            monitors.len(),
            monitors.iter().map(|m| format!("{} {}x{}", m.info.name, m.info.width, m.info.height)).collect::<Vec<_>>().join(", "),
        );
        Ok(Arc::new(Self {
            remote: Arc::new(remote),
            screencast: Arc::new(screencast),
            session: Arc::new(session),
            monitors,
        }))
    }

    pub fn monitors(&self) -> Vec<MonitorInfo> {
        self.monitors.iter().map(|m| m.info.clone()).collect()
    }

    /// End the portal session: the compositor stops the share and its
    /// indicator goes away.
    pub async fn close(&self) {
        if let Err(e) = self.session.close().await {
            debug!("closing the portal session: {e}");
        }
        info!("screen share ended");
    }

    /// Start streaming one monitor: a fresh PipeWire fd, a `pipewiresrc`
    /// fragment capped at `fps`, and (with `control`) an input task bound to
    /// that monitor's node.
    pub async fn open_stream(&self, index: u32, fps: u32, control: bool) -> Result<PortalStream> {
        let mon = self
            .monitors
            .iter()
            .find(|m| m.info.index == index)
            .ok_or_else(|| anyhow!("no such monitor {index}"))?;

        let fd = self
            .screencast
            .open_pipe_wire_remote(&self.session, OpenPipeWireRemoteOptions::default())
            .await
            .context("opening the PipeWire remote")?;
        // Cap the framerate during PipeWire negotiation: a high-refresh
        // monitor otherwise delivers (and we'd encode) far more than needed.
        let source = format!(
            "pipewiresrc fd={} path={} do-timestamp=true keepalive-time=1000 \
             ! video/x-raw,max-framerate={fps}/1",
            fd.as_raw_fd(),
            mon.node,
        );
        let (width, height) = (mon.info.width, mon.info.height);

        let input = control.then(|| {
            let (tx, rx) = input::channel();
            tokio::spawn(inject(self.remote.clone(), self.session.clone(), mon.node, (width, height), rx));
            tx
        });

        Ok(PortalStream {
            source,
            width,
            height,
            input,
            _keep: StreamKeep { _fd: fd, _remote: self.remote.clone(), _session: self.session.clone() },
        })
    }
}

fn save_token(token: &str) {
    if let Err(e) = fsutil::write_private(&token_path(), token.as_bytes()) {
        warn!("couldn't save the portal restore token: {e:#}");
    }
}

/// Select devices and all monitors, then start. `persist` asks the compositor
/// to remember the grant (not every compositor allows it).
async fn request(
    remote: &RemoteDesktop,
    screencast: &Screencast,
    session: &Session<RemoteDesktop>,
    token: Option<&str>,
    persist: bool,
) -> Result<ashpd::desktop::remote_desktop::SelectedDevices> {
    let mode = if persist { PersistMode::ExplicitlyRevoked } else { PersistMode::DoNot };
    remote
        .select_devices(
            session,
            SelectDevicesOptions::default()
                .set_devices(DeviceType::Keyboard | DeviceType::Pointer)
                .set_restore_token(token)
                .set_persist_mode(mode),
        )
        .await
        .context("selecting input devices")?;
    let monitors: BitFlags<SourceType> = SourceType::Monitor.into();
    screencast
        .select_sources(
            session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Embedded)
                .set_sources(monitors)
                // Let the user pick more than one monitor to make available.
                .set_multiple(true)
                .set_restore_token(token)
                .set_persist_mode(mode),
        )
        .await
        .context("selecting the screens to share")?;
    if token.is_none() {
        info!("waiting for the screen-share dialog to be accepted…");
    }
    remote
        .start(session, None, StartOptions::default())
        .await
        .context("starting the portal session")?
        .response()
        .context("the screen-share request was denied")
}

/// Pump input into the RemoteDesktop portal for one monitor until the channel
/// closes, then release anything still held. Absolute motion is addressed to
/// this monitor's `node`; buttons and keys are global to the session.
async fn inject(
    remote: Arc<RemoteDesktop>,
    session: Arc<Session<RemoteDesktop>>,
    node: u32,
    size: (u32, u32),
    mut rx: mpsc::Receiver<InputEvent>,
) {
    use std::collections::HashSet;
    let (w, h) = (size.0 as f64, size.1 as f64);
    let mut keys: HashSet<u16> = HashSet::new();
    let mut buttons: HashSet<MouseButton> = HashSet::new();
    let mut scroll = (0i32, 0i32);

    let mut next = None;
    while let Some(first) = match next.take() {
        Some(ev) => Some(ev),
        None => rx.recv().await,
    } {
        // Each portal call is a D-Bus round trip, so merge motion that piled
        // up meanwhile instead of replaying it.
        let ev = merge_motion(first, &mut rx, &mut next);
        if let Err(e) = apply(&remote, &session, node, (w, h), &mut keys, &mut buttons, &mut scroll, ev).await {
            debug!("portal input: {e}");
        }
    }

    for code in keys.drain().collect::<Vec<_>>() {
        release_key(&remote, &session, code).await;
    }
    for b in buttons.drain().collect::<Vec<_>>() {
        release_button(&remote, &session, b).await;
    }
}

/// Fold queued motion of the same kind into `ev`; the first event of another
/// kind is left in `next`.
fn merge_motion(mut ev: InputEvent, rx: &mut mpsc::Receiver<InputEvent>, next: &mut Option<InputEvent>) -> InputEvent {
    loop {
        if !matches!(ev, InputEvent::PointerAbs { .. } | InputEvent::PointerRel { .. }) {
            return ev;
        }
        let Ok(more) = rx.try_recv() else { return ev };
        ev = match (ev, more) {
            (InputEvent::PointerAbs { .. }, m @ InputEvent::PointerAbs { .. }) => m,
            (InputEvent::PointerRel { dx, dy }, InputEvent::PointerRel { dx: x, dy: y }) => {
                InputEvent::PointerRel { dx: dx + x, dy: dy + y }
            }
            (ev, other) => {
                *next = Some(other);
                return ev;
            }
        };
    }
}

/// Release a key, retrying once: a key left down on the host is worse than
/// a duplicate release.
async fn release_key(remote: &RemoteDesktop, session: &Session<RemoteDesktop>, code: u16) {
    for _ in 0..2 {
        if remote.notify_keyboard_keycode(session, code as i32, KeyState::Released, Default::default()).await.is_ok() {
            return;
        }
    }
    warn!("couldn't release key {code} on the host");
}

async fn release_button(remote: &RemoteDesktop, session: &Session<RemoteDesktop>, b: MouseButton) {
    for _ in 0..2 {
        if remote.notify_pointer_button(session, b.evdev() as i32, KeyState::Released, Default::default()).await.is_ok() {
            return;
        }
    }
    warn!("couldn't release mouse button {b:?} on the host");
}

#[allow(clippy::too_many_arguments)]
async fn apply(
    remote: &RemoteDesktop,
    session: &Session<RemoteDesktop>,
    node: u32,
    (w, h): (f64, f64),
    keys: &mut std::collections::HashSet<u16>,
    buttons: &mut std::collections::HashSet<MouseButton>,
    scroll: &mut (i32, i32),
    ev: InputEvent,
) -> Result<()> {
    let max = POINTER_MAX as f64;
    match ev {
        InputEvent::PointerAbs { x, y } => {
            let px = (x as f64 / max) * (w - 1.0).max(0.0);
            let py = (y as f64 / max) * (h - 1.0).max(0.0);
            remote.notify_pointer_motion_absolute(session, node, px, py, Default::default()).await?;
        }
        InputEvent::PointerRel { dx, dy } => {
            remote.notify_pointer_motion(session, dx as f64, dy as f64, Default::default()).await?;
        }
        InputEvent::Button { button, pressed } => {
            let changed = if pressed { buttons.insert(button) } else { buttons.remove(&button) };
            if changed {
                remote.notify_pointer_button(session, button.evdev() as i32, state(pressed), Default::default()).await?;
            }
        }
        InputEvent::Scroll { dx, dy } => {
            let v = take_steps(&mut scroll.1, dy);
            if v != 0 {
                remote.notify_pointer_axis_discrete(session, Axis::Vertical, v, Default::default()).await?;
            }
            let hstep = take_steps(&mut scroll.0, dx);
            if hstep != 0 {
                remote.notify_pointer_axis_discrete(session, Axis::Horizontal, hstep, Default::default()).await?;
            }
        }
        InputEvent::Key { code, pressed } => {
            if pressed {
                let r = remote.notify_keyboard_keycode(session, code as i32, KeyState::Pressed, Default::default()).await;
                // Only track a key as held once the press actually went through.
                r?;
                keys.insert(code);
            } else if keys.remove(&code) {
                release_key(remote, session, code).await;
            }
        }
        InputEvent::ReleaseAll => {
            for code in keys.drain().collect::<Vec<_>>() {
                release_key(remote, session, code).await;
            }
            for b in buttons.drain().collect::<Vec<_>>() {
                release_button(remote, session, b).await;
            }
        }
    }
    Ok(())
}

fn state(pressed: bool) -> KeyState {
    if pressed { KeyState::Pressed } else { KeyState::Released }
}

/// Most discrete steps one scroll event may become.
const MAX_STEPS: i32 = crate::input::MAX_SCROLL / 120;

/// 1/120th-notch units in; whole steps out (at most [`MAX_STEPS`] either
/// way), keeping a remainder under one notch. Saturates, never overflows.
fn take_steps(acc: &mut i32, delta: i32) -> i32 {
    *acc = acc.saturating_add(delta);
    let steps = (*acc / 120).clamp(-MAX_STEPS, MAX_STEPS);
    *acc = (*acc - steps * 120).clamp(-119, 119);
    steps
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hostile_scroll_steps_are_bounded() {
        let mut acc = 0;
        assert_eq!(take_steps(&mut acc, i32::MAX), MAX_STEPS);
        assert_eq!(take_steps(&mut acc, i32::MIN), -MAX_STEPS);
        assert!(acc.abs() < 120);
        let mut acc = 0;
        assert_eq!(take_steps(&mut acc, 60), 0);
        assert_eq!(take_steps(&mut acc, 60), 1);
        assert_eq!(acc, 0);
    }
}
