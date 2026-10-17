//! The signalling side of a connection, run on the tokio runtime. Anything
//! that needs the user or GTK is sent to the main thread as a [`ToUi`].

use std::net::IpAddr;
use std::sync::Arc;

use anyhow::{Result, anyhow, bail};
use linuxplay_net::auth::{self, Hello};
use linuxplay_net::tls::Identity;
use linuxplay_net::ws;
use linuxplay_proto::signal::{ClientMsg, HostMsg, MonitorInfo, VideoCodec};
use linuxplay_proto::{DEFAULT_PORT, PROTOCOL_VERSION};
use tokio::sync::oneshot;
use tracing::debug;

pub struct Request {
    pub address: String,
    /// The fingerprint pinned for this address, if paired before.
    pub pinned: Option<[u8; 32]>,
    pub client_name: String,
    pub codecs: Vec<VideoCodec>,
    /// The subset of `codecs` decoded in hardware.
    pub hw_codecs: Vec<VideoCodec>,
    /// This display's refresh rate.
    pub max_fps: u32,
    /// Which host display to open; `None` asks the user to choose.
    pub monitor: Option<u32>,
}

pub struct Offer {
    pub host_name: String,
    pub codec: VideoCodec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub sdp: String,
    pub local_ip: IpAddr,
    /// The host allows keyboard and mouse; without it the stream is view only.
    pub control: bool,
    /// The display this stream shows.
    pub monitor: MonitorInfo,
    /// Every display the host offered, for opening others.
    pub monitors: Vec<MonitorInfo>,
}

pub enum ToUi {
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
    /// The connection is over; `Some` says why it failed.
    Ended(Option<String>),
}

/// Run one connection until it ends, the host hangs up, or `cancel` fires
/// (or its sender is dropped).
pub async fn run(req: Request, identity: Arc<Identity>, ui: async_channel::Sender<ToUi>, cancel: async_channel::Receiver<()>) {
    let result = tokio::select! {
        r = session(&req, &identity, &ui) => r,
        _ = cancel.recv() => Ok(()),
    };
    let _ = ui.send(ToUi::Ended(result.err().map(|e| format!("{e:#}")))).await;
}

async fn session(req: &Request, identity: &Identity, ui: &async_channel::Sender<ToUi>) -> Result<()> {
    let (host, port) = ws::parse_address(&req.address, DEFAULT_PORT)?;
    ui.send(ToUi::Status(format!("Connecting to {host}…"))).await?;
    let mut conn = ws::connect(&host, port, identity).await?;

    // Authenticate: a pinned host must show the pinned certificate; a new
    // one must complete PIN pairing before its Welcome is believed.
    let hello = Hello {
        version: PROTOCOL_VERSION,
        name: req.client_name.clone(),
        codecs: req.codecs.clone(),
        hw_codecs: req.hw_codecs.clone(),
        max_fps: req.max_fps,
        audio: true,
    };
    let ask_pin = |host_name: String, password: bool| async move {
        let (tx, rx) = oneshot::channel();
        ui.send(ToUi::NeedPin { host_name, password, reply: tx }).await.ok()?;
        rx.await.ok().flatten()
    };
    let authed = auth::authenticate(
        &mut conn.signal,
        &conn.host_fp,
        &identity.fingerprint,
        req.pinned.as_ref(),
        hello,
        ask_pin,
    )
    .await?;
    let display_name = authed.host_name;
    // Only now, with the identity proven, is the host remembered.
    ui.send(ToUi::Verified { address: req.address.clone(), host_name: display_name.clone(), fingerprint: conn.host_fp })
        .await?;

    // The person at the host decides whether (and how) to share.
    let control = loop {
        match conn.signal.expect::<HostMsg>().await? {
            HostMsg::AwaitingApproval => {
                ui.send(ToUi::Status(format!("Waiting for someone at {display_name} to accept…"))).await?;
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
            ui.send(ToUi::NeedMonitor { host_name: display_name.clone(), monitors: monitors.clone(), reply: tx })
                .await?;
            rx.await.ok().flatten().ok_or_else(|| anyhow!("no display chosen"))?
        }
    };
    conn.signal.send(&ClientMsg::SelectMonitor { index }).await?;
    ui.send(ToUi::Status("Starting the stream…".into())).await?;

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
            Offer { host_name: display_name, codec, width, height, fps, sdp, local_ip: conn.local_ip, control, monitor, monitors }
        }
        HostMsg::Error { message } => bail!(message),
        m => bail!("unexpected message: {m:?}"),
    };

    let (tx, rx) = oneshot::channel();
    ui.send(ToUi::Offer(offer, tx)).await?;
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
