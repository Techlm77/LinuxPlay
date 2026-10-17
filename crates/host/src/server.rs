//! Accepting clients: limits, authentication, pairing, approval by the person
//! at the host, then one stream per display.
//!
//! Every connection is in one of three phases, each with its own limits:
//! authenticating (counted against the pending limits, with a deadline for
//! every stage), waiting for approval, and approved (counted against the
//! session cap). From `Welcome` on it is registered in [`Sessions`], so it can
//! be ended at once when its device is revoked or sharing is stopped.

use std::collections::{BTreeSet, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow, bail};
use linuxplay_net::rtc::{self, OfferOptions};
use linuxplay_net::tls::{self, Identity};
use linuxplay_net::ws::{self, Accepted};
use linuxplay_net::{Channel, SessionEvent, SessionHandle, pair};
use linuxplay_proto::control::ControlMsg;
use linuxplay_proto::input::InputEvent;
use linuxplay_proto::signal::{ClientMsg, HostMsg, MonitorInfo, VideoCodec};
use linuxplay_proto::{MAX_CONTROL_MESSAGE, PROTOCOL_VERSION, decode, encode};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore, mpsc};
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, warn};

use crate::approval::{Approver, Decision, PinBoard};
use crate::audio::AudioPipeline;
use crate::capture::{self, Capture, CaptureKind};
use crate::control::{self, Command};
use crate::input::{InputGate, Verdict};
use crate::limits::{self, PairLimiter, Pending, PendingPermit};
use crate::notify::{Note, Notifier};
use crate::portal::{self, PortalHost};
use crate::quality::{self, ClientCaps, Overrides};
use crate::sessions::{Grant, Sessions, State, Ticket};
use crate::tray::{HostTray, TrayCmd, TrayState};
use crate::trust::TrustStore;
use crate::video::{self, VideoPipeline};

pub struct HostConfig {
    pub name: String,
    /// Addresses to listen on (never a silent `0.0.0.0`).
    pub listen: Vec<IpAddr>,
    /// How those were chosen, e.g. "local network only".
    pub exposure: String,
    pub port: u16,
    /// UDP ports for media, inclusive.
    pub media_ports: Option<(u16, u16)>,
    pub capture: Option<CaptureKind>,
    pub encoder: Option<String>,
    pub fps: u32,
    pub start_kbps: u32,
    pub max_kbps: u32,
    pub audio: bool,
    /// Paired devices connect without asking, and the Wayland share grant is
    /// remembered. Off by default.
    pub unattended: bool,
    /// Permanent pairing password (unattended only); without it a one-time
    /// PIN is shown.
    pub password: Option<String>,
    /// Displays clients may stream; `None` means all.
    pub displays: Option<BTreeSet<u32>>,
}

/// How long each handshake stage may take.
#[derive(Clone, Copy)]
pub struct Timeouts {
    pub hello: Duration,
    /// The user typing the PIN on the client.
    pub pair: Duration,
    pub pair_finish: Duration,
    /// Everything from accepting the connection to `Welcome`.
    pub auth: Duration,
    /// The user choosing a display on the client.
    pub select: Duration,
    pub answer: Duration,
}

impl Default for Timeouts {
    fn default() -> Self {
        Self {
            hello: Duration::from_secs(10),
            pair: Duration::from_secs(120),
            pair_finish: Duration::from_secs(10),
            auth: Duration::from_secs(150),
            select: Duration::from_secs(120),
            answer: Duration::from_secs(15),
        }
    }
}

/// Most displays that may stream at once.
const MAX_STREAMS: usize = 4;
/// Approved connections at once: the streams plus some choosing a display.
const MAX_SESSIONS: usize = MAX_STREAMS * 2;
/// Roughly the encoder's throughput, in pixels per second, split across the
/// active streams. About three 1080p60 streams; above it, framerates drop.
const ENCODER_PIXEL_BUDGET: u64 = 1920 * 1080 * 60 * 3;
/// Session events waiting for the stream loop; beyond it they are dropped.
const STREAM_EVENT_QUEUE: usize = 64;
/// Control messages (pings) a client may send per second, and the burst.
const CONTROL_RATE: f64 = 20.0;
const CONTROL_BURST: f64 = 40.0;
/// How often the trust store is checked for changes made elsewhere.
const TRUST_POLL: Duration = Duration::from_secs(2);

/// One streaming display, as the budget controller sees it.
struct StreamSlot {
    monitor: u32,
    /// Bitrate this resolution/codec wants on its own, kbit/s.
    res_cap_kbps: u32,
    pixels: u64,
    fps_cap: u32,
    video: Weak<VideoPipeline>,
    session: SessionHandle,
}

