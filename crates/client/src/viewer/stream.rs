//! One running stream: the WebRTC session, the decoder and speakers it
//! feeds, and the numbers shown about it.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::Result;
use linuxplay_media::clock;
use linuxplay_net::{Channel, SessionEvent, SessionHandle, rtc};
use linuxplay_proto::control::{ControlMsg, HostStats, Rumble};
use linuxplay_proto::input::InputEvent;
use linuxplay_proto::signal::VideoCodec;
use linuxplay_proto::{MAX_CONTROL_MESSAGE, decode, encode};
use tracing::{debug, warn};

use super::hud::Line;
use crate::audio::AudioOut;
use crate::connect::Offer;
use super::render::Video;
use crate::decode::{Callbacks, VideoDecoder};
use crate::gl::GlBridge;
use crate::latency::{LatencyReport, LatencyTracker};

/// Session events waiting for the window's thread. Bounded, so a host
/// flooding the control channel can't grow it.
const EVENT_QUEUE: usize = 64;

/// What arrived from the network, counted on the network thread.
#[derive(Default)]
struct Traffic {
    frames: AtomicU64,
    bytes: AtomicU64,
}

/// What changed during one [`Stream::poll`].
#[derive(Default)]
pub struct Changes {
    /// The input channel opened: input can be sent from now on.
    pub input_opened: bool,
    /// Rumble effects the host asked for.
    pub rumble: Vec<Rumble>,
    /// Text copied on the host, newest.
    pub clipboard: Option<String>,
    /// The stream is over; the inner value says why if it failed.
    pub ended: Option<Option<String>>,
}

pub struct Stream {
    session: SessionHandle,
    decoder: VideoDecoder,
    _audio: Option<AudioOut>,
    events: async_channel::Receiver<SessionEvent>,
    decoder_error: Arc<Mutex<Option<String>>>,
    tracker: Arc<LatencyTracker>,
    traffic: Arc<Traffic>,
    codec: VideoCodec,
    /// The host lets this session use keyboard, mouse and gamepads.
    control: bool,
    /// The picture's size as the host announced it, until frames say otherwise.
    announced_size: (u32, u32),
    connected: bool,
    input_ready: bool,
    host: Option<HostStats>,
    last_tick: Instant,
    stats: Vec<Line>,
}

impl Stream {
    /// Answer the host's offer and start receiving. From other threads,
    /// `wake` is called whenever [`poll`](Self::poll) has something new and
    /// `on_frame` whenever a frame is ready to draw. Returns the stream and
    /// the SDP answer for the host. Blocks for a moment while the media
    /// sockets are set up on `runtime`.
    pub fn open(
        offer: &Offer,
        runtime: &tokio::runtime::Handle,
        gl: &GlBridge,
        wake: Arc<dyn Fn() + Send + Sync>,
        on_frame: Arc<dyn Fn() + Send + Sync>,
    ) -> Result<(Self, String)> {
        let tracker = Arc::new(LatencyTracker::default());
        let traffic = Arc::new(Traffic::default());
        let decoder_error: Arc<Mutex<Option<String>>> = Arc::default();

        let callbacks = Callbacks {
            on_frame,
            on_resize: wake.clone(),
            on_error: {
                let (slot, wake) = (decoder_error.clone(), wake.clone());
                Arc::new(move |e| {
                    *slot.lock().unwrap() = Some(e);
                    wake();
                })
            },
        };
        let decoder = VideoDecoder::start(offer.codec, gl, tracker.clone(), callbacks)?;
        decoder.frames().set_announced_size((offer.width, offer.height));
        let audio = AudioOut::new().inspect_err(|e| warn!("no audio playback: {e:#}")).ok();

        let (tx, events) = async_channel::bounded::<SessionEvent>(EVENT_QUEUE);
        let handler = {
            let video = decoder.feed();
            let audio = audio.as_ref().map(|a| a.feed());
            let traffic = traffic.clone();
            let tracker = tracker.clone();
            // Media goes straight to the decoders from the network thread,
            // and so does anything whose arrival time matters; everything
            // else is handled on the window's thread.
            Box::new(move |event: SessionEvent| {
                match event {
                    SessionEvent::Data(Channel::Control, bytes) if note_timing(&tracker, &bytes) => return,
                    SessionEvent::Video { data, rtp_time, .. } => {
                        traffic.frames.fetch_add(1, Ordering::Relaxed);
                        traffic.bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
                        video.push(&data, rtp_time);
                        return;
                    }
                    SessionEvent::Audio { data, .. } => {
                        if let Some(audio) = &audio {
                            audio.push(&data);
                        }
                        return;
                    }
                    // The one event that must arrive displaces the oldest.
                    closed @ SessionEvent::Closed(_) => drop(tx.force_send(closed)),
                    SessionEvent::Data(_, bytes) if bytes.len() > MAX_CONTROL_MESSAGE => return,
                    other => drop(tx.try_send(other)),
                }
                wake();
            })
        };
        let (session, answer) = runtime.block_on(rtc::accept_offer(offer.route.reach(), &offer.sdp, handler))?;

        let stream = Self {
            session,
            decoder,
            _audio: audio,
            events,
            decoder_error,
            tracker,
            traffic,
            codec: offer.codec,
            control: offer.control,
            announced_size: (offer.width, offer.height),
            connected: false,
            input_ready: false,
            host: None,
            last_tick: Instant::now(),
            stats: Vec::new(),
        };
        Ok((stream, answer))
    }

