//! One WebRTC peer connection, driven by str0m on its own thread.
//!
//! The host is always the offerer: it sends video (and optionally audio) and
//! opens the data channels. The client answers and only receives media.
//! Candidates are host candidates on the interface the signalling connection
//! already uses, so no candidate trickling is needed on a LAN.

use std::collections::HashMap;
use std::net::{IpAddr, UdpSocket};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use linuxplay_proto::signal::VideoCodec;
use linuxplay_proto::{CONTROL_CHANNEL, INPUT_CHANNEL};
use str0m::bwe::{Bitrate, BweKind};
use str0m::change::{SdpAnswer, SdpOffer, SdpPendingOffer};
use str0m::channel::{ChannelConfig, ChannelId, Reliability};
use str0m::media::{Direction, Frequency, KeyframeRequestKind, MediaKind, MediaTime, Mid, Pt};
use str0m::net::{Protocol, Receive};
use str0m::{Candidate, Event, IceConnectionState, Input, Output, Rtc, RtcConfig};
use tokio::sync::mpsc;
use tracing::{debug, info, warn};

/// How long a gap in the video may wait for a retransmission before the
/// frame is given up on. A LAN round trip is a millisecond or two; anything
/// later than this is better fixed with a keyframe than waited for.
const VIDEO_REORDER_TIMEOUT: Duration = Duration::from_millis(120);

/// Don't ask the host for keyframes more often than this. Short, so a frame
/// corrupted by a lost packet is repaired quickly instead of smearing.
const KEYFRAME_REQUEST_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Channel {
    Input,
    Control,
}

impl Channel {
    fn from_label(label: &str) -> Option<Self> {
        match label {
            INPUT_CHANNEL => Some(Self::Input),
            CONTROL_CHANNEL => Some(Self::Control),
            _ => None,
        }
    }
}

#[derive(Debug)]
pub enum SessionEvent {
    /// ICE and DTLS are up; media can flow.
    Connected,
    /// The session ended; `Some` carries the error that ended it.
    Closed(Option<String>),
    ChannelOpen(Channel),
    Data(Channel, Vec<u8>),
    /// A complete encoded video frame (Annex B for H.264/H.265).
    Video {
        data: Arc<[u8]>,
        rtp_time: u64,
        /// False when packets were lost before this frame; the decoder may
        /// need a keyframe, which has already been requested.
        contiguous: bool,
    },
    Audio {
        data: Arc<[u8]>,
        rtp_time: u64,
    },
    /// The peer lost video and wants a keyframe.
    KeyframeRequested,
    /// Bandwidth estimate for the outgoing media, bits per second.
    BitrateEstimate(u64),
}

enum Cmd {
    Video { data: Vec<u8>, pts: Duration },
    Audio { data: Vec<u8>, pts: Duration },
    Send(Channel, Vec<u8>),
    RequestKeyframe,
    DesiredBitrate(u64),
    Close,
}

/// Cheap handle to a running session; dropping every handle closes it.
#[derive(Clone)]
pub struct SessionHandle {
    tx: mpsc::UnboundedSender<Cmd>,
}

impl SessionHandle {
    /// Queue an encoded video frame. `pts` counts from the start of the stream.
    pub fn write_video(&self, data: Vec<u8>, pts: Duration) {
        let _ = self.tx.send(Cmd::Video { data, pts });
    }

    pub fn write_audio(&self, data: Vec<u8>, pts: Duration) {
        let _ = self.tx.send(Cmd::Audio { data, pts });
    }

    pub fn send(&self, channel: Channel, data: Vec<u8>) {
        let _ = self.tx.send(Cmd::Send(channel, data));
    }

    pub fn request_keyframe(&self) {
        let _ = self.tx.send(Cmd::RequestKeyframe);
    }

    /// Tell the bandwidth estimator how much we would like to send.
    pub fn set_desired_bitrate(&self, bps: u64) {
        let _ = self.tx.send(Cmd::DesiredBitrate(bps));
    }

    pub fn close(&self) {
        let _ = self.tx.send(Cmd::Close);
    }

