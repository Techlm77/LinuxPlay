//! The signalling side of a connection, run on the tokio runtime. Anything
//! that needs the user or the window is sent to the main thread as a
//! [`ConnectEvent`].

use std::collections::HashMap;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use linuxplay_net::auth::{self, Hello};
use linuxplay_net::tls::Identity;
use linuxplay_net::netif::{EXPOSED_ADVICE, is_public};
use linuxplay_net::rendezvous::{Account, Server};
use linuxplay_net::ws::{self, Route};
use linuxplay_proto::rendezvous::InviteLink;
use linuxplay_proto::signal::{ClientMsg, HostMsg, MonitorInfo, VideoCodec};
use linuxplay_proto::{DEFAULT_PORT, PROTOCOL_VERSION};
use tokio::sync::oneshot;
use tracing::debug;

pub struct Request {
    /// What the user asked to connect to: an invite link, the name of one of
    /// the account's devices, or a network address.
    pub target: String,
    /// The account this device is signed in to, if any.
    pub account: Option<Account>,
    /// Certificates pinned so far, by the key each host is remembered under.
    pub pins: HashMap<String, [u8; 32]>,
    pub client_name: String,
    pub codecs: Vec<VideoCodec>,
    /// The subset of `codecs` decoded in hardware.
    pub hw_codecs: Vec<VideoCodec>,
    /// This display's refresh rate.
    pub max_fps: u32,
    /// Which host display to open; `None` asks the user to choose.
    pub monitor: Option<u32>,
    /// A PIN or password known in advance, tried once before asking.
    pub pin: Option<String>,
}

pub struct Offer {
    pub host_name: String,
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub sdp: String,
    /// How the host is reached, which decides where media sockets go.
    pub route: Route,
    /// The host allows keyboard and mouse; without it the stream is view only.
    pub control: bool,
    /// The display this stream shows.
    pub monitor: MonitorInfo,
    /// Every display the host offered, for opening others.
    pub monitors: Vec<MonitorInfo>,
}

pub enum ConnectEvent {
    Status(String),
    /// Ask the user for the PIN (or password); `None` cancels.
    NeedPin {
        host_name: String,
        password: bool,
        reply: oneshot::Sender<Option<String>>,
    },
    /// Ask which display to open; reply with an index, or `None` to cancel.
    NeedMonitor {
        host_name: String,
        monitors: Vec<MonitorInfo>,
        reply: oneshot::Sender<Option<u32>>,
    },
    /// The host proved its identity (by pairing, or by matching the pin):
    /// remember (or refresh) it.
    Verified {
        address: String,
        host_name: String,
        fingerprint: [u8; 32],
    },
    /// Set up the stream and reply with the SDP answer.
    Offer(Offer, oneshot::Sender<Result<String>>),
    /// Something the user should know and act on, though the connection
    /// goes ahead: the host can be reached from the internet.
    Warning(String),
    /// The connection is over; `Some` says why it failed.
    Ended(Option<String>),
}

/// Where [`ConnectEvent`]s go: a queue the main thread drains, and a way to
/// wake it.
#[derive(Clone)]
pub struct UiSink {
    tx: async_channel::Sender<ConnectEvent>,
    wake: Arc<dyn Fn() + Send + Sync>,
}

impl UiSink {
    pub fn new(wake: impl Fn() + Send + Sync + 'static) -> (Self, async_channel::Receiver<ConnectEvent>) {
        let (tx, rx) = async_channel::unbounded();
        (Self { tx, wake: Arc::new(wake) }, rx)
    }

    async fn send(&self, event: ConnectEvent) -> Result<()> {
        self.tx.send(event).await.map_err(|_| anyhow!("the window was closed"))?;
        (self.wake)();
        Ok(())
    }
}

