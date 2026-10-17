//! Screen encoding: picks the best encoder for the agreed codec and runs
//! capture → convert → encode → appsink, handing each frame to the session.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, Ordering};
use std::sync::{Arc, OnceLock};
use std::time::{Duration, Instant};

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_video as gst_video;
use linuxplay_net::SessionHandle;
use linuxplay_proto::signal::VideoCodec;
use tracing::{debug, info, warn};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Backend {
    Va,
    Nvenc,
    Software,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Encoder {
    pub element: &'static str,
    pub codec: VideoCodec,
    pub backend: Backend,
}

/// Every encoder we know how to drive, best first within each codec.
const ENCODERS: &[Encoder] = {
    use Backend::*;
    use VideoCodec::*;
    const fn e(element: &'static str, codec: VideoCodec, backend: Backend) -> Encoder {
        Encoder { element, codec, backend }
    }
    &[
        e("nvh264enc", H264, Nvenc),
        e("vah264enc", H264, Va),
        e("vah264lpenc", H264, Va),
        e("x264enc", H264, Software),
        e("openh264enc", H264, Software),
        e("nvh265enc", H265, Nvenc),
        e("vah265enc", H265, Va),
        e("vah265lpenc", H265, Va),
        e("x265enc", H265, Software),
        e("nvav1enc", Av1, Nvenc),
        e("vaav1enc", Av1, Va),
        e("vaav1lpenc", Av1, Va),
        e("svtav1enc", Av1, Software),
    ]
};

fn has(factory: &str) -> bool {
    gst::ElementFactory::find(factory).is_some()
}

/// Encoders whose GStreamer element is installed and usable on this machine.
pub fn available() -> Vec<Encoder> {
    ENCODERS.iter().copied().filter(|e| has(e.element)).collect()
}

/// The conversion in front of the encoder, chosen so the colour conversion
/// happens on the same device that encodes.
fn converter(enc: &Encoder) -> &'static str {
    match enc.backend {
        Backend::Va => "vapostproc ! video/x-raw(memory:VAMemory),format=NV12",
        Backend::Nvenc if has("cudaupload") && has("cudaconvert") => {
            "cudaupload ! cudaconvert ! video/x-raw(memory:CUDAMemory),format=NV12"
        }
        Backend::Nvenc => "videoconvert n-threads=4 ! video/x-raw,format=NV12",
        Backend::Software => "videoconvert n-threads=4 ! video/x-raw,format=I420",
    }
}

fn parser(codec: VideoCodec) -> &'static str {
    match codec {
        VideoCodec::H264 => "h264parse config-interval=-1 ! video/x-h264,stream-format=byte-stream,alignment=au",
        VideoCodec::H265 => "h265parse config-interval=-1 ! video/x-h265,stream-format=byte-stream,alignment=au",
        VideoCodec::Av1 => "av1parse ! video/x-av1,stream-format=obu-stream,alignment=tu",
    }
}

/// Set a property from a string if the element has it; encoders differ
/// between GStreamer versions, so a missing knob is not an error.
fn set(el: &gst::Element, name: &str, value: &str) {
    let Some(pspec) = el.find_property(name) else {
        debug!("{} has no property {name}", el.name());
        return;
    };
    if !pspec.flags().contains(gst::glib::ParamFlags::WRITABLE) {
        warn!("{}: {name} is read-only", el.name());
        return;
    }
    // Parsing against the pspec yields the property's exact type, so the
    // set below can't hit a type mismatch.
    match gst::glib::Value::deserialize_with_pspec(value, &pspec) {
        Ok(v) => el.set_property_from_value(name, &v),
        Err(e) => warn!("{}: {name}={value}: {e}", el.name()),
    }
}

