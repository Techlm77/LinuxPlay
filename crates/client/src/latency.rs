//! How old each frame is when it reaches the screen, and where the time went.
//!
//! A frame is followed through four moments: captured on the host, received
//! here in full, decoded, and presented. The first is on the host's clock, so
//! the two clocks are aligned from the `Ping`/`Pong` exchange: the reply with
//! the shortest round trip gives the best estimate of the offset.

use std::collections::VecDeque;
use std::sync::Mutex;

use linuxplay_media::clock;
use tracing::trace;

/// Frames tracked between reception and presentation. At most a handful are
/// ever in flight; anything older has been skipped.
const MAX_IN_FLIGHT: usize = 64;
/// Offset samples kept; the one with the shortest round trip wins.
const CLOCK_SAMPLES: usize = 16;

/// Follows one frame, identified by its RTP time.
#[derive(Debug, Clone, Copy)]
struct InFlight {
    rtp: u64,
    received_us: u64,
    decoded_us: Option<u64>,
}

#[derive(Debug, Clone, Copy)]
struct ClockSample {
    rtt_us: u64,
    /// Host clock minus our clock.
    offset_us: i64,
}

/// Running total and peak of one stage's duration.
#[derive(Default, Clone, Copy)]
struct Span {
    sum_us: u64,
    max_us: u64,
}

impl Span {
    fn add(&mut self, us: u64) {
        self.sum_us += us;
        self.max_us = self.max_us.max(us);
    }

    fn stage(&self, frames: u64) -> Stage {
        Stage { avg_ms: self.sum_us as f64 / frames.max(1) as f64 / 1000.0, max_ms: self.max_us as f64 / 1000.0 }
    }
}

#[derive(Default)]
struct Sums {
    frames: u64,
    skipped: u64,
    decode: Span,
    present: Span,
    /// Frames whose capture time is known, which needs both clocks.
    timed: u64,
    /// Capture on the host to arrival here.
    arrive: Span,
    total: Span,
}

#[derive(Default)]
struct Inner {
    in_flight: VecDeque<InFlight>,
    clock: VecDeque<ClockSample>,
    /// Host clock at RTP time zero.
    video_base_us: Option<u64>,
    sums: Sums,
}

/// Shared by the network, decoder and render threads.
#[derive(Default)]
pub struct LatencyTracker {
    inner: Mutex<Inner>,
}

/// How long one stage took, in milliseconds.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct Stage {
    pub avg_ms: f64,
    pub max_ms: f64,
}

/// What happened to the frames presented since the last report.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub struct LatencyReport {
    pub frames: u64,
    /// Frames that arrived but were replaced by a newer one before being shown.
    pub skipped: u64,
    /// Received in full to decoded.
    pub decode: Stage,
    /// Decoded to handed to the display.
    pub present: Stage,
    /// Captured on the host to received here (encoding plus the network);
    /// `None` until the clocks are aligned.
    pub arrive: Option<Stage>,
    /// Captured on the host to handed to the display here.
    pub total: Option<Stage>,
    /// Best round trip seen recently.
    pub rtt_ms: Option<f64>,
}

impl LatencyTracker {
    /// The network delivered a complete frame.
    pub fn received(&self, rtp: u64) {
        trace!("received {rtp}");
        let mut inner = self.inner.lock().unwrap();
        if inner.in_flight.len() == MAX_IN_FLIGHT {
            inner.in_flight.pop_front();
            inner.sums.skipped += 1;
        }
        inner.in_flight.push_back(InFlight { rtp, received_us: clock::monotonic_us(), decoded_us: None });
    }

    /// The decoder produced the frame.
    pub fn decoded(&self, rtp: u64) {
        trace!("decoded {rtp}");
        let now = clock::monotonic_us();
        let mut inner = self.inner.lock().unwrap();
        if let Some(f) = inner.in_flight.iter_mut().find(|f| f.rtp == rtp) {
            f.decoded_us = Some(now);
        }
    }

    /// The frame is on its way to the screen. Older frames still in flight
    /// were skipped in its favour.
    pub fn presented(&self, rtp: u64) {
        trace!("presented {rtp}");
        let now = clock::monotonic_us();
        let mut inner = self.inner.lock().unwrap();
        let Some(pos) = inner.in_flight.iter().position(|f| f.rtp == rtp) else { return };
        let frame = inner.in_flight[pos];
        inner.in_flight.drain(..=pos);
        let capture_us = inner.capture_time(rtp);

        let sums = &mut inner.sums;
        sums.frames += 1;
        sums.skipped += pos as u64;
        let decoded_us = frame.decoded_us.unwrap_or(now);
        sums.decode.add(decoded_us.saturating_sub(frame.received_us));
        sums.present.add(now.saturating_sub(decoded_us));
        if let Some(capture_us) = capture_us {
            sums.timed += 1;
            sums.arrive.add(frame.received_us.saturating_sub(capture_us));
            sums.total.add(now.saturating_sub(capture_us));
        }
    }

