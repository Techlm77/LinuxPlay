//! Watching a pipeline for the error that ends it.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use gstreamer as gst;
use gstreamer::prelude::*;
use tracing::{debug, warn};

/// How often the watcher checks whether it should stop.
const POLL: gst::ClockTime = gst::ClockTime::from_mseconds(200);

/// Report the first error (or end of stream) on the pipeline's bus to
/// `on_error`, from a thread that ends soon after `stopped` is set.
pub fn watch(pipeline: &gst::Pipeline, stopped: Arc<AtomicBool>, on_error: impl Fn(String) + Send + 'static) {
    let bus = pipeline.bus().expect("a pipeline always has a bus");
    let name = pipeline.name();
    let spawned = std::thread::Builder::new().name("linuxplay-bus".into()).spawn(move || {
        let started = Instant::now();
        let wanted = [gst::MessageType::Error, gst::MessageType::Eos, gst::MessageType::Warning];
        while !stopped.load(Ordering::Relaxed) {
            let Some(msg) = bus.timed_pop_filtered(POLL, &wanted) else { continue };
            match msg.view() {
                gst::MessageView::Error(e) => {
                    let text = describe(e);
                    warn!("{name} failed after {:?}: {text}", started.elapsed());
                    on_error(text);
                    return;
                }
                gst::MessageView::Eos(_) => {
                    on_error("the stream ended".into());
                    return;
                }
                gst::MessageView::Warning(w) => debug!("{name}: {}", w.error()),
                _ => {}
            }
        }
    });
    if let Err(e) = spawned {
        warn!("couldn't watch the pipeline for errors: {e}");
    }
}

/// An error message as one readable line: where, what, and the detail.
pub fn describe(e: &gst::message::Error) -> String {
    format!(
        "{}: {} ({})",
        e.src().map(|s| s.path_string().to_string()).unwrap_or_default(),
        e.error(),
        e.debug().unwrap_or_default()
    )
}
