//! Decoding and playing what the host sends.

use anyhow::{Context, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use gtk::gdk;
use linuxplay_proto::signal::VideoCodec;
use tracing::{debug, info, warn};

/// Decoders per codec, best first.
fn decoders(codec: VideoCodec) -> &'static [&'static str] {
    match codec {
        VideoCodec::H264 => &["vah264dec", "nvh264dec", "avdec_h264", "openh264dec"],
        VideoCodec::H265 => &["vah265dec", "nvh265dec", "avdec_h265"],
        VideoCodec::Av1 => &["vaav1dec", "nvav1dec", "dav1ddec", "av1dec"],
    }
}

fn find_decoder(codec: VideoCodec) -> Option<&'static str> {
    decoders(codec).iter().copied().find(|d| gst::ElementFactory::find(d).is_some())
}

fn is_hardware(decoder: &str) -> bool {
    decoder.starts_with("va") || decoder.starts_with("nv")
}

/// Codecs this machine can decode, in the order we'd like the host to pick.
pub fn decodable() -> Vec<VideoCodec> {
    [VideoCodec::H264, VideoCodec::H265, VideoCodec::Av1]
        .into_iter()
        .filter(|c| find_decoder(*c).is_some())
        .collect()
}

/// The subset of [`decodable`] we can decode in hardware; the host weighs
/// these heavily, since software-decoding AV1/HEVC at 1080p60 is punishing.
pub fn hw_decodable() -> Vec<VideoCodec> {
    [VideoCodec::H264, VideoCodec::H265, VideoCodec::Av1]
        .into_iter()
        .filter(|c| find_decoder(*c).is_some_and(is_hardware))
        .collect()
}

fn set(el: &gst::Element, name: &str, value: &str) {
    if el.find_property(name).is_some() {
        el.set_property_from_str(name, value);
    }
}

/// Encoded video in, frames on a `gdk::Paintable` out.
pub struct VideoOut {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
    pub paintable: gdk::Paintable,
    pub decoder: &'static str,
}

impl VideoOut {
    /// Must run on the GTK main thread (the sink hands out its paintable there).
    pub fn new(codec: VideoCodec) -> Result<Self> {
        let decoder = find_decoder(codec).ok_or_else(|| anyhow!("no {} decoder installed", codec.name()))?;
        let (caps, parse) = match codec {
            VideoCodec::H264 => ("video/x-h264,stream-format=byte-stream,alignment=au", "h264parse"),
            VideoCodec::H265 => ("video/x-h265,stream-format=byte-stream,alignment=au", "h265parse"),
            VideoCodec::Av1 => ("video/x-av1,stream-format=obu-stream,alignment=tu", "av1parse"),
        };
        let convert = if is_hardware(decoder) { "" } else { "videoconvert ! " };
        let desc = format!(
            "appsrc name=src is-live=true format=time do-timestamp=true min-latency=0 caps={caps} \
             ! {parse} ! {decoder} name=dec ! {convert}gtk4paintablesink name=sink sync=false"
        );
        info!("video pipeline: {desc}");
        let pipeline = gst::parse::launch(&desc)
            .context("building the video pipeline")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("not a pipeline"))?;
        let dec = pipeline.by_name("dec").context("no decoder")?;
        // Output each frame as soon as it's decoded; repaint the last good
        // frame rather than show corruption after loss.
        set(&dec, "compliance", "flexible");
        set(&dec, "discard-corrupted-frames", "true");
        set(&dec, "thread-type", "slice");
        let src = pipeline
            .by_name("src")
            .and_then(|s| s.downcast::<gst_app::AppSrc>().ok())
            .context("no appsrc")?;
        src.set_max_bytes(0);
        let sink = pipeline.by_name("sink").context("no sink")?;
        let paintable = sink.property::<gdk::Paintable>("paintable");
        pipeline.set_state(gst::State::Playing).context("starting the video pipeline")?;
        Ok(Self { pipeline, src, paintable, decoder })
    }

    /// A handle for feeding encoded frames from any thread.
    pub fn feed(&self) -> VideoFeed {
        VideoFeed(self.src.clone())
    }

    pub fn bus(&self) -> gst::Bus {
        self.pipeline.bus().expect("pipeline bus")
    }
}

impl Drop for VideoOut {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

/// Send-able handle for pushing frames from the network thread.
#[derive(Clone)]
pub struct VideoFeed(gst_app::AppSrc);

impl VideoFeed {
    pub fn push(&self, data: &[u8]) {
        if let Err(e) = self.0.push_buffer(gst::Buffer::from_slice(data.to_vec())) {
            debug!("video push: {e:?}");
        }
    }
}

/// Opus in, speakers out.
pub struct AudioOut {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
}

impl AudioOut {
    pub fn new() -> Result<Self> {
        // A small output buffer keeps audio close to the picture; PipeWire's
        // pulse server honours it.
        let sink = if gst::ElementFactory::find("pulsesink").is_some() {
            "pulsesink buffer-time=40000 latency-time=10000"
        } else {
            "autoaudiosink"
        };
        let desc = format!(
            "appsrc name=src is-live=true format=time do-timestamp=true \
             caps=audio/x-opus,channel-mapping-family=0,channels=2,rate=48000 \
             ! opusdec plc=true ! audioconvert ! audioresample ! {sink} sync=false"
        );
        let pipeline = gst::parse::launch(&desc)
            .context("building the audio pipeline")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("not a pipeline"))?;
        let src = pipeline
            .by_name("src")
            .and_then(|s| s.downcast::<gst_app::AppSrc>().ok())
            .context("no appsrc")?;
        pipeline.set_state(gst::State::Playing).context("starting audio playback")?;
        Ok(Self { pipeline, src })
    }

    pub fn feed(&self) -> AudioFeed {
        AudioFeed(self.src.clone())
    }
}

impl Drop for AudioOut {
    fn drop(&mut self) {
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

#[derive(Clone)]
pub struct AudioFeed(gst_app::AppSrc);

impl AudioFeed {
    pub fn push(&self, data: &[u8]) {
        if let Err(e) = self.0.push_buffer(gst::Buffer::from_slice(data.to_vec())) {
            warn!("audio push: {e:?}");
        }
    }
}
