//! Decoding: encoded frames in, textures on the GPU out.
//!
//! The pipeline is `appsrc ! parser ! decoder ! glupload ! glcolorconvert !
//! appsink`. Nothing in it queues: a frame is decoded on the thread that
//! pushed it, and the sink keeps only the newest result, because showing an
//! old frame late is worse than skipping it.

mod ladder;

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use anyhow::{Context as _, Result, anyhow, bail};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gstreamer_gl as gst_gl;
use gstreamer_gl::prelude::*;
use gstreamer_video as gst_video;
use linuxplay_media::{bus, clock, codec, element};
use linuxplay_proto::signal::VideoCodec;
use tracing::{debug, info, warn};

pub use self::ladder::{Candidate, decodable, hw_decodable};
use crate::gl::GlBridge;
use crate::latency::LatencyTracker;

/// Encoded video waiting for the decoder, at most. Beyond it the oldest is
/// dropped: a host sending faster than we decode must not grow memory.
const MAX_QUEUED_BYTES: u64 = 8 << 20;

/// Called from GStreamer's threads.
#[derive(Clone)]
pub struct Callbacks {
    /// A new frame is ready in the [`FrameSource`].
    pub on_frame: Arc<dyn Fn() + Send + Sync>,
    /// The picture's size became known or changed.
    pub on_resize: Arc<dyn Fn() + Send + Sync>,
    /// The running decoder failed; [`VideoDecoder::fall_back`] may recover.
    pub on_error: Arc<dyn Fn(String) + Send + Sync>,
}

/// Where decoded frames wait for the renderer: only ever the newest one.
#[derive(Clone, Default)]
pub struct FrameSource {
    newest: Arc<Mutex<Option<gst::Sample>>>,
    /// Size of the newest decoded frame; zero until the first.
    decoded: Arc<PackedSize>,
    /// Size of the host's screen as the host announced it.
    announced: Arc<PackedSize>,
}

/// A width and height that can be read and written from any thread.
#[derive(Default)]
struct PackedSize(AtomicU64);

impl PackedSize {
    fn get(&self) -> (u32, u32) {
        let packed = self.0.load(Ordering::Relaxed);
        ((packed >> 32) as u32, packed as u32)
    }

    /// Store a size; returns whether it differs from the one before.
    fn set(&self, (width, height): (u32, u32)) -> bool {
        let packed = (width as u64) << 32 | height as u64;
        self.0.swap(packed, Ordering::Relaxed) != packed
    }
}

impl FrameSource {
    /// The newest decoded frame, if one arrived since the last call. Must run
    /// on the thread that owns the window's OpenGL context.
    pub fn take(&self, gl: &GlBridge) -> Option<Frame> {
        let sample = self.newest.lock().unwrap().take()?;
        Frame::from_sample(&sample, gl, self.announced.get())
    }

    /// The host said how large its screen is.
    pub fn set_announced_size(&self, size: (u32, u32)) {
        self.announced.set(size);
    }

    /// The size of the picture to show, once a frame has been decoded.
    pub fn size(&self) -> Option<(u32, u32)> {
        let decoded = self.decoded.get();
        (decoded != (0, 0)).then(|| codec::visible_size(decoded, self.announced.get()))
    }

    /// Store a decoded sample; returns whether the picture's size changed.
    fn put(&self, sample: gst::Sample) -> bool {
        let size = sample
            .caps()
            .and_then(|c| c.structure(0))
            .and_then(|s| Some((s.get::<i32>("width").ok()?, s.get::<i32>("height").ok()?)))
            .map(|(w, h)| (w.max(0) as u32, h.max(0) as u32));
        *self.newest.lock().unwrap() = Some(sample);
        size.is_some_and(|size| self.decoded.set(size))
    }
}

/// A decoded frame: an RGBA texture that lives as long as this value.
pub struct Frame {
    frame: gst_gl::GLVideoFrame<gst_gl::gl_video_frame::Readable>,
    rtp: u64,
    /// The part of the texture that is picture, from its top-left corner.
    visible: (u32, u32),
}