    pub fn is_closed(&self) -> bool {
        self.tx.is_closed()
    }
}

pub type EventHandler = Box<dyn FnMut(SessionEvent) + Send>;

/// What the host wants to send.
#[derive(Debug, Clone)]
pub struct OfferOptions {
    pub codec: VideoCodec,
    pub audio: bool,
    /// Starting point for the bandwidth estimator, bits per second.
    pub start_bitrate: u64,
}

fn base_config() -> RtcConfig {
    RtcConfig::new()
        .clear_codecs()
        .set_reordering_timeout_video(Some(VIDEO_REORDER_TIMEOUT))
        .set_stats_interval(Some(Duration::from_secs(1)))
}

fn enable_codec(cfg: RtcConfig, codec: VideoCodec) -> RtcConfig {
    match codec {
        VideoCodec::H264 => cfg.enable_h264(true),
        VideoCodec::H265 => cfg.enable_h265(true),
        VideoCodec::Av1 => cfg.enable_av1(true),
    }
}

fn bind(local_ip: IpAddr, rtc: &mut Rtc) -> Result<UdpSocket> {
    let socket = UdpSocket::bind((local_ip, 0)).context("binding the media UDP socket")?;
    let addr = socket.local_addr()?;
    let candidate = Candidate::host(addr, "udp").map_err(|e| anyhow!("host candidate {addr}: {e}"))?;
    rtc.add_local_candidate(candidate);
    debug!("media socket on {addr}");
    Ok(socket)
}

/// A host-side session whose offer has been sent but not yet answered.
pub struct PendingSession {
    rtc: Rtc,
    socket: UdpSocket,
    pending: SdpPendingOffer,
    video: Mid,
    audio: Option<Mid>,
}

/// Host side: create the offer. Returns the session waiting for its answer and
/// the SDP to send to the client.
pub fn create_offer(local_ip: IpAddr, opts: &OfferOptions) -> Result<(PendingSession, String)> {
    let mut cfg = enable_codec(base_config(), opts.codec)
        .enable_bwe(Some(Bitrate::bps(opts.start_bitrate)));
    if opts.audio {
        cfg = cfg.enable_opus(true, false);
    }
    let mut rtc = cfg.build(Instant::now());
    let socket = bind(local_ip, &mut rtc)?;

    let mut api = rtc.sdp_api();
    let stream = Some("linuxplay".to_string());
    let video = api.add_media(MediaKind::Video, Direction::SendOnly, stream.clone(), Some("screen".into()), None);
    let audio = opts
        .audio
        .then(|| api.add_media(MediaKind::Audio, Direction::SendOnly, stream, Some("audio".into()), None));
    api.add_channel_with_config(ChannelConfig {
        label: INPUT_CHANNEL.into(),
        ordered: true,
        reliability: Reliability::Reliable,
        ..Default::default()
    });
    api.add_channel(CONTROL_CHANNEL.into());
    let (offer, pending) = api.apply().context("empty offer")?;

    let session = PendingSession { rtc, socket, pending, video, audio };
    Ok((session, offer.to_sdp_string()))
}

impl PendingSession {
    /// Apply the client's answer and start the session thread.
    pub fn accept(mut self, answer_sdp: &str, on_event: EventHandler) -> Result<SessionHandle> {
        let answer = SdpAnswer::from_sdp_string(answer_sdp).context("parsing the SDP answer")?;
        self.rtc
            .sdp_api()
            .accept_answer(self.pending, answer)
            .context("applying the SDP answer")?;
        let mut state = LoopState::new(on_event);
        state.video = Some(self.video);
        state.audio = self.audio;
        spawn(self.rtc, self.socket, state)
    }
}

