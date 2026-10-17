//! Accepting clients: authentication, pairing, then one stream per display.

use std::collections::HashSet;
use std::net::IpAddr;
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use tokio::sync::{Mutex as AsyncMutex, Semaphore};

use anyhow::{Context, Result, anyhow, bail};
use linuxplay_net::rtc::{self, OfferOptions};
use linuxplay_net::tls::Identity;
use linuxplay_net::ws::{self, Accepted};
use linuxplay_net::{Channel, SessionEvent, SessionHandle, pair};
use linuxplay_proto::control::ControlMsg;
use linuxplay_proto::input::InputEvent;
use linuxplay_proto::signal::{ClientMsg, HostMsg, MonitorInfo};
use linuxplay_proto::{PROTOCOL_VERSION, decode, encode};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, warn};

use crate::audio::AudioPipeline;
use crate::capture::{self, Capture, CaptureKind};
use crate::portal::PortalHost;
use crate::quality::{self, ClientCaps, Overrides};
use crate::trust::TrustStore;
use crate::video::{self, VideoPipeline};

pub struct HostConfig {
    pub name: String,
    pub bind: IpAddr,
    pub port: u16,
    pub capture: Option<CaptureKind>,
    pub encoder: Option<String>,
    pub fps: u32,
    pub start_kbps: u32,
    pub max_kbps: u32,
    pub audio: bool,
    /// Permanent pairing password; without it a one-time PIN is shown.
    pub password: Option<String>,
}

/// Wrong PINs allowed before pairing is locked for a while.
const MAX_PAIR_FAILURES: u32 = 3;
const PAIR_LOCKOUT: Duration = Duration::from_secs(60);
/// How long the user has to type the PIN on the client.
const PAIR_TIMEOUT: Duration = Duration::from_secs(120);
/// Most displays that may stream at once.
const MAX_STREAMS: usize = 4;
/// Roughly the encoder's throughput, in pixels per second, split across the
/// active streams. About three 1080p60 streams; above it, framerates drop.
const ENCODER_PIXEL_BUDGET: u64 = 1920 * 1080 * 60 * 3;

#[derive(Default)]
struct PairLimiter {
    failures: u32,
    locked_until: Option<Instant>,
}

/// Cap on connections that have not finished authenticating, so a flood of
/// half-open TLS/pairing attempts can't pile up tasks on the host.
const MAX_PENDING: usize = 16;

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

enum StreamEvent {
    Session(SessionEvent),
    Failed(String),
}

pub struct Host {
    cfg: HostConfig,
    identity: Identity,
    capture_kind: CaptureKind,
    trust: Mutex<TrustStore>,
    limiter: Mutex<PairLimiter>,
    pending: Arc<Semaphore>,
    /// Lazily-created Wayland portal session, shared by all streams.
    portal: AsyncMutex<Option<Arc<PortalHost>>>,
    /// Monitor indices currently streaming.
    active: Mutex<HashSet<u32>>,
    /// Which display, if any, currently carries the audio.
    audio_owner: Mutex<Option<u32>>,
    budget: Budget,
}

/// Frees a display's reservation (and its share of the budget) on drop.
struct StreamGuard {
    host: Arc<Host>,
    monitor: u32,
}

impl Drop for StreamGuard {
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

impl Host {
    pub fn new(cfg: HostConfig, identity: Identity, trust: TrustStore) -> Result<Arc<Self>> {
        let capture_kind = match cfg.capture {
            Some(k) => k,
            None => CaptureKind::detect()?,
        };
        Ok(Arc::new(Self {
            cfg,
            identity,
            capture_kind,
            trust: Mutex::new(trust),
            limiter: Mutex::default(),
            pending: Arc::new(Semaphore::new(MAX_PENDING)),
            portal: AsyncMutex::new(None),
            active: Mutex::default(),
            audio_owner: Mutex::default(),
            budget: Budget::default(),
        }))
    }

    /// The Wayland portal session, created (with its share dialog) on first use.
    async fn portal(&self) -> Result<Arc<PortalHost>> {
        let mut guard = self.portal.lock().await;
        if let Some(p) = guard.as_ref() {
            return Ok(p.clone());
        }
        let p = PortalHost::create().await?;
        *guard = Some(p.clone());
        Ok(p)
    }

