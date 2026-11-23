//! Slowing down whoever tries too often: a count per key in a fixed window.

use std::collections::HashMap;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// Keys remembered at most; beyond it, finished windows are forgotten.
const MAX_KEYS: usize = 100_000;

pub struct RateLimiter {
    window: Duration,
    max: u32,
    hits: Mutex<HashMap<String, (Instant, u32)>>,
}

impl RateLimiter {
    /// Allow `max` attempts per key in every `window`.
    pub fn new(max: u32, window: Duration) -> Self {
        Self { window, max, hits: Mutex::default() }
    }

    /// Count an attempt for `key`; returns whether it is still within the limit.
    pub fn allow(&self, key: &str) -> bool {
        self.allow_at(key, Instant::now())
    }

    fn allow_at(&self, key: &str, now: Instant) -> bool {
        let mut hits = self.hits.lock().unwrap();
        if hits.len() >= MAX_KEYS {
            hits.retain(|_, (start, _)| now.duration_since(*start) < self.window);
        }
        let entry = hits.entry(key.to_string()).or_insert((now, 0));
        if now.duration_since(entry.0) >= self.window {
            *entry = (now, 0);
        }
        entry.1 += 1;
        entry.1 <= self.max
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attempts_beyond_the_limit_wait_for_the_next_window() {
        let limiter = RateLimiter::new(3, Duration::from_secs(60));
        let start = Instant::now();
        assert!((0..3).all(|_| limiter.allow_at("ip", start)));
        assert!(!limiter.allow_at("ip", start));
        assert!(limiter.allow_at("another ip", start), "limits are per key");
        assert!(!limiter.allow_at("ip", start + Duration::from_secs(59)));
        assert!(limiter.allow_at("ip", start + Duration::from_secs(60)));
    }
}
