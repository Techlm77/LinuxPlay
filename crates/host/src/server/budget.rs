//! Sharing one link and one encoder between the displays being streamed.

use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Mutex, Weak};

use linuxplay_net::SessionHandle;
use tracing::info;

use crate::quality;
use crate::video::VideoPipeline;

/// Roughly the encoder's throughput, in pixels per second, split across the
/// active streams. About three 1080p60 streams; above it, framerates drop.
const ENCODER_PIXEL_BUDGET: u64 = 1920 * 1080 * 60 * 3;

/// One streaming display, as the budget controller sees it.
pub(super) struct StreamSlot {
    pub(super) monitor: u32,
    /// Bitrate this resolution/codec wants on its own, kbit/s.
    pub(super) res_cap_kbps: u32,
    pub(super) pixels: u64,
    pub(super) fps_cap: u32,
    pub(super) video: Weak<VideoPipeline>,
    pub(super) session: SessionHandle,
}

/// Divides the link bandwidth and the encoder's throughput across the active
/// displays, re-running whenever one joins or leaves — the "re-benchmark" that
/// keeps two displays from each assuming they own the whole pipe.
#[derive(Default)]
pub(super) struct Budget {
    link_ceiling_kbps: AtomicU32,
    slots: Mutex<Vec<StreamSlot>>,
}

impl Budget {
    /// Set the link ceiling from the first stream's link type (once).
    pub(super) fn init_ceiling(&self, lan: bool) {
        let _ = self.link_ceiling_kbps.compare_exchange(
            0,
            quality::link_ceiling(lan),
            Ordering::SeqCst,
            Ordering::SeqCst,
        );
    }

    pub(super) fn register(&self, slot: StreamSlot) {
        self.slots.lock().unwrap().push(slot);
        self.rebalance();
    }

    pub(super) fn unregister(&self, monitor: u32) {
        self.slots.lock().unwrap().retain(|s| s.monitor != monitor);
        self.rebalance();
    }

    /// Recompute every active stream's bitrate ceiling and framerate cap.
    fn rebalance(&self) {
        let slots = self.slots.lock().unwrap();
        let n = slots.len().max(1) as u32;
        let ceiling = self.link_ceiling_kbps.load(Ordering::Relaxed).max(2_000);
        let per_stream_bw = (ceiling / n).max(1_000);

        let total_pixel_rate: u64 = slots.iter().map(|s| s.pixels * s.fps_cap as u64).sum();
        let fps_scale = if total_pixel_rate > ENCODER_PIXEL_BUDGET {
            ENCODER_PIXEL_BUDGET as f64 / total_pixel_rate as f64
        } else {
            1.0
        };

        for s in slots.iter() {
            let Some(video) = s.video.upgrade() else { continue };
            let bw = s.res_cap_kbps.min(per_stream_bw);
            video.set_max_kbps(bw);
            s.session.set_desired_bitrate(bw as u64 * 1000);
            let fps = ((s.fps_cap as f64 * fps_scale).floor() as u32).clamp(24, s.fps_cap);
            video.set_fps_cap(fps);
        }

        if n > 1 {
            info!(
                "re-benchmarked for {n} displays: ~{per_stream_bw} kbit/s each{}",
                if fps_scale < 0.99 { format!(", framerate ×{fps_scale:.2}") } else { String::new() },
            );
        }
    }
}
