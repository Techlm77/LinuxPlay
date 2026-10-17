//! Wayland capture and input through the desktop portals.
//!
//! One RemoteDesktop session carries both the screen cast (a PipeWire node we
//! feed to `pipewiresrc`) and input injection. The first time the host runs it
//! shows the compositor's share dialog; the returned restore token is saved so
//! later sessions start silently.

use std::fs;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
use std::sync::Arc;

use anyhow::{Context, Result};
use ashpd::desktop::PersistMode;
use ashpd::desktop::remote_desktop::{Axis, DeviceType, KeyState, RemoteDesktop, SelectDevicesOptions, StartOptions};
use ashpd::desktop::screencast::{CursorMode, OpenPipeWireRemoteOptions, Screencast, SelectSourcesOptions, SourceType};
use ashpd::desktop::Session;
use ashpd::enumflags2::BitFlags;
use linuxplay_proto::input::{InputEvent, MouseButton, POINTER_MAX};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

use crate::input::InputTx;

/// An open portal capture: the pipewire source fragment, the stream size, a
/// channel for input, and the resources that must outlive the stream.
pub struct PortalCapture {
    pub source: String,
    pub width: u32,
    pub height: u32,
    pub input: InputTx,
    /// Keeps the portal session, the pipewire fd and the input task alive;
    /// dropping it closes the session and ends the input task.
    pub _keep: Keep,
}

pub struct Keep {
    _session: Arc<Session<RemoteDesktop>>,
    _remote: Arc<RemoteDesktop>,
    _fd: OwnedFd,
}

fn token_path() -> PathBuf {
    linuxplay_net::tls::config_dir("host").join("portal-restore-token")
}

/// Set up the portals and start an input-injection task.
pub async fn open(_fps: u32) -> Result<PortalCapture> {
    let remote = RemoteDesktop::new().await.context("connecting to the RemoteDesktop portal")?;
    let screencast = Screencast::new().await.context("connecting to the ScreenCast portal")?;

    let session = remote.create_session(Default::default()).await.context("creating a portal session")?;

    // A saved restore token lets GNOME re-grant silently. KDE rejects
    // persistence on remote-desktop sessions, so we fall back to a fresh
    // (dialog-prompted) grant there.
    let saved = fs::read_to_string(token_path()).ok().filter(|s| !s.trim().is_empty());

    let response = match request(&remote, &screencast, &session, saved.as_deref(), true).await {
        Ok(r) => r,
        Err(e) => {
            // Either the token is stale or this compositor (e.g. KDE) refuses
            // to persist remote-desktop sessions; retry with a fresh,
            // non-persistent session that prompts the share dialog.
            debug!("portal persist attempt failed, retrying without persistence: {e:#}");
            let session = remote.create_session(Default::default()).await.context("recreating a portal session")?;
            let r = request(&remote, &screencast, &session, None, false).await?;
            return finish(remote, screencast, session, r).await;
        }
    };

    if let Some(new_token) = response.restore_token() {
        let path = token_path();
        if let Some(dir) = path.parent() {
            let _ = fs::create_dir_all(dir);
        }
        if let Err(e) = fs::write(&path, new_token) {
            warn!("couldn't save the portal restore token: {e}");
        }
    }

    finish(remote, screencast, session, response).await
}

/// Select devices and sources, then start. `persist` asks the compositor to
/// remember the grant (not all do).
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
    let monitor: BitFlags<SourceType> = SourceType::Monitor.into();
    screencast
        .select_sources(
            session,
            SelectSourcesOptions::default()
                .set_cursor_mode(CursorMode::Embedded)
                .set_sources(monitor)
                .set_multiple(false)
                .set_restore_token(token)
                .set_persist_mode(mode),
        )
        .await
        .context("selecting the screen to share")?;
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

/// Turn a started session into a running capture.
async fn finish(
    remote: RemoteDesktop,
    screencast: Screencast,
    session: Session<RemoteDesktop>,
    response: ashpd::desktop::remote_desktop::SelectedDevices,
) -> Result<PortalCapture> {
    let stream = response.streams().first().context("the portal returned no video stream")?;
    let (width, height) = stream
        .size()
        .map(|(w, h)| (w as u32, h as u32))
        .context("the portal stream has no size")?;
    let node = stream.pipe_wire_node_id();

    let fd = screencast
        .open_pipe_wire_remote(&session, OpenPipeWireRemoteOptions::default())
        .await
        .context("opening the PipeWire remote")?;

    let source = format!(
        "pipewiresrc fd={} path={} do-timestamp=true keepalive-time=1000",
        fd.as_raw_fd(),
        node,
    );
    info!("portal capture: monitor {width}x{height}, pipewire node {node}");

    let remote = Arc::new(remote);
    let session = Arc::new(session);
    let (tx, rx) = mpsc::unbounded_channel::<InputEvent>();
    tokio::spawn(inject(remote.clone(), session.clone(), node, (width, height), rx));

    Ok(PortalCapture {
        source,
        width,
        height,
        input: tx,
        _keep: Keep { _session: session, _remote: remote, _fd: fd },
    })
}

/// Pump input events into the RemoteDesktop portal until the channel closes,
/// then release anything still held.
async fn inject(
    remote: Arc<RemoteDesktop>,
    session: Arc<Session<RemoteDesktop>>,
    node: u32,
    size: (u32, u32),
    mut rx: mpsc::UnboundedReceiver<InputEvent>,
) {
    use std::collections::HashSet;
    let (w, h) = (size.0 as f64, size.1 as f64);
    let mut keys: HashSet<u16> = HashSet::new();
    let mut buttons: HashSet<MouseButton> = HashSet::new();
    let mut scroll = (0i32, 0i32);

    while let Some(ev) = rx.recv().await {
        if let Err(e) = apply(&remote, &session, node, (w, h), &mut keys, &mut buttons, &mut scroll, ev).await {
            debug!("portal input: {e}");
        }
    }

    // The client went away; lift every key and button it left down.
    for code in keys.drain().collect::<Vec<_>>() {
        let _ = remote.notify_keyboard_keycode(&session, code as i32, KeyState::Released, Default::default()).await;
    }
    for b in buttons.drain().collect::<Vec<_>>() {
        let _ = remote.notify_pointer_button(&session, b.evdev() as i32, KeyState::Released, Default::default()).await;
    }
    let _ = session.close().await;
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
            // The portal takes whole wheel steps; bank the remainder.
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
                keys.insert(code);
                remote.notify_keyboard_keycode(session, code as i32, KeyState::Pressed, Default::default()).await?;
            } else if keys.remove(&code) {
                remote.notify_keyboard_keycode(session, code as i32, KeyState::Released, Default::default()).await?;
            }
        }
        InputEvent::ReleaseAll => {
            for code in keys.drain().collect::<Vec<_>>() {
                remote.notify_keyboard_keycode(session, code as i32, KeyState::Released, Default::default()).await?;
            }
            for b in buttons.drain().collect::<Vec<_>>() {
                remote.notify_pointer_button(session, b.evdev() as i32, KeyState::Released, Default::default()).await?;
            }
        }
    }
    Ok(())
}

fn state(pressed: bool) -> KeyState {
    if pressed { KeyState::Pressed } else { KeyState::Released }
}

/// 1/120th-notch units in; whole steps out, keeping the remainder.
fn take_steps(acc: &mut i32, delta: i32) -> i32 {
    *acc += delta;
    let steps = *acc / 120;
    *acc -= steps * 120;
    steps
}