    /// A `Pong` came back: `sent_us` is our clock when the `Ping` left and
    /// `host_us` the host's clock when it answered.
    pub fn clock_sample(&self, sent_us: u64, host_us: u64) {
        let now = clock::monotonic_us();
        let rtt_us = now.saturating_sub(sent_us);
        // The host answered half a round trip ago, give or take.
        let offset_us = host_us as i64 - (sent_us + rtt_us / 2) as i64;
        let mut inner = self.inner.lock().unwrap();
        if inner.clock.len() == CLOCK_SAMPLES {
            inner.clock.pop_front();
        }
        inner.clock.push_back(ClockSample { rtt_us, offset_us });
    }

    /// The host said where its video clock starts.
    pub fn set_video_base(&self, base_us: u64) {
        self.inner.lock().unwrap().video_base_us = Some(base_us);
    }

    /// Averages since the last call.
    pub fn report(&self) -> LatencyReport {
        let mut inner = self.inner.lock().unwrap();
        let s = std::mem::take(&mut inner.sums);
        let timed = (s.timed > 0).then_some(s.timed);
        LatencyReport {
            frames: s.frames,
            skipped: s.skipped,
            decode: s.decode.stage(s.frames),
            present: s.present.stage(s.frames),
            arrive: timed.map(|n| s.arrive.stage(n)),
            total: timed.map(|n| s.total.stage(n)),
            rtt_ms: inner.best_clock().map(|c| c.rtt_us as f64 / 1000.0),
        }
    }
}

impl Inner {
    fn best_clock(&self) -> Option<ClockSample> {
        self.clock.iter().copied().min_by_key(|c| c.rtt_us)
    }

    /// When the frame with this RTP time was captured, on our clock.
    fn capture_time(&self, rtp: u64) -> Option<u64> {
        let host_us = self.video_base_us?.checked_add(clock::rtp_to_us(rtp))?;
        let offset = self.best_clock()?.offset_us;
        u64::try_from(host_us as i64 - offset).ok()
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn stages_are_measured_and_skipped_frames_counted() {
        let t = LatencyTracker::default();
        t.received(100);
        t.received(200);
        t.received(300);
        std::thread::sleep(Duration::from_millis(3));
        t.decoded(300);
        std::thread::sleep(Duration::from_millis(2));
        t.presented(300);
        let r = t.report();
        assert_eq!((r.frames, r.skipped), (1, 2));
        assert!(r.decode.avg_ms >= 3.0 && r.decode.avg_ms < 50.0, "{r:?}");
        assert!(r.present.avg_ms >= 2.0 && r.present.avg_ms < 50.0, "{r:?}");
        assert_eq!(r.decode.avg_ms, r.decode.max_ms, "one frame: the average is the peak");
        // No clock yet, so nothing can be said about the host's side.
        assert_eq!((r.total, r.arrive), (None, None));
        // A report starts the next window from zero.
        assert_eq!(t.report().frames, 0);
    }

    #[test]
    fn total_latency_uses_the_aligned_clocks() {
        let t = LatencyTracker::default();
        // The host's clock runs exactly one hour ahead of ours.
        const AHEAD: u64 = 3_600_000_000;
        let now = clock::monotonic_us();
        t.clock_sample(now, now + AHEAD);
        // RTP time zero was 20 ms ago, and this frame is from 10 ms after that.
        t.set_video_base(now + AHEAD - 20_000);
        let rtp = clock::VIDEO_CLOCK_RATE / 100;
        t.received(rtp);
        t.decoded(rtp);
        t.presented(rtp);
        let r = t.report();
        let total = r.total.unwrap().avg_ms;
        assert!((10.0..30.0).contains(&total), "{r:?}");
        assert!(r.arrive.unwrap().avg_ms <= total);
    }

    #[test]
    fn the_shortest_round_trip_sets_the_offset() {
        let t = LatencyTracker::default();
        let now = clock::monotonic_us();
        // A slow, misleading sample and a fast, accurate one.
        t.clock_sample(now.saturating_sub(400_000), now + 5_000_000);
        t.clock_sample(now, now + 1_000);
        let inner = t.inner.lock().unwrap();
        let best = inner.best_clock().unwrap();
        assert!(best.rtt_us < 100_000);
        assert!((best.offset_us - 1_000).abs() < 50_000);
    }

    #[test]
    fn a_stalled_renderer_cannot_grow_the_queue() {
        let t = LatencyTracker::default();
        for rtp in 0..10_000 {
            t.received(rtp);
        }
        assert_eq!(t.inner.lock().unwrap().in_flight.len(), MAX_IN_FLIGHT);
    }
}