impl Frame {
    /// Map the newest sample for drawing. Must run on the thread that owns
    /// the window's OpenGL context.
    fn from_sample(sample: &gst::Sample, gl: &GlBridge, announced: (u32, u32)) -> Option<Self> {
        let info = gst_video::VideoInfo::from_caps(sample.caps()?).ok()?;
        let buffer = sample.buffer_owned()?;
        let rtp = clock::ns_to_rtp(buffer.pts()?.nseconds());
        let frame = gst_gl::GLVideoFrame::from_buffer_readable(buffer, &info).ok()?;
        // GStreamer drew this texture in its own context; make sure that
        // work has finished before ours samples it.
        if let Some(sync) = frame.buffer().meta::<gst_gl::GLSyncMeta>() {
            sync.wait(gl.context());
        }
        let visible = codec::visible_size((frame.width(), frame.height()), announced);
        Some(Self { frame, rtp, visible })
    }

    /// The OpenGL name of the frame's RGBA texture.
    pub fn texture(&self) -> u32 {
        self.frame.texture_id(0).unwrap_or(0)
    }

    /// The size of the picture in pixels.
    pub fn size(&self) -> (u32, u32) {
        self.visible
    }

    /// How much of the texture the picture covers, as fractions of its width
    /// and height: less than all of it when the encoder padded the picture.
    pub fn coverage(&self) -> (f32, f32) {
        let (width, height) = (self.frame.width().max(1), self.frame.height().max(1));
        (self.visible.0 as f32 / width as f32, self.visible.1 as f32 / height as f32)
    }

    /// The frame's RTP time, which identifies it to the latency tracker.
    pub fn rtp(&self) -> u64 {
        self.rtp
    }
}

/// Feeds encoded frames to whichever decoder is currently running. Cheap to
/// clone and usable from any thread.
#[derive(Clone)]
pub struct VideoFeed {
    src: Arc<Mutex<gst_app::AppSrc>>,
    codec: VideoCodec,
    tracker: Arc<LatencyTracker>,
}

impl VideoFeed {
    /// Hand over one complete encoded frame with its RTP time.
    pub fn push(&self, data: &[u8], rtp: u64) {
        self.tracker.received(rtp);
        let mut buffer = gst::Buffer::from_slice(codec::for_decoder(self.codec, data).into_owned());
        buffer
            .get_mut()
            .expect("a new buffer is not shared yet")
            .set_pts(gst::ClockTime::from_nseconds(clock::rtp_to_ns(rtp)));
        if let Err(e) = self.src.lock().unwrap().push_buffer(buffer) {
            debug!("video push: {e:?}");
        }
    }
}

/// One running decode pipeline.
struct Running {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    candidate: Candidate,
    stopped: Arc<AtomicBool>,
}