/// Run one connection until it ends, the host hangs up, or `cancel` fires
/// (or its sender is dropped).
pub async fn run(req: Request, identity: Arc<Identity>, ui: UiSink, cancel: async_channel::Receiver<()>) {
    let result = tokio::select! {
        r = session(&req, &identity, &ui) => r,
        _ = cancel.recv() => Ok(()),
    };
    let _ = ui.send(ConnectEvent::Ended(result.err().map(|e| format!("{e:#}")))).await;
}

/// A way to reach a host, worked out from what the user asked for.
enum Path {
    /// Straight to a network address.
    Direct { host: String, port: u16 },
    /// To one of the account's devices, through the rendezvous server.
    Device { account: Account, id: String, name: String },
    /// To whoever sent an invite, through the invite's rendezvous server.
    Invite(InviteLink),
}

impl Path {
    /// What the host is remembered under once it has proven who it is.
    /// Invites are for one visit and are not remembered.
    fn key(&self, target: &str) -> Option<String> {
        match self {
            Self::Direct { .. } => Some(target.to_string()),
            Self::Device { id, .. } => Some(format!("device:{id}")),
            Self::Invite(_) => None,
        }
    }
}

/// Decide what `req.target` means: an invite link if it reads as one, else a
/// device of the account by name or id, else an address.
async fn resolve(req: &Request) -> Result<Path> {
    if let Some(link) = InviteLink::parse(&req.target) {
        return Ok(Path::Invite(link));
    }
    if let Some(account) = &req.account {
        // A server that can't be reached must not stop a direct connection.
        let devices = account.devices().await.unwrap_or_default();
        let wanted = req.target.trim();
        let found = devices.into_iter().find(|d| !d.current && (d.name.eq_ignore_ascii_case(wanted) || d.id == wanted));
        if let Some(device) = found {
            if !device.online {
                bail!("{} isn't sharing right now. Start LinuxPlay's host on it first.", device.name);
            }
            return Ok(Path::Device { account: account.clone(), id: device.id, name: device.name });
        }
    }
    let (host, port) = ws::parse_address(&req.target, DEFAULT_PORT)?;
    Ok(Path::Direct { host, port })
}

/// Open the signalling connection along `path`.
async fn dial(path: &Path, identity: &Identity, ui: &UiSink) -> Result<ws::Connected> {
    match path {
        Path::Direct { host, port } => {
            ui.send(ConnectEvent::Status(format!("Connecting to {host}…"))).await?;
            let conn = ws::connect(host, *port, identity).await?;
            if let Route::Direct { peer, .. } = conn.route
                && is_public(peer.ip()) {
                    ui.send(ConnectEvent::Warning(format!("{host} is on the open internet. {EXPOSED_ADVICE}"))).await?;
                }
            Ok(conn)
        }
        Path::Device { account, id, name } => {
            ui.send(ConnectEvent::Status(format!("Connecting to {name}…"))).await?;
            warn_if_exposed(account.server(), ui).await?;
            let stun = account.server().stun().await;
            let tunnel = account.call_device(id).await?;
            ws::connect_io(tunnel, Route::Relayed { stun }, identity).await
        }
        Path::Invite(link) => {
            ui.send(ConnectEvent::Status("Connecting to the host that invited you…".into())).await?;
            let server = Server::new(&link.server)?;
            warn_if_exposed(&server, ui).await?;
            let stun = server.stun().await;
            let tunnel = server.call_invite(&link.id).await?;
            ws::connect_io(tunnel, Route::Relayed { stun }, identity).await
        }
    }
}

/// Pass on the account server's own warning that the internet can reach it.
async fn warn_if_exposed(server: &Server, ui: &UiSink) -> Result<()> {
    if let Some(warning) = server.info().await.ok().and_then(|info| info.warning) {
        ui.send(ConnectEvent::Warning(format!("The account server says: {warning}"))).await?;
    }
    Ok(())
}