    /// Take in everything that happened since the last call.
    pub fn poll(&mut self) -> Changes {
        let mut changes = Changes::default();
        while let Ok(event) = self.events.try_recv() {
            self.on_event(event, &mut changes);
        }
        if let Some(error) = self.decoder_error.lock().unwrap().take() {
            match self.decoder.fall_back() {
                // The new decoder starts from nothing: it needs a keyframe.
                Ok(()) => self.session.request_keyframe(),
                Err(e) => changes.ended = Some(Some(format!("{e:#} ({error})"))),
            }
        }
        changes
    }

    fn on_event(&mut self, event: SessionEvent, changes: &mut Changes) {
        match event {
            SessionEvent::Connected => self.connected = true,
            SessionEvent::ChannelOpen(Channel::Input) => {
                self.input_ready = true;
                changes.input_opened = true;
            }
            // Align the clocks as soon as there is a way to.
            SessionEvent::ChannelOpen(Channel::Control) => self.ping(),
            SessionEvent::Data(Channel::Control, bytes) => match decode::<ControlMsg>(&bytes) {
                Ok(ControlMsg::VideoSize { width, height }) => {
                    self.announced_size = (width, height);
                    self.decoder.frames().set_announced_size((width, height));
                }
                Ok(ControlMsg::Stats(stats)) => self.host = Some(stats),
                Ok(ControlMsg::Rumble(effect)) => changes.rumble.push(effect),
                Ok(ControlMsg::Clipboard { text }) if self.control => changes.clipboard = Some(text),
                Ok(other) => debug!("control: {other:?}"),
                Err(e) => debug!("control: {e}"),
            },
            SessionEvent::Closed(error) => changes.ended = Some(error),
            _ => {}
        }
    }

    /// Once a second: probe the round trip and refresh the statistics.
    pub fn tick(&mut self) {
        if self.connected {
            self.ping();
        }
        let elapsed = self.last_tick.elapsed().as_secs_f64().max(0.001);
        self.last_tick = Instant::now();
        let fps = self.traffic.frames.swap(0, Ordering::Relaxed) as f64 / elapsed;
        let mbps = self.traffic.bytes.swap(0, Ordering::Relaxed) as f64 * 8.0 / 1e6 / elapsed;
        let report = self.tracker.report();
        self.stats = self.describe(fps, mbps, &report);
        debug!(
            "{} | peaks: arrive {:.1} decode {:.1} display {:.1}",
            self.stats.iter().map(|l| l.text.as_str()).collect::<Vec<_>>().join(" | "),
            report.arrive.map_or(0.0, |a| a.max_ms),
            report.decode.max_ms,
            report.present.max_ms
        );
    }

