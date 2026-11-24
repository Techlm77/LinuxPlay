//! One display being streamed to one client, from capture to the last frame.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use anyhow::{Result, anyhow, bail};
use linuxplay_media::clock;
use linuxplay_net::rtc::{self, OfferOptions};
use linuxplay_net::ws::{Accepted, Route};
use linuxplay_net::{Channel, SessionEvent, SessionHandle};
use linuxplay_proto::control::{ControlMsg, HostStats};
use linuxplay_proto::signal::{ClientMsg, HostMsg};
use linuxplay_proto::{MAX_CONTROL_MESSAGE, decode, encode};
use tokio::sync::{mpsc, watch};
use tracing::{debug, info, warn};

use super::Host;
use super::budget::StreamSlot;
use super::handshake::HelloInfo;
use crate::audio::AudioPipeline;
use crate::clipboard::Text;
use crate::input::{InputGate, Verdict};
use crate::quality::{self, Overrides};
use crate::sessions::Grant;
use crate::video::{self, VideoPipeline};

/// Session events waiting for the stream loop; beyond it they are dropped.
const STREAM_EVENT_QUEUE: usize = 64;
/// Control messages (pings) a client may send per second, and the burst.
const CONTROL_RATE: f64 = 20.0;
const CONTROL_BURST: f64 = 40.0;

/// How a stream ends. At most one per pipeline plus one for the session, so
/// the channel carrying these is bounded by construction.
enum StreamEnd {
    Closed(Option<String>),
    Failed(String),
}

/// Frees a display's reservation (and its share of the budget) on drop.
pub(super) struct StreamGuard<'a> {
    pub(super) host: &'a Host,
    pub(super) monitor: u32,
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
    pub(super) async fn stream(
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
        if width == 0 || height == 0 {
            bail!("display {monitor} has no size ({width}x{height}), so it can't be streamed");
        }

        // Weigh encoders, codecs, link type and resolution into one plan.
        let overrides = Overrides {
            encoder: cfg.encoder.clone(),
            fps: cfg.fps,
            start_kbps: cfg.start_kbps,
            max_kbps: cfg.max_kbps,
        };
        let lan = match acc.route {
            Route::Direct { local_ip, peer } => quality::is_lan(local_ip, peer.ip()),
            // Relayed means the peer isn't simply next to us on the network.
            Route::Relayed { .. } => false,
        };
        let plan = quality::plan(&video::available(), &hello.caps, lan, width, height, &overrides)?;
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
        let (pending, sdp) = rtc::create_offer(acc.route.reach(), &opts).await?;
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
        // Rumble from the virtual gamepads goes back to the client's own.
        let rumble_to = session.clone();
        capture.feedback.connect(move |effect| rumble_to.send(Channel::Control, encode(&ControlMsg::Rumble(effect))));
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
        // Only a session that may control this desktop shares its clipboard,
        // in either direction.
        let mut copies = grant.control.then(|| self.clipboard.copies());
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
                        // Lets the client work out how old each frame is.
                        if let Some(base_us) = video.clock_base_us() {
                            session.send(Channel::Control, encode(&ControlMsg::VideoClock { base_us }));
                        }
                    }
                    SessionEvent::Data(Channel::Control, bytes) => {
                        if bytes.len() > MAX_CONTROL_MESSAGE || !pings.take() {
                            continue;
                        }
                        match decode::<ControlMsg>(&bytes) {
                            Ok(ControlMsg::Ping { t }) => {
                                let pong = ControlMsg::Pong { t, host_us: clock::monotonic_us() };
                                session.send(Channel::Control, encode(&pong));
                            }
                            Ok(ControlMsg::Clipboard { text }) if grant.control => {
                                debug!("clipboard: {} characters from {}", text.chars().count(), hello.name);
                                self.clipboard.paste(&text);
                            }
                            Ok(m) => debug!("control: {m:?}"),
                            Err(e) => debug!("control: {e}"),
                        }
                    }
                    SessionEvent::BitrateEstimate(bps) => video.on_bandwidth_estimate(bps),
                    _ => {}
                },
                Some(text) = next_copy(&mut copies) => {
                    debug!("clipboard: {} characters copied here, sent to {}", text.chars().count(), hello.name);
                    session.send(Channel::Control, encode(&ControlMsg::Clipboard { text: text.to_string() }));
                }
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
                    let stats = ControlMsg::Stats(HostStats {
                        bitrate_kbps: s.kbps as u32,
                        fps: s.fps.round() as u32,
                        encoder: enc.element.to_string(),
                        encode_us: s.encode_us,
                    });
                    session.send(Channel::Control, encode(&stats));
                    // Again with every report: when the control channel opened
                    // a slow encoder may not have been running yet.
                    if let Some(base_us) = video.clock_base_us() {
                        session.send(Channel::Control, encode(&ControlMsg::VideoClock { base_us }));
                    }
                }
            }
        }
    }
}

/// The next text copied on this desktop; never, without a subscription.
async fn next_copy(copies: &mut Option<watch::Receiver<Text>>) -> Option<Arc<str>> {
    let Some(receiver) = copies else { return std::future::pending().await };
    if receiver.changed().await.is_err() {
        *copies = None;
        return std::future::pending().await;
    }
    receiver.borrow_and_update().clone()
}