/// Low-latency settings: no B-frames, no lookahead, and a long GOP because
/// lost frames are repaired with on-demand keyframes. Variable bitrate, with
/// the bitrate as the ceiling, so a still desktop costs next to nothing.
fn configure(el: &gst::Element, enc: &Encoder, fps: u32) {
    let gop = (fps * 10).to_string();
    match enc.backend {
        Backend::Va => {
            set(el, "rate-control", "vbr");
            set(el, "target-percentage", "95");
            // 1 (best quality) .. 7 (fastest). The fastest preset blocks up
            // badly on video; 4 is a good quality/latency balance for a GPU.
            set(el, "target-usage", "4");
            // Spread bits to the busy blocks so motion doesn't smear.
            set(el, "mbbrc", "1");
            set(el, "b-frames", "0");
            set(el, "ref-frames", "2");
            set(el, "key-int-max", &gop);
        }
        Backend::Nvenc => {
            set(el, "preset", "p1");
            set(el, "tune", "ultra-low-latency");
            set(el, "rate-control", "vbr");
            set(el, "gop-size", &gop);
            set(el, "bframes", "0");
            set(el, "b-frames", "0");
            set(el, "rc-lookahead", "0");
            set(el, "zero-reorder-delay", "true");
        }
        Backend::Software => match enc.element {
            "x264enc" | "x265enc" => {
                set(el, "tune", "zerolatency");
                set(el, "speed-preset", "ultrafast");
                set(el, "key-int-max", &gop);
                set(el, "bframes", "0");
                set(el, "sliced-threads", "true");
                set(el, "vbv-buf-capacity", "120");
            }
            "openh264enc" => {
                set(el, "usage-type", "screen");
                set(el, "complexity", "low");
                set(el, "rate-control", "bitrate");
                set(el, "gop-size", &gop);
            }
            "svtav1enc" => {
                set(el, "preset", "12");
                set(el, "intra-period-length", &gop);
            }
            _ => {}
        },
    }
}

fn set_bitrate(el: &gst::Element, enc: &Encoder, kbps: u32) {
    let (name, value) = match enc.element {
        "openh264enc" => ("bitrate", kbps.saturating_mul(1000)),
        "svtav1enc" => ("target-bitrate", kbps),
        _ => ("bitrate", kbps),
    };
    set(el, name, &value.to_string());
}

#[derive(Default)]
struct Counters {
    frames: AtomicU64,
    bytes: AtomicU64,
}

pub struct VideoPipeline {
    pipeline: gst::Pipeline,
    encoder: gst::Element,
    rate: gst::Element,
    sink: gst_app::AppSink,
    pub enc: Encoder,
    session: Arc<OnceLock<SessionHandle>>,
    counters: Arc<Counters>,
    bitrate_kbps: AtomicU32,
    max_kbps: AtomicU32,
    fps_cap: AtomicU32,
    fps: AtomicU32,
    stopped: Arc<AtomicBool>,
}

/// Encoder output shared with the stats logger.
pub struct Stats {
    pub fps: f64,
    pub kbps: f64,
}

/// Lowest bitrate the adaptation will go to, kbit/s.
const MIN_KBPS: u32 = 1_000;
/// After the session drops a frame, ask for a keyframe at most this often.
const KEYFRAME_RECOVERY_INTERVAL: Duration = Duration::from_millis(100);

impl VideoPipeline {
    /// Build and start the pipeline. `source` is a gst-launch fragment that
    /// produces raw video. `on_error` is called once if the pipeline fails.
    pub fn start(
        source: &str,
        enc: Encoder,
        fps: u32,
        start_kbps: u32,
        max_kbps: u32,
        on_error: impl Fn(String) + Send + 'static,
    ) -> Result<Arc<Self>> {
        // videorate (drop-only) lets us lower the framerate live under
        // bandwidth pressure without rebuilding the pipeline.
        let desc = format!(
            "{source} ! videorate name=rate drop-only=true max-rate={fps} \
             ! queue max-size-buffers=1 max-size-bytes=0 max-size-time=0 leaky=downstream \
             ! {conv} ! {el} name=enc ! {parse} \
             ! appsink name=sink sync=false async=false max-buffers=8 enable-last-sample=false",
            conv = converter(&enc),
            el = enc.element,
            parse = parser(enc.codec),
        );
        info!("video pipeline: {desc}");
        let pipeline = gst::parse::launch(&desc)
            .context("building the video pipeline")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("not a pipeline"))?;
        let encoder = pipeline.by_name("enc").context("no encoder")?;
        let rate = pipeline.by_name("rate").context("no videorate")?;
        let sink = pipeline
            .by_name("sink")
            .and_then(|s| s.downcast::<gst_app::AppSink>().ok())
            .context("no appsink")?;