/// Client side: answer the host's offer and start the session thread.
/// Returns the handle and the SDP answer to send back.
pub fn accept_offer(local_ip: IpAddr, offer_sdp: &str, on_event: EventHandler) -> Result<(SessionHandle, String)> {
    let mut cfg = base_config().enable_opus(true, false);
    for codec in [VideoCodec::H264, VideoCodec::H265, VideoCodec::Av1] {
        cfg = enable_codec(cfg, codec);
    }
    let mut rtc = cfg.build(Instant::now());
    let socket = bind(local_ip, &mut rtc)?;

    let offer = SdpOffer::from_sdp_string(offer_sdp).context("parsing the SDP offer")?;
    let answer = rtc.sdp_api().accept_offer(offer).context("accepting the SDP offer")?;
    let handle = spawn(rtc, socket, LoopState::new(on_event))?;
    Ok((handle, answer.to_sdp_string()))
}

fn spawn(rtc: Rtc, socket: UdpSocket, mut state: LoopState) -> Result<SessionHandle> {
    let (tx, rx) = mpsc::unbounded_channel();
    socket.set_nonblocking(true)?;
    std::thread::Builder::new()
        .name("linuxplay-rtc".into())
        .spawn(move || {
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("tokio runtime");
            let result = rt.block_on(run(rtc, socket, rx, &mut state));
            if let Err(e) = &result {
                warn!("session ended: {e:#}");
            }
            (state.on_event)(SessionEvent::Closed(result.err().map(|e| format!("{e:#}"))));
        })?;
    Ok(SessionHandle { tx })
}

async fn run(mut rtc: Rtc, socket: UdpSocket, mut rx: mpsc::UnboundedReceiver<Cmd>, st: &mut LoopState) -> Result<()> {
    let socket = tokio::net::UdpSocket::from_std(socket)?;
    let local = socket.local_addr()?;
    let mut buf = vec![0u8; 2048];

    loop {
        let timeout = loop {
            match rtc.poll_output()? {
                Output::Timeout(t) => break t,
                Output::Transmit(t) => {
                    if let Err(e) = socket.send_to(&t.contents, t.destination).await {
                        debug!("send to {}: {e}", t.destination);
                    }
                }
                Output::Event(e) => {
                    if !st.handle_event(&mut rtc, e) {
                        rtc.disconnect();
                        return Ok(());
                    }
                }
            }
        };

        let now = Instant::now();
        if timeout <= now {
            rtc.handle_input(Input::Timeout(now))?;
            continue;
        }

        tokio::select! {
            cmd = rx.recv() => match cmd {
                None | Some(Cmd::Close) => {
                    rtc.disconnect();
                    return Ok(());
                }
                Some(cmd) => st.apply(&mut rtc, cmd)?,
            },
            r = socket.recv_from(&mut buf) => {
                let (n, source) = r?;
                // Anything that isn't STUN/DTLS/RTP is ignored.
                if let Ok(contents) = buf[..n].try_into() {
                    let input = Input::Receive(Instant::now(), Receive { proto: Protocol::Udp, source, destination: local, contents });
                    rtc.handle_input(input)?;
                }
            }
            _ = tokio::time::sleep_until(timeout.into()) => {
                rtc.handle_input(Input::Timeout(Instant::now()))?;
            }
        }
    }
}

struct LoopState {
    on_event: EventHandler,
    video: Option<Mid>,
    audio: Option<Mid>,
    video_pt: Option<Pt>,
    audio_pt: Option<Pt>,
    channels: HashMap<ChannelId, Channel>,
    channel_ids: HashMap<Channel, ChannelId>,
    connected: bool,
    last_keyframe_request: Option<Instant>,
}

impl LoopState {
    fn new(on_event: EventHandler) -> Self {
        Self {
            on_event,
            video: None,
            audio: None,
            video_pt: None,
            audio_pt: None,
            channels: HashMap::new(),
            channel_ids: HashMap::new(),
            connected: false,
            last_keyframe_request: None,
        }
    }