    /// The displays this host can share.
    async fn monitors(&self) -> Result<Vec<MonitorInfo>> {
        match self.capture_kind {
            CaptureKind::X11 => capture::monitors_x11(),
            CaptureKind::Portal => Ok(self.portal().await?.monitors()),
        }
    }

    /// Open a capture of one display.
    async fn open_capture(&self, fps: u32, index: u32) -> Result<Capture> {
        match self.capture_kind {
            CaptureKind::X11 => {
                let monitors = capture::monitors_x11()?;
                let mon = monitors
                    .iter()
                    .find(|m| m.index == index)
                    .ok_or_else(|| anyhow!("no such display {index}"))?;
                capture::open_x11(fps, mon)
            }
            CaptureKind::Portal => {
                let stream = self.portal().await?.open_stream(index, fps).await?;
                Ok(capture::from_portal(stream))
            }
        }
    }

    pub async fn run(self: Arc<Self>) -> Result<()> {
        let acceptor = TlsAcceptor::from(Arc::new(self.identity.server_config()?));
        let listener = TcpListener::bind((self.cfg.bind, self.cfg.port))
            .await
            .with_context(|| format!("listening on {}:{}", self.cfg.bind, self.cfg.port))?;
        info!("listening on {}", listener.local_addr()?);
        loop {
            let (tcp, peer) = listener.accept().await?;
            // Drop new connections while too many are still authenticating.
            let Ok(permit) = self.pending.clone().try_acquire_owned() else {
                debug!("{peer}: refused, too many pending connections");
                continue;
            };
            let host = self.clone();
            let acceptor = acceptor.clone();
            tokio::spawn(async move {
                let _permit = permit;
                match ws::accept(tcp, &acceptor).await {
                    Ok(acc) => {
                        if let Err(e) = host.handle(acc).await {
                            warn!("{peer}: {e:#}");
                        }
                    }
                    Err(e) => debug!("{peer}: {e:#}"),
                }
            });
        }
    }

    async fn handle(self: &Arc<Self>, mut acc: Accepted) -> Result<()> {
        let ClientMsg::Hello { version, name, codecs, hw_codecs, max_fps, audio, pair } =
            acc.signal.expect().await?
        else {
            bail!("expected Hello");
        };
        let name: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
        if version != PROTOCOL_VERSION {
            let message = format!("this host speaks protocol {PROTOCOL_VERSION}, the client {version}; update both");
            acc.signal.send(&HostMsg::Error { message: message.clone() }).await?;
            bail!(message);
        }

        let known = self.trust.lock().unwrap().get(&acc.client_fp).is_some();
        if pair || !known {
            self.pair(&mut acc, &name).await?;
        }
        acc.signal.send(&HostMsg::Welcome { host_name: self.cfg.name.clone() }).await?;

        // Offer the displays (this is what opens the Wayland share dialog).
        let monitors = match self.monitors().await {
            Ok(m) => m,
            Err(e) => {
                let _ = acc.signal.send(&HostMsg::Error { message: format!("{e:#}") }).await;
                return Err(e);
            }
        };
        acc.signal.send(&HostMsg::Monitors { monitors: monitors.clone() }).await?;

        let index = match acc.signal.expect::<ClientMsg>().await? {
            ClientMsg::SelectMonitor { index } => index,
            _ => bail!("expected SelectMonitor"),
        };
        if !monitors.iter().any(|m| m.index == index) {
            let message = format!("this host has no display {index}");
            acc.signal.send(&HostMsg::Error { message: message.clone() }).await?;
            bail!(message);
        }

        // Reserve the display: one stream each, up to the overall cap.
        let reject = {
            let mut active = self.active.lock().unwrap();
            if active.contains(&index) {
                Some("that display is already being streamed".to_string())
            } else if active.len() >= MAX_STREAMS {
                Some(format!("this host is already streaming {MAX_STREAMS} displays"))
            } else {
                active.insert(index);
                None
            }
        };
        if let Some(message) = reject {
            acc.signal.send(&HostMsg::Error { message: message.clone() }).await?;
            bail!(message);
        }
        let _guard = StreamGuard { host: self.clone(), monitor: index };

        info!("{name} ({}) streaming display {index}", acc.peer);
        let caps = ClientCaps {
            codecs,
            hw_codecs,
            max_fps: if max_fps == 0 { 60 } else { max_fps },
        };
        let result = self.stream(&mut acc, &name, &caps, audio, index).await;
        if let Err(e) = &result {
            let _ = acc.signal.send(&HostMsg::Error { message: format!("{e:#}") }).await;
        }
        acc.signal.close().await;
        info!("{name} disconnected from display {index}");
        result
    }