        configure(&encoder, &enc, fps);
        let start_kbps = start_kbps.clamp(MIN_KBPS, max_kbps);
        set_bitrate(&encoder, &enc, start_kbps);

        let session: Arc<OnceLock<SessionHandle>> = Arc::default();
        let counters: Arc<Counters> = Arc::default();
        {
            let session = session.clone();
            let counters = counters.clone();
            let mut last_recovery: Option<Instant> = None;
            sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                        let pts = buffer.pts().unwrap_or(gst::ClockTime::ZERO);
                        let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                        counters.frames.fetch_add(1, Ordering::Relaxed);
                        counters.bytes.fetch_add(map.len() as u64, Ordering::Relaxed);
                        if let Some(h) = session.get() {
                            let sent = h.write_video(map.as_slice().to_vec(), Duration::from_nanos(pts.nseconds()));
                            // The session is behind and dropped this frame; the
                            // frames after it can't be decoded until a keyframe.
                            if !sent && last_recovery.is_none_or(|t| t.elapsed() >= KEYFRAME_RECOVERY_INTERVAL) {
                                last_recovery = Some(Instant::now());
                                let event = gst_video::UpstreamForceKeyUnitEvent::builder().all_headers(true).build();
                                sink.send_event(event);
                            }
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        }

        let stopped = Arc::new(AtomicBool::new(false));
        watch_bus(&pipeline, stopped.clone(), on_error);
        pipeline.set_state(gst::State::Playing).context("starting the video pipeline")?;

        Ok(Arc::new(Self {
            pipeline,
            encoder,
            rate,
            sink,
            enc,
            session,
            counters,
            bitrate_kbps: AtomicU32::new(start_kbps),
            max_kbps: AtomicU32::new(max_kbps),
            fps_cap: AtomicU32::new(fps),
            fps: AtomicU32::new(fps),
            stopped,
        }))
    }

    /// Start sending frames to this session.
    pub fn attach(&self, session: SessionHandle) {
        let _ = self.session.set(session);
    }

    pub fn force_keyframe(&self) {
        let event = gst_video::UpstreamForceKeyUnitEvent::builder().all_headers(true).build();
        if !self.sink.send_event(event) {
            debug!("keyframe request not handled");
        }
    }

    pub fn bitrate_kbps(&self) -> u32 {
        self.bitrate_kbps.load(Ordering::Relaxed)
    }

    pub fn fps(&self) -> u32 {
        self.fps.load(Ordering::Relaxed)
    }

    pub fn max_kbps(&self) -> u32 {
        self.max_kbps.load(Ordering::Relaxed)
    }

    /// Lower (or raise) this stream's bitrate ceiling while running, used when
    /// the bandwidth budget is shared with another display.
    pub fn set_max_kbps(&self, max_kbps: u32) {
        let max_kbps = max_kbps.max(MIN_KBPS);
        self.max_kbps.store(max_kbps, Ordering::Relaxed);
        if self.bitrate_kbps() > max_kbps {
            set_bitrate(&self.encoder, &self.enc, max_kbps);
            self.bitrate_kbps.store(max_kbps, Ordering::Relaxed);
        }
    }

    /// Lower (or raise) the framerate cap while running, used to keep several
    /// displays within the encoder's capacity.
    pub fn set_fps_cap(&self, cap: u32) {
        let cap = cap.max(15);
        self.fps_cap.store(cap, Ordering::Relaxed);
        if self.fps() > cap {
            self.rate.set_property("max-rate", cap as i32);
            self.fps.store(cap, Ordering::Relaxed);
        }
    }