    fn ping(&self) {
        self.session.send(Channel::Control, encode(&ControlMsg::Ping { t: clock::monotonic_us() }));
    }

    fn describe(&self, fps: f64, mbps: f64, latency: &LatencyReport) -> Vec<Line> {
        let (w, h) = self.video_size();
        let mut lines = vec![Line::normal(format!("{w}×{h} · {fps:.0} fps · {mbps:.1} Mbit/s"))];

        match latency.total {
            Some(total) => {
                lines.push(Line::normal(format!("Latency {:.1} ms · peak {:.1}", total.avg_ms, total.max_ms)));
            }
            None => lines.push(Line::normal("Latency: measuring…")),
        }
        if let (Some(arrive), Some(host)) = (latency.arrive, &self.host) {
            let encode = host.encode_us as f64 / 1000.0;
            lines.push(Line::dim(format!(
                "host {encode:.1} · network {:.1} · decode {:.1} · display {:.1}",
                (arrive.avg_ms - encode).max(0.0),
                latency.decode.avg_ms,
                latency.present.avg_ms
            )));
        }

        let encoder = self.host.as_ref().map_or("…", |h| h.encoder.as_str());
        lines.push(Line::dim(format!("{} · {encoder} to {}", self.codec.name(), self.decoder.name())));
        let rtt = latency.rtt_ms.map_or_else(|| "–".to_string(), |r| format!("{r:.1} ms"));
        lines.push(Line::dim(format!("Round trip {rtt} · skipped {}", latency.skipped)));
        lines
    }

    /// Send input to the host, once it can be received and if it is allowed.
    /// Share text copied here with the host, if this session may control it
    /// and the text fits in one message.
    pub fn send_clipboard(&self, text: &str) {
        let message = encode(&ControlMsg::Clipboard { text: text.to_string() });
        if self.control && message.len() <= MAX_CONTROL_MESSAGE {
            self.session.send(Channel::Control, message);
        }
    }

    pub fn send_input(&self, event: InputEvent) {
        // The host ignores input from a view-only session anyway.
        if self.control && self.input_ready {
            self.session.send(Channel::Input, encode(&event));
        }
    }

    /// What the render thread needs to show this stream.
    pub fn video(&self) -> Video {
        Video { frames: self.decoder.frames(), tracker: self.tracker.clone() }
    }

    /// Has the first frame been decoded?
    pub fn has_picture(&self) -> bool {
        self.decoder.frames().size().is_some()
    }

    /// The picture's size in pixels.
    pub fn video_size(&self) -> (u32, u32) {
        self.decoder.frames().size().unwrap_or(self.announced_size)
    }

    pub fn can_control(&self) -> bool {
        self.control
    }

    /// The statistics panel's lines, as of the last tick.
    pub fn stats(&self) -> &[Line] {
        &self.stats
    }
}

/// Handle the control messages that carry timing, right where they arrive:
/// a `Pong` is only worth as much as the precision of its arrival time.
/// Returns whether `bytes` was such a message.
fn note_timing(tracker: &LatencyTracker, bytes: &[u8]) -> bool {
    if bytes.len() > MAX_CONTROL_MESSAGE {
        return false;
    }
    match decode::<ControlMsg>(bytes) {
        Ok(ControlMsg::Pong { t, host_us }) => tracker.clock_sample(t, host_us),
        Ok(ControlMsg::VideoClock { base_us }) => tracker.set_video_base(base_us),
        _ => return false,
    }
    true
}

impl Drop for Stream {
    fn drop(&mut self) {
        self.session.close();
    }
}
