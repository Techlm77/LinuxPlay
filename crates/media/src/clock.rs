//! The clock latency is measured against.
//!
//! Both ends timestamp with `CLOCK_MONOTONIC`, which is also what GStreamer's
//! system clock reads, so a pipeline's base time and [`monotonic_us`] are on
//! the same timeline. Video travels with a 90 kHz RTP clock that starts at
//! zero when the host's pipeline starts.

/// RTP clock rate of every video codec we send.
pub const VIDEO_CLOCK_RATE: u64 = 90_000;

/// Microseconds on this machine's monotonic clock.
pub fn monotonic_us() -> u64 {
    let mut ts = libc::timespec { tv_sec: 0, tv_nsec: 0 };
    // SAFETY: `ts` is a valid, writable timespec; CLOCK_MONOTONIC always exists.
    unsafe { libc::clock_gettime(libc::CLOCK_MONOTONIC, &mut ts) };
    ts.tv_sec as u64 * 1_000_000 + ts.tv_nsec as u64 / 1_000
}

/// Video RTP ticks to microseconds.
pub fn rtp_to_us(ticks: u64) -> u64 {
    (ticks as u128 * 1_000_000 / VIDEO_CLOCK_RATE as u128) as u64
}

/// Video RTP ticks to nanoseconds, for GStreamer timestamps. Rounds to the
/// nearest nanosecond, so [`ns_to_rtp`] gets the same tick back.
pub fn rtp_to_ns(ticks: u64) -> u64 {
    let rate = VIDEO_CLOCK_RATE as u128;
    ((ticks as u128 * 1_000_000_000 + rate / 2) / rate) as u64
}

/// Nanoseconds to the nearest video RTP tick.
pub fn ns_to_rtp(ns: u64) -> u64 {
    ((ns as u128 * VIDEO_CLOCK_RATE as u128 + 500_000_000) / 1_000_000_000) as u64
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn monotonic_clock_advances() {
        let a = monotonic_us();
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(monotonic_us() >= a + 2_000);
    }

    #[test]
    fn rtp_conversions_round_trip() {
        assert_eq!(rtp_to_us(VIDEO_CLOCK_RATE), 1_000_000);
        assert_eq!(rtp_to_ns(1_500), 16_666_667);
        assert_eq!(ns_to_rtp(1_000_000_000), VIDEO_CLOCK_RATE);
        // A tick is not a whole number of nanoseconds, yet survives the trip
        // through a GStreamer timestamp.
        for ticks in (0..200_000).chain(u32::MAX as u64 - 1_000..u32::MAX as u64 + 1_000) {
            assert_eq!(ns_to_rtp(rtp_to_ns(ticks)), ticks);
        }
        // Hours of ticks must not overflow.
        assert_eq!(rtp_to_us(VIDEO_CLOCK_RATE * 3600 * 100), 3600 * 100 * 1_000_000);
    }
}