/// Divides the link bandwidth and the encoder's throughput across the active
/// displays, re-running whenever one joins or leaves — the "re-benchmark" that
/// keeps two displays from each assuming they own the whole pipe.
#[derive(Default)]
struct Budget {
    link_ceiling_kbps: AtomicU32,
    slots: Mutex<Vec<StreamSlot>>,
}

impl Budget {
    /// Set the link ceiling from the first stream's link type (once).
    fn init_ceiling(&self, lan: bool) {
        let _ = self.link_ceiling_kbps.compare_exchange(
            0,
            quality::link_ceiling(lan),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    fn register(&self, slot: StreamSlot) {
        self.slots.lock().unwrap().push(slot);
        self.rebalance();
    }

    fn unregister(&self, monitor: u32) {
        self.slots.lock().unwrap().retain(|s| s.monitor != monitor);
        self.rebalance();
    }

    /// Recompute every active stream's bitrate ceiling and framerate cap.
    fn rebalance(&self) {
        let slots = self.slots.lock().unwrap();
        let n = slots.len().max(1) as u32;
        let ceiling = self.link_ceiling_kbps.load(Ordering::Relaxed).max(2_000);
        let per_stream_bw = (ceiling / n).max(1_000);

        let total_pixel_rate: u64 = slots.iter().map(|s| s.pixels * s.fps_cap as u64).sum();
        let fps_scale = if total_pixel_rate > ENCODER_PIXEL_BUDGET {
            ENCODER_PIXEL_BUDGET as f64 / total_pixel_rate as f64
        } else {
            1.0
        };

        for s in slots.iter() {
            let Some(video) = s.video.upgrade() else { continue };
            let bw = s.res_cap_kbps.min(per_stream_bw);
            video.set_max_kbps(bw);
            s.session.set_desired_bitrate(bw as u64 * 1000);
            let fps = ((s.fps_cap as f64 * fps_scale).floor() as u32).clamp(24, s.fps_cap);
            video.set_fps_cap(fps);
        }

        if n > 1 {
            info!(
                "re-benchmarked for {n} displays: ~{per_stream_bw} kbit/s each{}",
                if fps_scale < 0.99 { format!(", framerate ×{fps_scale:.2}") } else { String::new() },
            );
        }
    }
}

/// How a stream ends. At most one per pipeline plus one for the session, so
/// the channel carrying these is bounded by construction.
enum StreamEnd {
    Closed(Option<String>),
    Failed(String),
}

/// What the client said in `Hello`.
struct HelloInfo {
    name: String,
    caps: ClientCaps,
    audio: bool,
}

pub struct Host {
    cfg: HostConfig,
    identity: Identity,
    capture_kind: CaptureKind,
    pub(crate) timeouts: Timeouts,
    trust: Mutex<TrustStore>,
    pairing: Arc<Mutex<PairLimiter>>,
    pending: Arc<Pending>,
    session_quota: Arc<Semaphore>,
    pub(crate) sessions: Arc<Sessions>,
    pub(crate) approver: Arc<Approver>,
    pub(crate) pins: Arc<PinBoard>,
    notifier: Option<Arc<Notifier>>,
    /// The Wayland portal session, created after the first approval.
    portal: AsyncMutex<Option<Arc<PortalHost>>>,
    /// Monitor indices currently streaming.
    active: Mutex<HashSet<u32>>,
    /// Which display, if any, currently carries the audio.
    audio_owner: Mutex<Option<u32>>,
    budget: Budget,
    /// Input "injected" by the test capture.
    pub(crate) test_input: Arc<Mutex<Vec<InputEvent>>>,
}

/// Frees a display's reservation (and its share of the budget) on drop.
struct StreamGuard<'a> {
    host: &'a Host,
    monitor: u32,
}

impl Drop for StreamGuard<'_> {
    fn drop(&mut self) {
        self.host.active.lock().unwrap().remove(&self.monitor);
        let mut audio = self.host.audio_owner.lock().unwrap();
        if *audio == Some(self.monitor) {
            *audio = None;
        }
        drop(audio);
        self.host.budget.unregister(self.monitor);
    }
}

/// Stops a stream's pipelines and session however the stream ends,
/// including when its future is dropped because the session was revoked.
struct StreamCleanup {
    video: Arc<VideoPipeline>,
    audio: Option<Arc<AudioPipeline>>,
    session: Option<SessionHandle>,
}

impl Drop for StreamCleanup {
    fn drop(&mut self) {
        if let Some(s) = &self.session {
            s.close();
        }
        self.video.stop();
        if let Some(a) = &self.audio {
            a.stop();
        }
    }
}

/// Ends a pairing attempt in the limiter, as a failure unless marked ok.
struct PairAttempt {
    limiter: Arc<Mutex<PairLimiter>>,
    source: IpAddr,
    ok: bool,
}