    /// Returns false when the session should end.
    fn handle_event(&mut self, rtc: &mut Rtc, event: Event) -> bool {
        match event {
            Event::Connected => {
                info!("WebRTC connected");
                self.connected = true;
                (self.on_event)(SessionEvent::Connected);
            }
            Event::IceConnectionStateChange(state) => {
                debug!("ICE {state:?}");
                if state == IceConnectionState::Disconnected {
                    return false;
                }
            }
            Event::MediaAdded(m) => match m.kind {
                MediaKind::Video => self.video = Some(m.mid),
                MediaKind::Audio => self.audio = Some(m.mid),
            },
            Event::MediaData(d) => {
                let rtp_time = d.time.numer();
                if Some(d.mid) == self.video {
                    if !d.contiguous {
                        self.request_keyframe(rtc);
                    }
                    (self.on_event)(SessionEvent::Video { data: d.data, rtp_time, contiguous: d.contiguous });
                } else if Some(d.mid) == self.audio {
                    (self.on_event)(SessionEvent::Audio { data: d.data, rtp_time });
                }
            }
            Event::KeyframeRequest(_) => (self.on_event)(SessionEvent::KeyframeRequested),
            Event::ChannelOpen(id, label) => {
                if let Some(ch) = Channel::from_label(&label) {
                    self.channels.insert(id, ch);
                    self.channel_ids.insert(ch, id);
                    (self.on_event)(SessionEvent::ChannelOpen(ch));
                }
            }
            Event::ChannelData(d) => {
                if let Some(&ch) = self.channels.get(&d.id) {
                    (self.on_event)(SessionEvent::Data(ch, d.data));
                }
            }
            Event::ChannelClose(id) => {
                if let Some(ch) = self.channels.remove(&id) {
                    self.channel_ids.remove(&ch);
                }
            }
            Event::EgressBitrateEstimate(BweKind::Twcc { estimate: b, .. }) => {
                (self.on_event)(SessionEvent::BitrateEstimate(b.as_u64()));
            }
            _ => {}
        }
        true
    }

    fn apply(&mut self, rtc: &mut Rtc, cmd: Cmd) -> Result<()> {
        match cmd {
            Cmd::Video { data, pts } => {
                let (Some(mid), true) = (self.video, self.connected) else { return Ok(()) };
                write_media(rtc, mid, &mut self.video_pt, Frequency::NINETY_KHZ, data, pts)?;
            }
            Cmd::Audio { data, pts } => {
                let (Some(mid), true) = (self.audio, self.connected) else { return Ok(()) };
                write_media(rtc, mid, &mut self.audio_pt, Frequency::FORTY_EIGHT_KHZ, data, pts)?;
            }
            Cmd::Send(ch, data) => {
                if let Some(mut c) = self.channel_ids.get(&ch).and_then(|id| rtc.channel(*id)) {
                    if let Err(e) = c.write(true, &data) {
                        debug!("{ch:?} channel write: {e}");
                    }
                }
            }
            Cmd::RequestKeyframe => self.request_keyframe(rtc),
            Cmd::DesiredBitrate(bps) => rtc.bwe().set_desired_bitrate(Bitrate::bps(bps)),
            Cmd::Close => unreachable!("handled by the loop"),
        }
        Ok(())
    }

    fn request_keyframe(&mut self, rtc: &mut Rtc) {
        let now = Instant::now();
        if self
            .last_keyframe_request
            .is_some_and(|t| now.duration_since(t) < KEYFRAME_REQUEST_INTERVAL)
        {
            return;
        }
        let Some(mut w) = self.video.and_then(|mid| rtc.writer(mid)) else { return };
        if w.request_keyframe(None, KeyframeRequestKind::Pli).is_ok() {
            self.last_keyframe_request = Some(now);
        }
    }
}

fn write_media(
    rtc: &mut Rtc,
    mid: Mid,
    pt: &mut Option<Pt>,
    clock: Frequency,
    data: Vec<u8>,
    pts: Duration,
) -> Result<()> {
    let Some(writer) = rtc.writer(mid) else { return Ok(()) };
    let pt = match *pt {
        Some(pt) => pt,
        None => {
            let p = writer.payload_params().next().context("no negotiated codec")?.pt();
            *pt = Some(p);
            p
        }
    };
    let ticks = (pts.as_nanos() * clock.get() as u128 / 1_000_000_000) as u64;
    writer.write(pt, Instant::now(), MediaTime::new(ticks, clock), data)?;
    Ok(())
}
