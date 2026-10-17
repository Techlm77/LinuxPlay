//! Accepting clients: authentication, pairing, then one stream at a time.

use std::net::IpAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::Semaphore;

use anyhow::{Context, Result, anyhow, bail};
use linuxplay_net::rtc::{self, OfferOptions};
use linuxplay_net::tls::Identity;
use linuxplay_net::ws::{self, Accepted};
use linuxplay_net::{Channel, SessionEvent, pair};
use linuxplay_proto::control::ControlMsg;
use linuxplay_proto::input::InputEvent;
use linuxplay_proto::signal::{ClientMsg, HostMsg};
use linuxplay_proto::{PROTOCOL_VERSION, decode, encode};
use tokio::net::TcpListener;
use tokio::sync::mpsc;
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, warn};

use crate::audio::AudioPipeline;
use crate::capture::{self, CaptureKind};
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

#[derive(Default)]
struct PairLimiter {
    failures: u32,
    locked_until: Option<Instant>,
}

/// Cap on connections that have not finished authenticating, so a flood of
/// half-open TLS/pairing attempts can't pile up tasks on the host.
const MAX_PENDING: usize = 16;

pub struct Host {
    cfg: HostConfig,
    identity: Identity,
    trust: Mutex<TrustStore>,
    limiter: Mutex<PairLimiter>,
    pending: Arc<Semaphore>,
    busy: AtomicBool,
}

struct BusyGuard<'a>(&'a AtomicBool);

impl Drop for BusyGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::SeqCst);
    }
}

enum StreamEvent {
    Session(SessionEvent),
    Failed(String),
}

impl Host {
    pub fn new(cfg: HostConfig, identity: Identity, trust: TrustStore) -> Arc<Self> {
        Arc::new(Self {
            cfg,
            identity,
            trust: Mutex::new(trust),
            limiter: Mutex::default(),
            pending: Arc::new(Semaphore::new(MAX_PENDING)),
            busy: AtomicBool::new(false),
        })
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

    async fn handle(&self, mut acc: Accepted) -> Result<()> {
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

        if self.busy.swap(true, Ordering::SeqCst) {
            let message = "this host is already streaming to another client".to_string();
            acc.signal.send(&HostMsg::Error { message: message.clone() }).await?;
            bail!(message);
        }
        let _busy = BusyGuard(&self.busy);
        info!("{name} ({}) connected", acc.peer);
        let caps = ClientCaps {
            codecs,
            hw_codecs,
            max_fps: if max_fps == 0 { 60 } else { max_fps },
        };
        let result = self.stream(&mut acc, &name, &caps, audio).await;
        if let Err(e) = &result {
            let _ = acc.signal.send(&HostMsg::Error { message: format!("{e:#}") }).await;
        }
        acc.signal.close().await;
        info!("{name} disconnected");
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

    async fn stream(&self, acc: &mut Accepted, name: &str, caps: &ClientCaps, want_audio: bool) -> Result<()> {
        let cfg = &self.cfg;
        let kind = match cfg.capture {
            Some(k) => k,
            None => CaptureKind::detect()?,
        };
        // Capture at the host's framerate cap; the planner's chosen rate is
        // applied downstream, so we never encode faster than the client wants.
        let capture = capture::open(kind, cfg.fps.max(1)).await?;
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

        let (ev_tx, mut ev_rx) = mpsc::unbounded_channel::<StreamEvent>();
        let on_error = |tx: mpsc::UnboundedSender<StreamEvent>| move |e: String| drop(tx.send(StreamEvent::Failed(e)));
        let video = VideoPipeline::start(&capture.source, enc, plan.fps, plan.start_kbps, plan.max_kbps, on_error(ev_tx.clone()))?;
        let audio = if want_audio && cfg.audio {
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
        session.set_desired_bitrate(plan.max_kbps as u64 * 1000);
        info!(
            "streaming {width}x{height}@{} {} via {} to {name} over {}",
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