async fn session(req: &Request, identity: &Identity, ui: &UiSink) -> Result<()> {
    let path = resolve(req).await?;
    let key = path.key(&req.target);
    let pinned = key.as_ref().and_then(|k| req.pins.get(k));
    let mut conn = dial(&path, identity, ui).await?;

    // Authenticate: a pinned host must show the pinned certificate; a new
    // one must complete PIN pairing before its Welcome is believed.
    let hello = Hello {
        version: PROTOCOL_VERSION,
        name: req.client_name.clone(),
        codecs: req.codecs.clone(),
        hw_codecs: req.hw_codecs.clone(),
        max_fps: req.max_fps,
        audio: !conn.route.is_same_machine(),
    };
    // An invite carries its own secret; otherwise use what was given, if anything.
    let mut known_pin = match &path {
        Path::Invite(link) => Some(link.secret.clone()),
        _ => req.pin.clone(),
    };
    let ask_pin = |host_name: String, password: bool| {
        let known = known_pin.take();
        async move {
            if known.is_some() {
                return known;
            }
            let (tx, rx) = oneshot::channel();
            ui.send(ConnectEvent::NeedPin { host_name, password, reply: tx }).await.ok()?;
            rx.await.ok().flatten()
        }
    };
    let authed = auth::authenticate(
        &mut conn.signal,
        &conn.host_fp,
        &identity.fingerprint,
        pinned,
        hello,
        ask_pin,
    )
    .await?;
    let display_name = authed.host_name;
    // Only now, with the identity proven, is the host remembered.
    if let Some(key) = key {
        ui.send(ConnectEvent::Verified { address: key, host_name: display_name.clone(), fingerprint: conn.host_fp }).await?;
    }

    // The person at the host decides whether (and how) to share.
    let control = loop {
        match conn.signal.expect::<HostMsg>().await? {
            HostMsg::AwaitingApproval => {
                ui.send(ConnectEvent::Status(format!("Waiting for someone at {display_name} to accept…"))).await?;
            }
            HostMsg::Approved { control, .. } => break control,
            HostMsg::Error { message } => bail!(message),
            m => bail!("unexpected message: {m:?}"),
        }
    };

    // Choose a display from the host's list.
    let HostMsg::Monitors { monitors } = conn.signal.expect().await? else {
        bail!("expected the display list");
    };
    if monitors.is_empty() {
        bail!("the host has no displays to share");
    }
    let index = match req.monitor {
        Some(i) => i,
        None if monitors.len() == 1 => monitors[0].index,
        None => {
            let (tx, rx) = oneshot::channel();
            ui.send(ConnectEvent::NeedMonitor { host_name: display_name.clone(), monitors: monitors.clone(), reply: tx })
                .await?;
            rx.await.ok().flatten().ok_or_else(|| anyhow!("no display chosen"))?
        }
    };
    conn.signal.send(&ClientMsg::SelectMonitor { index }).await?;
    ui.send(ConnectEvent::Status("Starting the stream…".into())).await?;

    let offer = match conn.signal.expect::<HostMsg>().await? {
        HostMsg::Offer { sdp, codec, width, height, fps } => {
            let monitor = monitors.iter().find(|m| m.index == index).cloned().unwrap_or(MonitorInfo {
                index,
                name: "Display".into(),
                width,
                height,
                x: 0,
                y: 0,
                primary: false,
            });
            Offer { host_name: display_name, codec, width, height, fps, sdp, route: conn.route, control, monitor, monitors }
        }
        HostMsg::Error { message } => bail!(message),
        m => bail!("unexpected message: {m:?}"),
    };

    let (tx, rx) = oneshot::channel();
    ui.send(ConnectEvent::Offer(offer, tx)).await?;
    let answer = rx.await.map_err(|_| anyhow!("cancelled"))??;
    conn.signal.send(&ClientMsg::Answer { sdp: answer }).await?;

    // Media flows peer to peer now; the socket stays open as the session's
    // lifeline and for the host to report errors.
    loop {
        match conn.signal.recv::<HostMsg>().await? {
            None => return Ok(()),
            Some(HostMsg::Error { message }) => bail!(message),
            Some(m) => debug!("ignoring {m:?}"),
        }
    }
}