impl Drop for PairAttempt {
    fn drop(&mut self) {
        self.limiter.lock().unwrap().finish(self.source, self.ok, Instant::now());
    }
}

/// A token bucket for messages a client may send.
struct Bucket {
    tokens: f64,
    last: Instant,
}

impl Bucket {
    fn take(&mut self) -> bool {
        let now = Instant::now();
        self.tokens = (self.tokens + now.duration_since(self.last).as_secs_f64() * CONTROL_RATE).min(CONTROL_BURST);
        self.last = now;
        if self.tokens < 1.0 {
            return false;
        }
        self.tokens -= 1.0;
        true
    }
}

impl Host {
    pub async fn new(cfg: HostConfig, identity: Identity, trust: TrustStore) -> Result<Arc<Self>> {
        let notifier = Notifier::connect().await;
        Self::with_notifier(cfg, identity, trust, notifier)
    }

    pub fn with_notifier(
        cfg: HostConfig,
        identity: Identity,
        trust: TrustStore,
        notifier: Option<Arc<Notifier>>,
    ) -> Result<Arc<Self>> {
        let capture_kind = match cfg.capture {
            Some(k) => k,
            None => CaptureKind::detect()?,
        };
        if cfg.password.is_some() && !cfg.unattended {
            bail!("a pairing password is only used for unattended access; add --unattended");
        }
        if !cfg.unattended && capture_kind == CaptureKind::Portal {
            // Persistent sharing is off: forget any grant saved earlier.
            if let Err(e) = portal::remove_token() {
                warn!("{e:#}");
            }
        }
        Ok(Arc::new(Self {
            cfg,
            identity,
            capture_kind,
            timeouts: Timeouts::default(),
            trust: Mutex::new(trust),
            pairing: Arc::default(),
            pending: Pending::new(),
            session_quota: Arc::new(Semaphore::new(MAX_SESSIONS)),
            sessions: Arc::default(),
            approver: Approver::new(notifier.clone()),
            pins: PinBoard::new(notifier.clone()),
            notifier,
            portal: AsyncMutex::new(None),
            active: Mutex::default(),
            audio_owner: Mutex::default(),
            budget: Budget::default(),
            test_input: Arc::default(),
        }))
    }

    /// The Wayland portal session, created (with its share dialog) on first use.
    async fn portal(&self) -> Result<Arc<PortalHost>> {
        let mut guard = self.portal.lock().await;
        if let Some(p) = guard.as_ref() {
            return Ok(p.clone());
        }
        let p = PortalHost::create(self.cfg.unattended).await?;
        *guard = Some(p.clone());
        Ok(p)
    }

    /// In attended mode, end the screen share once nobody is connected, so
    /// the next connection has to be granted again.
    async fn release_portal_if_idle(&self) {
        if self.cfg.unattended || self.capture_kind != CaptureKind::Portal {
            return;
        }
        let mut guard = self.portal.lock().await;
        if !self.sessions.is_empty() {
            return;
        }
        if let Some(p) = guard.take() {
            p.close().await;
        }
    }

    /// The displays this host can share.
    async fn monitors(&self) -> Result<Vec<MonitorInfo>> {
        match self.capture_kind {
            CaptureKind::X11 => capture::monitors_x11(),
            CaptureKind::Portal => Ok(self.portal().await?.monitors()),
            CaptureKind::Test => Ok(capture::monitors_test()),
        }
    }

    /// Open a capture of one display; input only with `control`.
    async fn open_capture(&self, fps: u32, index: u32, control: bool) -> Result<Capture> {
        let find = |monitors: Vec<MonitorInfo>| {
            monitors.into_iter().find(|m| m.index == index).ok_or_else(|| anyhow!("no such display {index}"))
        };
        match self.capture_kind {
            CaptureKind::X11 => capture::open_x11(fps, &find(capture::monitors_x11()?)?, control),
            CaptureKind::Portal => {
                let stream = self.portal().await?.open_stream(index, fps, control).await?;
                Ok(capture::from_portal(stream))
            }
            CaptureKind::Test => {
                Ok(capture::open_test(fps, &find(capture::monitors_test())?, control, self.test_input.clone()))
            }
        }
    }