    async fn pair(&self, acc: &mut Accepted, name: &str) -> Result<()> {
        let locked_for = {
            let mut lim = self.limiter.lock().unwrap();
            match lim.locked_until {
                Some(until) if Instant::now() < until => Some(until - Instant::now()),
                Some(_) => {
                    *lim = PairLimiter::default();
                    None
                }
                None => None,
            }
        };
        if let Some(left) = locked_for {
            let message = format!("too many wrong PINs; try again in {} s", left.as_secs() + 1);
            acc.signal.send(&HostMsg::Error { message: message.clone() }).await?;
            bail!(message);
        }

        let pin = match &self.cfg.password {
            Some(p) => p.clone(),
            None => {
                let pin = pair::new_pin();
                announce_pin(name, acc.peer.ip(), &pin);
                pin
            }
        };
        acc.signal
            .send(&HostMsg::PairingRequired {
                host_name: self.cfg.name.clone(),
                password: self.cfg.password.is_some(),
            })
            .await?;

        let ClientMsg::PairStart { spake } = tokio::time::timeout(PAIR_TIMEOUT, acc.signal.expect())
            .await
            .map_err(|_| anyhow!("pairing timed out"))??
        else {
            bail!("expected PairStart");
        };

        let outcome = async {
            let (state, reply, mac) = pair::host_respond(&pin, &spake, &self.identity.fingerprint, &acc.client_fp)?;
            acc.signal.send(&HostMsg::PairReply { spake: reply, mac }).await?;
            // A client that saw a wrong PIN hangs up instead of answering.
            let ClientMsg::PairFinish { mac } = acc.signal.expect().await? else {
                bail!("expected PairFinish");
            };
            state.verify(&mac)
        }
        .await;

        match outcome {
            Ok(()) => {
                *self.limiter.lock().unwrap() = PairLimiter::default();
                self.trust.lock().unwrap().add(&acc.client_fp, name)?;
                info!("paired with {name}");
                Ok(())
            }
            Err(e) => {
                let mut lim = self.limiter.lock().unwrap();
                lim.failures += 1;
                if lim.failures >= MAX_PAIR_FAILURES {
                    lim.locked_until = Some(Instant::now() + PAIR_LOCKOUT);
                    warn!("pairing locked for {PAIR_LOCKOUT:?} after {} failures", lim.failures);
                }
                Err(e.context(format!("pairing with {name} failed")))
            }
        }
    }

    async fn stream(&self, acc: &mut Accepted, name: &str, caps: &ClientCaps, want_audio: bool, monitor: u32) -> Result<()> {
        let cfg = &self.cfg;
        // Capture at the host's framerate cap; the planner's chosen rate is
        // applied downstream, so we never encode faster than the client wants.
        let capture = self.open_capture(cfg.fps.max(1), monitor).await?;
        let (width, height) = (capture.width, capture.height);

        // Weigh encoders, codecs, link type and resolution into one plan.
        let overrides = Overrides {
            encoder: cfg.encoder.clone(),
            fps: cfg.fps,
            start_kbps: cfg.start_kbps,
            max_kbps: cfg.max_kbps,
        };
        let plan = quality::plan(&video::available(), caps, acc.local_ip, acc.peer.ip(), width, height, &overrides)?;
        let enc = plan.encoder;
        self.budget.init_ceiling(plan.lan);

        // Only the first display to want audio gets it; others would just
        // duplicate the same desktop sound.
        let give_audio = want_audio && cfg.audio && {
            let mut owner = self.audio_owner.lock().unwrap();
            if owner.is_none() {
                *owner = Some(monitor);
                true
            } else {
                false
            }
        };

        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<StreamEvent>();
        let on_error = |tx: mpsc::UnboundedSender<StreamEvent>| move |e: String| drop(tx.send(StreamEvent::Failed(e)));
        let video = VideoPipeline::start(&capture.source, enc, plan.fps, plan.start_kbps, plan.max_kbps, on_error(ev_tx.clone()))?;
        let audio = if give_audio {
            AudioPipeline::start(on_error(ev_tx.clone()))
                .inspect_err(|e| warn!("no audio: {e:#}"))
                .ok()
        } else {
            None
        };

        let opts = OfferOptions { codec: enc.codec, audio: audio.is_some(), start_bitrate: plan.start_kbps as u64 * 1000 };
        let (pending, sdp) = rtc::create_offer(acc.local_ip, &opts)?;
        acc.signal.send(&HostMsg::Offer { sdp, codec: enc.codec, width, height, fps: plan.fps }).await?;
        let ClientMsg::Answer { sdp } = acc.signal.expect().await? else {
            bail!("expected Answer");
        };

        let handler = {
            let video = video.clone();
            let input = capture.input.clone();
            let tx = ev_tx.clone();
            Box::new(move |ev: SessionEvent| match ev {
                // These two are latency-critical, so they skip the queue.
                SessionEvent::KeyframeRequested => video.force_keyframe(),
                SessionEvent::Data(Channel::Input, bytes) => match decode::<InputEvent>(&bytes) {
                    Ok(e) => drop(input.send(e)),
                    Err(e) => debug!("input: {e}"),
                },
                other => drop(tx.send(StreamEvent::Session(other))),
            })
        };
        let session = pending.accept(&sdp, handler)?;
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
            "streaming display {monitor} {width}x{height}@{} {} via {} to {name} over {}",
            plan.fps,
            enc.codec.name(),
            enc.element,
            if plan.lan { "LAN" } else { "the internet" },
        );

