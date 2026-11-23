//! Playing the host's sound: Opus in, speakers out.

use anyhow::{Context as _, Result, anyhow};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use linuxplay_media::element;
use tracing::warn;

/// Encoded audio waiting to be played, at most; beyond it the oldest is dropped.
const MAX_QUEUED_BYTES: u64 = 256 << 10;

pub struct AudioOut {
    pipeline: gst::Pipeline,
    src: gst_app::AppSrc,
}

impl AudioOut {
    pub fn new() -> Result<Self> {
        // A small output buffer keeps the sound close to the picture;
        // PipeWire's pulse server honours it.
        let sink = if element::exists("pulsesink") {
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
        let src =
            pipeline.by_name("src").and_then(|s| s.downcast::<gst_app::AppSrc>().ok()).context("no appsrc")?;
        src.set_max_bytes(MAX_QUEUED_BYTES);
        element::set(src.upcast_ref(), "leaky-type", "downstream");
        pipeline.set_state(gst::State::Playing).context("starting audio playback")?;
        Ok(Self { pipeline, src })
    }

    /// A handle for feeding Opus packets from any thread.
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