    async fn bind_listeners(&self) -> Result<Vec<TcpListener>> {
        let mut listeners = Vec::new();
        for ip in &self.cfg.listen {
            let l = TcpListener::bind((*ip, self.cfg.port))
                .await
                .with_context(|| format!("listening on {}", SocketAddr::new(*ip, self.cfg.port)))?;
            listeners.push(l);
        }
        Ok(listeners)
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        // Claim the control socket first: it also stops a second host.
        let (control_listener, _socket) = control::bind().await?;
        let listeners = self.bind_listeners().await?;
        let shown = listeners
            .iter()
            .filter_map(|l| l.local_addr().ok())
            .map(|a| a.to_string())
            .collect::<Vec<_>>()
            .join(", ");
        info!("listening on {shown} ({})", self.cfg.exposure);
        if let Some((a, b)) = self.cfg.media_ports {
            info!("media on UDP ports {a}-{b}");
        }
        if self.cfg.unattended {
            warn!("unattended access is on: paired devices connect without asking");
        } else {
            info!("each connection must be approved on this computer");
        }

        let quit = Arc::new(Notify::new());
        tokio::spawn(control::serve(control_listener, self.clone()));
        self.start_tray(quit.clone(), &shown).await;
        self.spawn_trust_watch();
        self.announce_started(&shown).await;
        self.accept_loop(listeners, quit).await
    }