        let stats_every = Duration::from_secs(2);
        let mut stats_timer = tokio::time::interval_at(tokio::time::Instant::now() + stats_every, stats_every);
        let mut last_stats = Instant::now();
        let result = loop {
            tokio::select! {
                msg = acc.signal.recv::<ClientMsg>() => match msg {
                    Ok(None) => break Ok(()),
                    Ok(Some(m)) => debug!("ignoring {m:?}"),
                    Err(e) => break Err(e),
                },
                ev = ev_rx.recv() => match ev {
                    Some(StreamEvent::Session(SessionEvent::Connected)) => video.force_keyframe(),
                    Some(StreamEvent::Session(SessionEvent::ChannelOpen(Channel::Control))) => {
                        session.send(Channel::Control, encode(&ControlMsg::VideoSize { width, height }));
                    }
                    Some(StreamEvent::Session(SessionEvent::Data(Channel::Control, bytes))) => {
                        match decode::<ControlMsg>(&bytes) {
                            Ok(ControlMsg::Ping { t }) => session.send(Channel::Control, encode(&ControlMsg::Pong { t })),
                            Ok(m) => debug!("control: {m:?}"),
                            Err(e) => debug!("control: {e}"),
                        }
                    }
                    Some(StreamEvent::Session(SessionEvent::BitrateEstimate(bps))) => video.on_bandwidth_estimate(bps),
                    Some(StreamEvent::Session(SessionEvent::Closed(err))) => {
                        break err.map_or(Ok(()), |e| Err(anyhow!(e)));
                    }
                    Some(StreamEvent::Failed(e)) => break Err(anyhow!(e)),
                    Some(StreamEvent::Session(_)) | None => {}
                },
                _ = stats_timer.tick() => {
                    let s = video.take_stats(last_stats.elapsed());
                    last_stats = Instant::now();
                    debug!("video {:.0} fps, {:.0} kbit/s (target {})", s.fps, s.kbps, video.bitrate_kbps());
                    let stats = ControlMsg::Stats {
                        bitrate_kbps: s.kbps as u32,
                        fps: s.fps.round() as u32,
                        encoder: enc.element.to_string(),
                    };
                    session.send(Channel::Control, encode(&stats));
                }
            }
        };

        session.close();
        video.stop();
        if let Some(a) = &audio {
            a.stop();
        }
        result
    }
}

/// Show the one-time PIN to whoever is at the host.
fn announce_pin(client: &str, ip: IpAddr, pin: &str) {
    info!("pairing request from {client} ({ip}): PIN {pin}");
    eprintln!("\n    Pairing request from {client} ({ip})\n    PIN: {pin}\n");
    let _ = std::process::Command::new("notify-send")
        .args(["--app-name=LinuxPlay", "--urgency=critical", "LinuxPlay pairing request"])
        .arg(format!("{client} ({ip}) wants to connect.\nPIN: {pin}"))
        .spawn();
}
