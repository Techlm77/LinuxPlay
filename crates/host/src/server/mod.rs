//! Accepting clients: limits, authentication, pairing, approval by the person
//! at the host, then one stream per display.
//!
//! Every connection is in one of three phases, each with its own limits:
//! authenticating (counted against the pending limits, with a deadline for
//! every stage), waiting for approval, and approved (counted against the
//! session cap). From `Welcome` on it is registered in [`Sessions`], so it can
//! be ended at once when its device is revoked or sharing is stopped.

mod account;
mod budget;
mod desktop;
mod handshake;
mod stream;

use std::collections::{BTreeSet, HashMap, HashSet};
use std::net::{IpAddr, SocketAddr};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use linuxplay_net::exposure::Exposure;
use linuxplay_net::rendezvous::Account;
use linuxplay_net::tls::{self, Identity};
use linuxplay_net::ws::{self, Accepted};
use linuxplay_proto::api;
use linuxplay_proto::input::InputEvent;
use linuxplay_proto::signal::{HostMsg, MonitorInfo};
use tokio::net::{TcpListener, TcpStream};
use tokio::sync::{Mutex as AsyncMutex, Notify, Semaphore, mpsc};
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, warn};

use crate::approval::{Approver, Decision, PinBoard};
use crate::capture::{self, Capture, CaptureKind, TestPicture};
use crate::control::{self, Command};
use crate::invites::Invites;
use crate::limits::{PairLimiter, Pending, PendingPermit};
use crate::netif::{self, ListenMode};
use crate::notify::Notifier;
use crate::clipboard::Clipboard;
use crate::portal::{self, PortalHost, PortalOptions};
use crate::sessions::{Sessions, State};
use crate::trust::TrustStore;

use self::budget::Budget;

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
    /// What the test capture shows.
    pub test_picture: TestPicture,
    /// On Wayland, also offer a new virtual screen in the share dialog.
    pub virtual_display: bool,
    /// The account this computer is signed in to, if any: it makes the host
    /// reachable through the account's rendezvous server.
    pub account: Option<Account>,
}