    /// Follow the bandwidth estimate, leaving headroom for audio, data and
    /// retransmissions. Small changes are ignored so the encoder isn't
    /// reconfigured constantly.
    pub fn on_bandwidth_estimate(&self, bps: u64) {
        let target = ((bps / 1000) as f64 * 0.85) as u32;
        let target = target.clamp(MIN_KBPS, self.max_kbps());
        let current = self.bitrate_kbps();
        let change = (target as f64 - current as f64).abs() / current as f64;
        if change >= 0.1 {
            debug!("video bitrate {current} → {target} kbit/s");
            set_bitrate(&self.encoder, &self.enc, target);
            self.bitrate_kbps.store(target, Ordering::Relaxed);
        }
        self.adapt_fps(target);
    }

    /// Drop the framerate when the bitrate sinks so low that spending it on
    /// 60 fps would make each frame mushy; raise it again when there's room.
    /// Steps are sticky (hysteresis) so the rate doesn't oscillate.
    fn adapt_fps(&self, target_kbps: u32) {
        let cap = self.fps_cap.load(Ordering::Relaxed);
        let full = self.max_kbps().max(MIN_KBPS) as f64;
        let ratio = target_kbps as f64 / full;
        let current = self.fps();
        // Raise thresholds above the lowering ones so a value that just
        // triggered a drop doesn't immediately trigger a rise.
        let desired = if ratio < 0.25 {
            (cap / 2).max(30)
        } else if ratio < 0.45 {
            (cap * 3 / 4).max(30)
        } else if ratio > 0.6 {
            cap
        } else {
            current
        };
        let desired = desired.min(cap);
        if desired != current {
            debug!("video {current} → {desired} fps (bitrate at {:.0}% of ceiling)", ratio * 100.0);
            self.rate.set_property("max-rate", desired as i32);
            self.fps.store(desired, Ordering::Relaxed);
        }
    }

    /// Frames and bitrate since the last call.
    pub fn take_stats(&self, elapsed: Duration) -> Stats {
        let frames = self.counters.frames.swap(0, Ordering::Relaxed);
        let bytes = self.counters.bytes.swap(0, Ordering::Relaxed);
        let secs = elapsed.as_secs_f64().max(0.001);
        Stats { fps: frames as f64 / secs, kbps: bytes as f64 * 8.0 / 1000.0 / secs }
    }

    pub fn stop(&self) {
        self.stopped.store(true, Ordering::Relaxed);
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for VideoPipeline {
    fn drop(&mut self) {
        self.stop();
    }
}

/// Report the first error on the pipeline's bus. Polls so the thread ends
/// soon after `stopped` is set.
pub fn watch_bus(pipeline: &gst::Pipeline, stopped: Arc<AtomicBool>, on_error: impl Fn(String) + Send + 'static) {
    let bus = pipeline.bus().expect("pipeline bus");
    let name = pipeline.name();
    std::thread::spawn(move || {
        let started = Instant::now();
        while !stopped.load(Ordering::Relaxed) {
            let Some(msg) = bus.timed_pop_filtered(
                gst::ClockTime::from_mseconds(200),
                &[gst::MessageType::Error, gst::MessageType::Eos, gst::MessageType::Warning],
            ) else {
                continue;
            };
            match msg.view() {
                gst::MessageView::Error(e) => {
                    let text = format!(
                        "{}: {} ({})",
                        e.src().map(|s| s.path_string().to_string()).unwrap_or_default(),
                        e.error(),
                        e.debug().unwrap_or_default()
                    );
                    warn!("{name} failed after {:?}: {text}", started.elapsed());
                    on_error(text);
                    return;
                }
                gst::MessageView::Eos(_) => {
                    on_error("capture ended".into());
                    return;
                }
                gst::MessageView::Warning(w) => debug!("{name}: {}", w.error()),
                _ => {}
            }
        }
    });
}