impl Drop for Running {
    fn drop(&mut self) {
        self.stopped.store(true, Ordering::Relaxed);
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

pub struct VideoDecoder {
    codec: VideoCodec,
    gl: GlBridge,
    callbacks: Callbacks,
    tracker: Arc<LatencyTracker>,
    /// Decoders not tried yet, best first.
    remaining: std::vec::IntoIter<Candidate>,
    running: Running,
    feed: VideoFeed,
    frames: FrameSource,
}

impl VideoDecoder {
    /// Start decoding `codec` with the best decoder that starts.
    pub fn start(codec: VideoCodec, gl: &GlBridge, tracker: Arc<LatencyTracker>, callbacks: Callbacks) -> Result<Self> {
        let frames = FrameSource::default();
        let mut remaining = ladder::candidates(codec).into_iter();
        let running = start_next(codec, gl, &mut remaining, &frames, &tracker, &callbacks)?;
        let feed = VideoFeed { src: Arc::new(Mutex::new(running.src.clone())), codec, tracker: tracker.clone() };
        Ok(Self { codec, gl: gl.clone(), callbacks, tracker, remaining, running, feed, frames })
    }

    /// The running decoder failed: replace it with the next one. The stream
    /// needs a keyframe afterwards. Fails when no decoder is left.
    pub fn fall_back(&mut self) -> Result<()> {
        let failed = self.running.candidate.element;
        let next = start_next(self.codec, &self.gl, &mut self.remaining, &self.frames, &self.tracker, &self.callbacks)
            .with_context(|| format!("{failed} failed and nothing else can decode {}", self.codec.name()))?;
        warn!("{failed} failed; decoding with {} instead", next.candidate.element);
        *self.feed.src.lock().unwrap() = next.src.clone();
        self.running = next;
        Ok(())
    }

    /// The decoder element in use, e.g. `vah264dec`.
    pub fn name(&self) -> &'static str {
        self.running.candidate.element
    }

    pub fn feed(&self) -> VideoFeed {
        self.feed.clone()
    }

    /// Where the renderer picks up decoded frames.
    pub fn frames(&self) -> FrameSource {
        self.frames.clone()
    }
}

/// Start the first of `remaining` that builds and reaches the playing state.
fn start_next(
    codec: VideoCodec,
    gl: &GlBridge,
    remaining: &mut std::vec::IntoIter<Candidate>,
    frames: &FrameSource,
    tracker: &Arc<LatencyTracker>,
    callbacks: &Callbacks,
) -> Result<Running> {
    let mut last_error = anyhow!("no {} decoder is installed", codec.name());
    for candidate in remaining {
        match start_one(codec, candidate, gl, frames, tracker, callbacks) {
            Ok(running) => return Ok(running),
            Err(e) => {
                warn!("{} can't be used: {e:#}", candidate.element);
                last_error = e;
            }
        }
    }
    Err(last_error)
}

fn start_one(
    codec: VideoCodec,
    candidate: Candidate,
    gl: &GlBridge,
    frames: &FrameSource,
    tracker: &Arc<LatencyTracker>,
    callbacks: &Callbacks,
) -> Result<Running> {
    let desc = format!(
        "appsrc name=src is-live=true format=time caps={caps} ! {parser} ! {decoder} name=dec{post} \
         ! glupload ! glcolorconvert ! appsink name=sink sync=false async=false max-buffers=2 drop=true \
         enable-last-sample=false",
        caps = codec::caps(codec),
        parser = codec::parser(codec),
        decoder = candidate.element,
        post = candidate.post,
    );
    info!("video pipeline: {desc}");
    let pipeline = gst::parse::launch(&desc)
        .context("building the decode pipeline")?
        .downcast::<gst::Pipeline>()
        .map_err(|_| anyhow!("not a pipeline"))?;
    gl.attach(&pipeline);

    tune(&pipeline.by_name("dec").context("no decoder")?);
    let src = pipeline.by_name("src").and_then(|s| s.downcast::<gst_app::AppSrc>().ok()).context("no appsrc")?;
    bound(&src, MAX_QUEUED_BYTES);
    let sink = pipeline.by_name("sink").and_then(|s| s.downcast::<gst_app::AppSink>().ok()).context("no appsink")?;
    sink.set_caps(Some(&texture_caps()));
    sink.set_callbacks(sink_callbacks(frames.clone(), tracker.clone(), callbacks.clone()));

    let stopped = Arc::new(AtomicBool::new(false));
    let running = Running { pipeline, src, candidate, stopped: stopped.clone() };
    if running.pipeline.set_state(gst::State::Playing).is_err() {
        bail!("the pipeline would not start");
    }
    let on_error = callbacks.on_error.clone();
    bus::watch(&running.pipeline, stopped, move |e| on_error(e));
    Ok(running)
}

/// What the sink accepts: an RGBA 2D texture.
fn texture_caps() -> gst::Caps {
    gst::Caps::builder("video/x-raw")
        .features([gst_gl::CAPS_FEATURE_MEMORY_GL_MEMORY])
        .field("format", "RGBA")
        .field("texture-target", "2D")
        .build()
}

/// Keep the newest decoded frame and tell the renderer about it.
fn sink_callbacks(frames: FrameSource, tracker: Arc<LatencyTracker>, callbacks: Callbacks) -> gst_app::AppSinkCallbacks {
    gst_app::AppSinkCallbacks::builder()
        .new_sample(move |sink| {
            let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
            if let Some(pts) = sample.buffer().and_then(|b| b.pts()) {
                tracker.decoded(clock::ns_to_rtp(pts.nseconds()));
            }
            if frames.put(sample) {
                (callbacks.on_resize)();
            }
            (callbacks.on_frame)();
            Ok(gst::FlowSuccess::Ok)
        })
        .build()
}

/// Output each frame as soon as it is decoded, and repaint the last good one
/// rather than show corruption after a loss.
fn tune(decoder: &gst::Element) {
    element::set(decoder, "compliance", "flexible");
    element::set(decoder, "discard-corrupted-frames", "true");
    // Frame threading would hold back one frame per thread.
    element::set(decoder, "thread-type", "slice");
}

/// Keep at most `max_bytes` waiting in an appsrc, dropping the oldest.
fn bound(src: &gst_app::AppSrc, max_bytes: u64) {
    src.set_max_bytes(max_bytes);
    element::set(src.upcast_ref(), "leaky-type", "downstream");
}