impl HostConfig {
    /// The settings of a host started with no options: this computer's local
    /// network and, when it is connected, Tailscale; every display, sound on,
    /// and every connection approved by the person at it.
    pub fn standard(name: String) -> Result<Self> {
        let mut listen = netif::resolve(ListenMode::Lan, &[]).unwrap_or_default();
        let tailscale = linuxplay_net::netif::tailscale_addresses().unwrap_or_default();
        let exposure = if tailscale.is_empty() { "local network only" } else { "local network and Tailscale" };
        listen.extend(tailscale);
        if listen.is_empty() {
            anyhow::bail!("this computer has no local network or Tailscale address to share on");
        }
        Ok(Self {
            name,
            listen,
            exposure: exposure.to_string(),
            port: linuxplay_proto::DEFAULT_PORT,
            media_ports: Some(linuxplay_proto::DEFAULT_MEDIA_PORTS),
            capture: None,
            encoder: None,
            fps: 60,
            start_kbps: 0,
            max_kbps: 0,
            audio: true,
            unattended: false,
            password: None,
            displays: None,
            test_picture: TestPicture::default(),
            virtual_display: false,
            account: None,
        })
    }
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
/// How often the trust store is checked for changes made elsewhere.
const TRUST_POLL: Duration = Duration::from_secs(2);

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
    /// Fires when the host should stop.
    quit: Arc<Notify>,
    /// Whether the internet can reach this host, which it shouldn't.
    exposure: Exposure,
    /// This desktop's clipboard, shared with sessions that may control it.
    pub(crate) clipboard: Clipboard,
    /// Invites handed out, and which sessions came by which invite. Guests
    /// are not trusted devices, so the trust store says nothing about them.
    invites: Invites,
    guests: Mutex<HashMap<u64, String>>,
    /// Input "injected" by the test capture.
    pub(crate) test_input: Arc<Mutex<Vec<InputEvent>>>,
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
        let exposure = Exposure::new(&cfg.listen.iter().map(|ip| SocketAddr::new(*ip, cfg.port)).collect::<Vec<_>>());
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
            quit: Arc::default(),
            exposure,
            clipboard: Clipboard::default(),
            invites: Invites::default(),
            guests: Mutex::default(),
            test_input: Arc::default(),
        }))
    }

    /// The Wayland portal session, created (with its share dialog) on first use.
    async fn portal(&self) -> Result<Arc<PortalHost>> {
        let mut guard = self.portal.lock().await;
        if let Some(p) = guard.as_ref() {
            return Ok(p.clone());
        }
        let options = PortalOptions {
            persist: self.cfg.unattended,
            virtual_display: self.cfg.virtual_display,
            clipboard: self.clipboard.port(),
        };
        let p = PortalHost::create(options).await?;
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
            CaptureKind::Test => Ok(capture::monitors_test(self.cfg.test_picture)),
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
                let picture = self.cfg.test_picture;
                let monitor = find(capture::monitors_test(picture))?;
                Ok(capture::open_test(fps, &monitor, picture.pattern, control, self.test_input.clone()))
            }
        }
    }

    /// Stop sharing: [`run`](Self::run) ends every session and returns.
    pub fn stop(&self) {
        self.quit.notify_one();
    }

    /// What the host is doing right now, for a front end to show.
    pub fn status(&self) -> api::Hosting {
        let sessions = self
            .sessions
            .snapshot()
            .into_iter()
            .filter_map(|s| match s.state {
                State::Approved(grant) => Some(api::Session {
                    id: s.id,
                    name: s.name,
                    address: s.ip.to_string(),
                    display: s.display,
                    control: grant.control,
                }),
                State::Waiting => None,
            })
            .collect();
        api::Hosting {
            warning: self.exposure.warning(),
            // The engine fills this in when it runs the account server.
            account_server: None,
            unattended: self.cfg.unattended,
            pins: self.pins.list().into_iter().map(|(who, pin)| api::PairingPin { who, pin }).collect(),
            waiting: self.approver.waiting().into_iter().map(|(id, who)| api::WaitingConnection { id, who }).collect(),
            sessions,
            invites: self
                .invites
                .list()
                .into_iter()
                .map(|(id, control, left)| api::InviteInfo { id, control, minutes_left: left.as_secs().div_ceil(60) })
                .collect(),
        }
    }

    /// Answer a connection that is waiting for approval. Returns false if
    /// no connection with this id is waiting.
    pub fn decide(&self, id: u64, decision: Decision) -> bool {
        self.approver.resolve(id, decision)
    }

    /// End one session, or all of them. Returns how many ended.
    pub fn disconnect(&self, id: Option<u64>) -> usize {
        match id {
            Some(id) => self.sessions.kill(id) as usize,
            None => self.sessions.kill_all(),
        }
    }

    /// The devices this host lets connect.
    pub fn trusted(&self) -> Vec<api::TrustedDevice> {
        let trust = self.trust.lock().unwrap();
        trust.clients().iter().map(|c| api::TrustedDevice { name: c.name.clone(), fingerprint: c.fingerprint.clone() }).collect()
    }

    /// Forget the device whose fingerprint starts with `prefix` and end its
    /// sessions. Returns how many devices were forgotten.
    pub fn revoke(&self, prefix: &str) -> Result<usize> {
        let removed = self.trust.lock().unwrap().revoke(prefix)?.len();
        self.enforce_trust(true);
        Ok(removed)
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

        let quit = self.quit.clone();
        tokio::spawn(control::serve(control_listener, self.clone()));
        if self.capture_kind == CaptureKind::X11 {
            crate::clipboard::x11::spawn(std::env::var("DISPLAY").ok(), self.clipboard.port());
        }
        if let Some(account) = self.cfg.account.clone() {
            tokio::spawn(self.clone().stay_reachable(account));
        }
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
            // Someone on the internet got here: a port is open on the router.
            if self.exposure.contact(peer.ip()) {
                let host = self.clone();
                tokio::spawn(async move { host.announce_exposed(peer.ip()).await });
            }
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
        let guests = self.guests.lock().unwrap();
        let ended = self.sessions.kill_where(|id, fp| !guests.contains_key(&id) && !trust.contains(fp));
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
        let ticket = self.sessions.register(acc.client_fp, &hello.name, acc.route.peer_ip());
        if let Some(invite) = &acc.invite {
            self.guests.lock().unwrap().insert(ticket.id, invite.clone());
        }
        let killed = ticket.killed();
        let result = tokio::select! {
            r = self.serve(&mut acc, &ticket, permit, &hello) => r,
            _ = killed => Err(anyhow!("the host ended this session")),
        };
        if let Err(e) = &result {
            let _ = acc.signal.send(&HostMsg::Error { message: format!("{e:#}") }).await;
        }
        acc.signal.close().await;
        info!("{} ({}) disconnected", hello.name, acc.route);
        self.guests.lock().unwrap().remove(&ticket.id);
        drop(ticket);
        self.release_portal_if_idle().await;
        result
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
        for (id, control, left) in self.invites.list() {
            out.push_str(&format!(
                "invite    {id}  {}  {} min left\n",
                if control { "control" } else { "view only" },
                left.as_secs().div_ceil(60)
            ));
        }
        out
    }
}

impl control::Handler for Host {
    async fn control(&self, cmd: Command) -> String {
        match cmd {
            Command::Reload => format!("ok: ended {} live session(s)\n", self.enforce_trust(true)),
            Command::List => self.describe(),
            Command::Decide(id, d) => {
                if self.decide(id, d) {
                    "ok\n".into()
                } else {
                    format!("error: no connection [{id}] is waiting for approval\n")
                }
            }
            Command::Disconnect(None) => format!("ok: ended {} session(s)\n", self.disconnect(None)),
            Command::Disconnect(Some(id)) => {
                if self.disconnect(Some(id)) == 1 {
                    "ok\n".into()
                } else {
                    format!("error: no session [{id}]\n")
                }
            }
            Command::Invite { minutes, control } => match self.invite(minutes, control).await {
                Ok(link) => format!("ok: {link}\n"),
                Err(e) => format!("error: {e:#}\n"),
            },
            Command::Uninvite(id) => match self.uninvite(&id).await {
                Ok(ended) => format!("ok: withdrawn; ended {ended} session(s)\n"),
                Err(e) => format!("error: {e:#}\n"),
            },
        }
    }
}
