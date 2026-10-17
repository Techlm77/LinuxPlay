//! Desktop audio: whatever plays on the default output, as 10 ms Opus frames.

use std::sync::atomic::AtomicBool;
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use gstreamer as gst;
use gstreamer::prelude::*;
use gstreamer_app as gst_app;
use linuxplay_net::SessionHandle;
use tracing::info;

use crate::video::watch_bus;

pub struct AudioPipeline {
    pipeline: gst::Pipeline,
    session: Arc<OnceLock<SessionHandle>>,
    stopped: Arc<AtomicBool>,
}

fn source() -> Result<&'static str> {
    if gst::ElementFactory::find("pulsesrc").is_some() {
        // Works with PulseAudio and with PipeWire's pulse server.
        Ok("pulsesrc device=@DEFAULT_MONITOR@ buffer-time=20000 latency-time=5000")
    } else if gst::ElementFactory::find("pipewiresrc").is_some() {
        Ok("pipewiresrc stream-properties=\"props,stream.capture.sink=true\"")
    } else {
        bail!("neither pulsesrc nor pipewiresrc is installed")
    }
}

impl AudioPipeline {
    pub fn start(on_error: impl Fn(String) + Send + 'static) -> Result<Arc<Self>> {
        let desc = format!(
            "{} ! audioconvert ! audioresample ! audio/x-raw,rate=48000,channels=2 \
             ! opusenc bitrate=128000 frame-size=10 audio-type=restricted-lowdelay \
             ! appsink name=sink sync=false async=false max-buffers=16 enable-last-sample=false",
            source()?
        );
        info!("audio pipeline: {desc}");
        let pipeline = gst::parse::launch(&desc)
            .context("building the audio pipeline")?
            .downcast::<gst::Pipeline>()
            .map_err(|_| anyhow!("not a pipeline"))?;
        let sink = pipeline
            .by_name("sink")
            .and_then(|s| s.downcast::<gst_app::AppSink>().ok())
            .context("no appsink")?;

        let session: Arc<OnceLock<SessionHandle>> = Arc::default();
        {
            let session = session.clone();
            sink.set_callbacks(
                gst_app::AppSinkCallbacks::builder()
                    .new_sample(move |sink| {
                        let sample = sink.pull_sample().map_err(|_| gst::FlowError::Eos)?;
                        let buffer = sample.buffer().ok_or(gst::FlowError::Error)?;
                        let pts = buffer.pts().unwrap_or(gst::ClockTime::ZERO);
                        let map = buffer.map_readable().map_err(|_| gst::FlowError::Error)?;
                        if let Some(h) = session.get() {
                            h.write_audio(map.as_slice().to_vec(), Duration::from_nanos(pts.nseconds()));
                        }
                        Ok(gst::FlowSuccess::Ok)
                    })
                    .build(),
            );
        }

        let stopped = Arc::new(AtomicBool::new(false));
        watch_bus(&pipeline, stopped.clone(), on_error);
        pipeline.set_state(gst::State::Playing).context("starting audio capture")?;
        Ok(Arc::new(Self { pipeline, session, stopped }))
    }

    pub fn attach(&self, session: SessionHandle) {
        let _ = self.session.set(session);
    }

    pub fn stop(&self) {
        self.stopped.store(true, std::sync::atomic::Ordering::Relaxed);
        let _ = self.pipeline.set_state(gst::State::Null);
    }
}

impl Drop for AudioPipeline {
    fn drop(&mut self) {
        self.stop();
    }
}