    /// Hand connections from every listener to [`handle`](Self::handle)
    /// until `quit` fires.
    pub(crate) async fn accept_loop(self: Arc<Self>, listeners: Vec<TcpListener>, quit: Arc<Notify>) -> Result<()> {
        let acceptor = TlsAcceptor::from(Arc::new(self.identity.server_config()?));
        let (conn_tx, mut conn_rx) = mpsc::channel::<(TcpStream, SocketAddr)>(32);
        let mut tasks = Vec::new();
        for l in listeners {
            let tx = conn_tx.clone();
            tasks.push(tokio::spawn(async move {
                loop {
                    match l.accept().await {
                        // If the queue is full the connection is simply dropped.
                        Ok(c) => drop(tx.try_send(c)),
                        Err(e) => {
                            warn!("accept: {e}");
                            tokio::time::sleep(Duration::from_millis(200)).await;
                        }
                    }
                }
            }));
        }
        drop(conn_tx);

        let result = loop {
            let (tcp, peer) = tokio::select! {
                _ = quit.notified() => {
                    info!("quit requested");
                    break Ok(());
                }
                c = conn_rx.recv() => match c {
                    Some(c) => c,
                    None => break Ok(()),
                },
            };
            // Drop new connections while this source (or everyone) has too
            // many still authenticating.
            let Some(permit) = self.pending.try_acquire(peer.ip()) else {
                debug!("{peer}: refused, too many pending connections");
                continue;
            };
            let host = self.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                match ws::accept(tcp, &acceptor).await {
                    Ok(acc) => {
                        if let Err(e) = host.handle(acc, permit).await {
                            warn!("{peer}: {e:#}");
                        }
                    }
                    Err(e) => debug!("{peer}: {e:#}"),
                }
            });
        };
        for t in tasks {
            t.abort();
        }
        self.sessions.kill_all();
        result
    }

    fn tray_state(&self) -> TrayState {
        let sessions = self
            .sessions
            .snapshot()
            .into_iter()
            .filter_map(|s| match s.state {
                State::Approved(g) => Some((
                    s.id,
                    format!(
                        "{} ({}){}{}",
                        s.name,
                        s.ip,
                        s.display.map(|d| format!(" · display {d}")).unwrap_or_default(),
                        if g.control { "" } else { " · view only" }
                    ),
                )),
                State::Waiting => None,
            })
            .collect();
        TrayState { pins: self.pins.list(), waiting: self.approver.waiting(), sessions }
    }

    /// Show a tray icon (where a StatusNotifier host exists), keep it fresh,
    /// and act on its menu. Harmless where no tray is available.
    async fn start_tray(self: &Arc<Self>, quit: Arc<Notify>, shown: &str) {
        use ksni::TrayMethods;
        let fp = tls::fingerprint_display(&self.identity.fingerprint);
        let short: String = fp.chars().take(14).collect();
        let (cmds, mut cmd_rx) = mpsc::channel(16);
        let tray = HostTray {
            host_name: self.cfg.name.clone(),
            detail: format!("{} · {shown} · {short}…", self.cfg.exposure),
            unattended: self.cfg.unattended,
            state: TrayState::default(),
            quit,
            cmds,
        };
        let handle = match tokio::time::timeout(Duration::from_secs(3), tray.spawn()).await {
            Ok(Ok(handle)) => handle,
            Ok(Err(e)) => {
                debug!("no system tray: {e}");
                return;
            }
            Err(_) => {
                debug!("system tray setup timed out");
                return;
            }
        };
        let this = Arc::downgrade(self);
        tokio::spawn(async move {
            while let Some(cmd) = cmd_rx.recv().await {
                let Some(host) = this.upgrade() else { break };
                match cmd {
                    TrayCmd::Decide(id, d) => drop(host.approver.resolve(id, d)),
                    TrayCmd::Disconnect(id) => drop(host.sessions.kill(id)),
                    TrayCmd::StopSharing => {
                        let n = host.sessions.kill_all();
                        info!("sharing stopped from the tray; ended {n} session(s)");
                    }
                }
            }
        });
        let this = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut last = TrayState::default();
            loop {
                tokio::time::sleep(Duration::from_millis(500)).await;
                let Some(host) = this.upgrade() else { break };
                if handle.is_closed() {
                    break;
                }
                let state = host.tray_state();
                if state != last {
                    last = state.clone();
                    handle.update(|t| t.state = state).await;
                }
            }
        });
    }

    fn spawn_trust_watch(self: &Arc<Self>) {
        let this = Arc::downgrade(self);
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(TRUST_POLL);
            loop {
                tick.tick().await;
                let Some(host) = this.upgrade() else { break };
                host.enforce_trust(false);
            }
        });
    }

    /// Re-read the trust store if it changed (or `force`), and end every
    /// session of a device that is no longer in it. Returns how many ended.
    pub(crate) fn enforce_trust(&self, force: bool) -> usize {
        let trust = &mut *self.trust.lock().unwrap();
        if force || trust.is_stale() {
            if let Err(e) = trust.reload() {
                warn!("the trust store can't be read, so no device is trusted until it is fixed: {e:#}");
            }
            if trust.clients().is_empty() && self.capture_kind == CaptureKind::Portal {
                // Every grant is revoked: the saved portal grant goes too.
                let _ = portal::remove_token();
            }
        }
        let ended = self.sessions.kill_where(|_, fp| !trust.contains(fp));
        if ended > 0 {
            info!("ended {ended} session(s) of devices that are no longer trusted");
        }
        ended
    }

    pub(crate) fn is_trusted(&self, fp: &tls::Fingerprint) -> bool {
        let mut trust = self.trust.lock().unwrap();
        let reloaded = if trust.is_stale() { trust.reload() } else { Ok(()) };
        if let Err(e) = reloaded {
            warn!("the trust store can't be read: {e:#}");
        }
        trust.contains(fp)
    }

    async fn handle(self: &Arc<Self>, mut acc: Accepted, permit: PendingPermit) -> Result<()> {
        let hello = tokio::time::timeout(self.timeouts.auth, self.authenticate(&mut acc))
            .await
            .map_err(|_| anyhow!("authentication took too long"))??;

        // From here on the host can end this session at any moment.
        let ticket = self.sessions.register(acc.client_fp, &hello.name, acc.peer.ip());
        let killed = ticket.killed();
        let result = tokio::select! {
            r = self.serve(&mut acc, &ticket, permit, &hello) => r,
            _ = killed => Err(anyhow!("the host ended this session")),
        };
        if let Err(e) = &result {
            let _ = acc.signal.send(&HostMsg::Error { message: format!("{e:#}") }).await;
        }
        acc.signal.close().await;
        info!("{} ({}) disconnected", hello.name, acc.peer);
        drop(ticket);
        self.release_portal_if_idle().await;
        result
    }

    /// `Hello`, pairing if needed, `Welcome`.
    async fn authenticate(&self, acc: &mut Accepted) -> Result<HelloInfo> {
        let ClientMsg::Hello { version, name, codecs, hw_codecs, max_fps, audio, pair } =
            acc.signal.expect_within(self.timeouts.hello, "Hello").await?
        else {
            bail!("expected Hello");
        };
        let name: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
        if version != PROTOCOL_VERSION {
            let message = format!("this host speaks protocol {PROTOCOL_VERSION}, the client {version}; update both");
            acc.signal.send(&HostMsg::Error { message: message.clone() }).await?;
            bail!(message);
        }
        if pair || !self.is_trusted(&acc.client_fp) {
            self.pair(acc, &name).await?;
        }
        acc.signal.send(&HostMsg::Welcome { host_name: self.cfg.name.clone() }).await?;
        let few = |v: Vec<VideoCodec>| v.into_iter().take(8).collect::<Vec<_>>();
        let caps = ClientCaps {
            codecs: few(codecs),
            hw_codecs: few(hw_codecs),
            max_fps: if max_fps == 0 { 60 } else { max_fps.min(240) },
        };
        Ok(HelloInfo { name, caps, audio })
    }

    async fn pair(&self, acc: &mut Accepted, name: &str) -> Result<()> {
        let source = limits::source(acc.peer.ip());
        let begun = self.pairing.lock().unwrap().begin(source, Instant::now());
        if let Err(denied) = begun {
            let message = denied.to_string();
            acc.signal.send(&HostMsg::Error { message: message.clone() }).await?;
            bail!(message);
        }
        let mut attempt = PairAttempt { limiter: self.pairing.clone(), source, ok: false };

        let (pin, _shown) = match &self.cfg.password {
            Some(p) => (p.clone(), None),
            None => {
                let pin = pair::new_pin();
                let shown = self.pins.show(name, acc.peer.ip(), &pin).await;
                (pin, Some(shown))
            }
        };
        acc.signal
            .send(&HostMsg::PairingRequired {
                host_name: self.cfg.name.clone(),
                password: self.cfg.password.is_some(),
            })
            .await?;

        let ClientMsg::PairStart { spake } = acc.signal.expect_within(self.timeouts.pair, "the PIN").await? else {
            bail!("expected PairStart");
        };
        let fail = |e: anyhow::Error| e.context(format!("pairing with {name} failed"));
        let (state, reply, mac) =
            pair::host_respond(&pin, &spake, &self.identity.fingerprint, &acc.client_fp).map_err(fail)?;
        acc.signal.send(&HostMsg::PairReply { spake: reply, mac }).await?;
        // A client that saw a wrong PIN hangs up instead of answering.
        let ClientMsg::PairFinish { mac } =
            acc.signal.expect_within(self.timeouts.pair_finish, "PairFinish").await.map_err(fail)?
        else {
            bail!("expected PairFinish");
        };
        state.verify(&mac).map_err(fail)?;

        self.trust.lock().unwrap().add(&acc.client_fp, name)?;
        attempt.ok = true;
        info!("paired with {name}");
        Ok(())
    }

    /// Everything after `Welcome`: approval, display choice, streaming.
    async fn serve(&self, acc: &mut Accepted, ticket: &Ticket, permit: PendingPermit, hello: &HelloInfo) -> Result<()> {
        // It may have been revoked between authenticating and registering.
        if !self.is_trusted(&acc.client_fp) {
            bail!("this device is no longer trusted");
        }
        let grant = self.approve(acc, ticket, &hello.name).await?;
        // Approved: it stops counting as pending, and counts as a session.
        drop(permit);
        let _quota = self
            .session_quota
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow!("this host has too many connections open"))?;
        acc.signal.send(&HostMsg::Approved { control: grant.control, audio: grant.audio }).await?;

        // Only the displays this session may see (this is also what opens
        // the Wayland share dialog).
        let monitors: Vec<MonitorInfo> =
            self.monitors().await?.into_iter().filter(|m| grant.allows_display(m.index)).collect();
        if monitors.is_empty() {
            bail!("no display is shared with this device");
        }
        acc.signal.send(&HostMsg::Monitors { monitors: monitors.clone() }).await?;
        let index = match acc.signal.expect_within::<ClientMsg>(self.timeouts.select, "a display choice").await? {
            ClientMsg::SelectMonitor { index } => index,
            _ => bail!("expected SelectMonitor"),
        };
        if !monitors.iter().any(|m| m.index == index) {
            bail!("display {index} isn't shared with this device");
        }

        // Reserve the display: one stream each, up to the overall cap.
        {
            let mut active = self.active.lock().unwrap();
            if active.contains(&index) {
                bail!("that display is already being streamed");
            } else if active.len() >= MAX_STREAMS {
                bail!("this host is already streaming {MAX_STREAMS} displays");
            }
            active.insert(index);
        }
        let _guard = StreamGuard { host: self, monitor: index };
        ticket.set_display(index);
        info!(
            "{} ({}) streaming display {index}{}",
            hello.name,
            acc.peer,
            if grant.control { "" } else { ", view only" }
        );
        self.stream(acc, hello, &grant, index, ticket.active.clone()).await
    }

    /// Decide what this session may do. Attended (the default): ask the
    /// person at the host, unless this device is already in an approved
    /// visit (another of its windows is open). Unattended: allow, and say so.
    async fn approve(&self, acc: &mut Accepted, ticket: &Ticket, name: &str) -> Result<Grant> {
        let full = Grant { control: true, audio: self.cfg.audio, displays: self.cfg.displays.clone() };
        let grant = if self.cfg.unattended {
            self.announce_connected(name, acc.peer.ip()).await;
            full
        } else if let Some(g) = self.sessions.grant_of(&acc.client_fp, ticket.id) {
            g
        } else {
            acc.signal.send(&HostMsg::AwaitingApproval).await?;
            let device: String = tls::fingerprint_display(&acc.client_fp).chars().take(19).collect();
            let decision = tokio::select! {
                d = self.approver.ask(ticket.id, name, acc.peer.ip(), &device) => d,
                _ = acc.signal.recv::<ClientMsg>() => bail!("the client left before the host answered"),
            };
            match decision {
                Decision::Deny => bail!("the host declined the connection"),
                Decision::ViewOnly => Grant { control: false, ..full },
                Decision::Control => full,
            }
        };
        ticket.approve(grant.clone());
        Ok(grant)
    }

    async fn stream(
        &self,
        acc: &mut Accepted,
        hello: &HelloInfo,
        grant: &Grant,
        monitor: u32,
        active: Arc<AtomicBool>,
    ) -> Result<()> {
        let cfg = &self.cfg;
        // Capture at the host's framerate cap; the planner's chosen rate is
        // applied downstream, so we never encode faster than the client wants.
        let capture = self.open_capture(cfg.fps.max(1), monitor, grant.control).await?;
        let (width, height) = (capture.width, capture.height);

        // Weigh encoders, codecs, link type and resolution into one plan.
        let overrides = Overrides {
            encoder: cfg.encoder.clone(),
            fps: cfg.fps,
            start_kbps: cfg.start_kbps,
            max_kbps: cfg.max_kbps,
        };
        let plan = quality::plan(&video::available(), &hello.caps, acc.local_ip, acc.peer.ip(), width, height, &overrides)?;
        let enc = plan.encoder;
        self.budget.init_ceiling(plan.lan);

        // Only the first display to want audio gets it; others would just
        // duplicate the same desktop sound.
        let give_audio = grant.audio && hello.audio && {
            let mut owner = self.audio_owner.lock().unwrap();
            if owner.is_none() {
                *owner = Some(monitor);
                true
            } else {
                false
            }
        };

        let (ev_tx, mut ev_rx) = mpsc::channel::<SessionEvent>(STREAM_EVENT_QUEUE);
        let (end_tx, mut end_rx) = mpsc::unbounded_channel::<StreamEnd>();
        let on_error = |tx: mpsc::UnboundedSender<StreamEnd>| move |e: String| drop(tx.send(StreamEnd::Failed(e)));
        let video = VideoPipeline::start(&capture.source, enc, plan.fps, plan.start_kbps, plan.max_kbps, on_error(end_tx.clone()))?;
        let audio = if give_audio {
            AudioPipeline::start(on_error(end_tx.clone()))
                .inspect_err(|e| warn!("no audio: {e:#}"))
                .ok()
        } else {
            None
        };
        let mut cleanup = StreamCleanup { video: video.clone(), audio: audio.clone(), session: None };

        let opts = OfferOptions {
            codec: enc.codec,
            audio: audio.is_some(),
            start_bitrate: plan.start_kbps as u64 * 1000,
            ports: cfg.media_ports,
        };
        let (pending, sdp) = rtc::create_offer(acc.local_ip, &opts)?;
        acc.signal.send(&HostMsg::Offer { sdp, codec: enc.codec, width, height, fps: plan.fps }).await?;
        let ClientMsg::Answer { sdp } = acc.signal.expect_within(self.timeouts.answer, "the answer").await? else {
            bail!("expected Answer");
        };

        let handler = {
            let video = video.clone();
            // The permission check happens here, on the host, for every event.
            let mut gate = InputGate::new(capture.input.clone(), active);
            let mut abusive = false;
            let end = end_tx.clone();
            Box::new(move |ev: SessionEvent| match ev {
                // These two are latency-critical, so they skip the queue.
                SessionEvent::KeyframeRequested => video.force_keyframe(),
                SessionEvent::Data(Channel::Input, bytes) => {
                    if abusive {
                        return;
                    }
                    if let Verdict::Abuse(why) = gate.handle(&bytes) {
                        abusive = true;
                        let _ = end.send(StreamEnd::Failed(format!("the client sent too much input ({why})")));
                    }
                }
                SessionEvent::Closed(err) => drop(end.send(StreamEnd::Closed(err))),
                // Anything else waits in a bounded queue, or is dropped.
                other => drop(ev_tx.try_send(other)),
            })
        };
        drop(end_tx);
        let session = pending.accept(&sdp, handler)?;
        cleanup.session = Some(session.clone());
        video.attach(session.clone());
        if let Some(a) = &audio {
            a.attach(session.clone());
        }
        // Join the budget pool; this sets the initial ceiling and framerate,
        // and re-balances any display already streaming.
        self.budget.register(StreamSlot {
            monitor,
            res_cap_kbps: plan.max_kbps,
            pixels: width as u64 * height as u64,
            fps_cap: plan.fps,
            video: Arc::downgrade(&video),
            session: session.clone(),
        });
        info!(
            "streaming display {monitor} {width}x{height}@{} {} via {} to {} over {}",
            plan.fps,
            enc.codec.name(),
            enc.element,
            hello.name,
            if plan.lan { "LAN" } else { "the internet" },
        );

        let stats_every = Duration::from_secs(2);
        let mut stats_timer = tokio::time::interval_at(tokio::time::Instant::now() + stats_every, stats_every);
        let mut last_stats = Instant::now();
        let mut pings = Bucket { tokens: CONTROL_BURST, last: Instant::now() };
        loop {
            tokio::select! {
                msg = acc.signal.recv::<ClientMsg>() => match msg {
                    Ok(None) => return Ok(()),
                    Ok(Some(m)) => debug!("ignoring {m:?}"),
                    Err(e) => return Err(e),
                },
                Some(end) = end_rx.recv() => match end {
                    StreamEnd::Closed(err) => return err.map_or(Ok(()), |e| Err(anyhow!(e))),
                    StreamEnd::Failed(e) => return Err(anyhow!(e)),
                },
                Some(ev) = ev_rx.recv() => match ev {
                    SessionEvent::Connected => video.force_keyframe(),
                    SessionEvent::ChannelOpen(Channel::Control) => {
                        session.send(Channel::Control, encode(&ControlMsg::VideoSize { width, height }));
                    }
                    SessionEvent::Data(Channel::Control, bytes) => {
                        if bytes.len() > MAX_CONTROL_MESSAGE || !pings.take() {
                            continue;
                        }
                        match decode::<ControlMsg>(&bytes) {
                            Ok(ControlMsg::Ping { t }) => session.send(Channel::Control, encode(&ControlMsg::Pong { t })),
                            Ok(m) => debug!("control: {m:?}"),
                            Err(e) => debug!("control: {e}"),
                        }
                    }
                    SessionEvent::BitrateEstimate(bps) => video.on_bandwidth_estimate(bps),
                    _ => {}
                },
                _ = stats_timer.tick() => {
                    let s = video.take_stats(last_stats.elapsed());
                    last_stats = Instant::now();
                    debug!("video {:.0} fps, {:.0} kbit/s (target {})", s.fps, s.kbps, video.bitrate_kbps());
                    let drops = session.take_drops();
                    if drops.video_frames > 0 || drops.other > 0 {
                        warn!(
                            "display {monitor}: dropped {} video frames and {} other messages in {stats_every:?}; \
                             the link or this computer can't keep up",
                            drops.video_frames, drops.other
                        );
                    }
                    let stats = ControlMsg::Stats {
                        bitrate_kbps: s.kbps as u32,
                        fps: s.fps.round() as u32,
                        encoder: enc.element.to_string(),
                    };
                    session.send(Channel::Control, encode(&stats));
                }
            }
        }
    }

    /// A human-readable list for `linuxplay-host sessions`.
    fn describe(&self) -> String {
        let mut out = String::new();
        for (who, pin) in self.pins.list() {
            out.push_str(&format!("pairing   {who}  PIN {pin}\n"));
        }
        for (id, who) in self.approver.waiting() {
            out.push_str(&format!(
                "waiting   [{id}] {who}  (linuxplay-host approve {id} [--view-only] | deny {id})\n"
            ));
        }
        for s in self.sessions.snapshot() {
            let State::Approved(g) = &s.state else { continue };
            out.push_str(&format!(
                "connected [{}] {} ({}){}  {}  device {}\n",
                s.id,
                s.name,
                s.ip,
                s.display.map(|d| format!("  display {d}")).unwrap_or_default(),
                if g.control { "control" } else { "view only" },
                &s.fingerprint[..16],
            ));
        }
        if out.is_empty() {
            out.push_str("nobody is connected\n");
        }
        out
    }

    /// Let the user know the host is up, even with no terminal or tray.
    async fn announce_started(&self, shown: &str) {
        let Some(n) = &self.notifier else { return };
        let body = format!(
            "Listening on {shown} ({}). {}",
            self.cfg.exposure,
            if self.cfg.unattended {
                "Unattended: paired devices connect without asking."
            } else {
                "You'll be asked before anyone connects."
            }
        );
        let note = Note { summary: "LinuxPlay Host is running", body: &body, actions: &[], urgent: false, transient: true };
        n.show(note, None).await;
    }

    /// The unattended-mode indicator: say who just connected.
    async fn announce_connected(&self, name: &str, ip: IpAddr) {
        info!("{name} ({ip}) connected without approval (unattended)");
        let Some(n) = &self.notifier else { return };
        let body = format!("{name} ({ip}) connected (unattended access). Disconnect from the tray or with `linuxplay-host disconnect all`.");
        let note = Note { summary: "LinuxPlay: device connected", body: &body, actions: &[], urgent: false, transient: false };
        n.show(note, None).await;
    }
}

impl control::Handler for Host {
    async fn control(&self, cmd: Command) -> String {
        match cmd {
            Command::Reload => format!("ok: ended {} live session(s)\n", self.enforce_trust(true)),
            Command::List => self.describe(),
            Command::Decide(id, d) => {
                if self.approver.resolve(id, d) {
                    "ok\n".into()
                } else {
                    format!("error: no connection [{id}] is waiting for approval\n")
                }
            }
            Command::Disconnect(None) => format!("ok: ended {} session(s)\n", self.sessions.kill_all()),
            Command::Disconnect(Some(id)) => {
                if self.sessions.kill(id) {
                    "ok\n".into()
                } else {
                    format!("error: no session [{id}]\n")
                }
            }
        }
    }
}
