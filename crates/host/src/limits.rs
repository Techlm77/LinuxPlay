//! Limits on what unauthenticated peers can make the host do.
//!
//! Everything is counted per source (an IPv4 address, or an IPv6 /64), so
//! one noisy peer can't use up the host for everyone, and there are global
//! ceilings and rates on top so rotating addresses doesn't help much either.

use std::collections::{HashMap, VecDeque};
use std::net::{IpAddr, Ipv6Addr};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use tokio::sync::{OwnedSemaphorePermit, Semaphore};

/// Connections still authenticating, in total and from one source.
pub const MAX_PENDING: usize = 16;
pub const MAX_PENDING_PER_SOURCE: usize = 4;

/// Wrong PINs one source gets before it is locked out; each further failure
/// doubles the lockout, up to the maximum.
const FREE_FAILURES: u32 = 3;
const BASE_LOCKOUT: Duration = Duration::from_secs(60);
const MAX_LOCKOUT: Duration = Duration::from_secs(60 * 60);
/// Pairing dialogs open at once, in total and from one source.
const MAX_PAIRING: u32 = 3;
const MAX_PAIRING_PER_SOURCE: u32 = 1;
/// Pairing attempts the host starts in any window, whoever asks.
const GLOBAL_ATTEMPTS: usize = 10;
const GLOBAL_WINDOW: Duration = Duration::from_secs(10 * 60);
/// Sources remembered; beyond this new sources are turned away until old
/// records expire, so the table itself can't be used to exhaust memory.
const MAX_SOURCES: usize = 4096;

/// Who a connection comes from, for counting: the IPv4 address, or the /64
/// of an IPv6 one (a single host usually controls a whole /64).
pub fn source(ip: IpAddr) -> IpAddr {
    match ip.to_canonical() {
        IpAddr::V6(v6) => {
            let s = v6.segments();
            IpAddr::V6(Ipv6Addr::new(s[0], s[1], s[2], s[3], 0, 0, 0, 0))
        }
        v4 => v4,
    }
}

/// Connections that have not finished authenticating.
pub struct Pending {
    global: Arc<Semaphore>,
    per_source: Mutex<HashMap<IpAddr, usize>>,
}

/// Held while a connection authenticates; dropping it frees the slot.
pub struct PendingPermit {
    _global: OwnedSemaphorePermit,
    source: IpAddr,
    owner: Arc<Pending>,
}

impl Drop for PendingPermit {
    fn drop(&mut self) {
        let mut map = self.owner.per_source.lock().unwrap();
        if let Some(n) = map.get_mut(&self.source) {
            *n -= 1;
            if *n == 0 {
                map.remove(&self.source);
            }
        }
    }
}

impl Pending {
    pub fn new() -> Arc<Self> {
        Arc::new(Self { global: Arc::new(Semaphore::new(MAX_PENDING)), per_source: Mutex::default() })
    }

    /// A slot for a new connection from `ip`, or `None` if it should be
    /// dropped straight away.
    pub fn try_acquire(self: &Arc<Self>, ip: IpAddr) -> Option<PendingPermit> {
        let source = source(ip);
        let mut map = self.per_source.lock().unwrap();
        let n = map.get(&source).copied().unwrap_or(0);
        if n >= MAX_PENDING_PER_SOURCE {
            return None;
        }
        let global = self.global.clone().try_acquire_owned().ok()?;
        map.insert(source, n + 1);
        Some(PendingPermit { _global: global, source, owner: self.clone() })
    }
}

struct SourceRecord {
    failures: u32,
    locked_until: Option<Instant>,
    active: u32,
    last_seen: Instant,
}

impl SourceRecord {
    fn new(now: Instant) -> Self {
        Self { failures: 0, locked_until: None, active: 0, last_seen: now }
    }
}

/// Why a pairing attempt may not start.
#[derive(Debug, PartialEq, Eq)]
pub enum PairDenied {
    /// This source guessed wrong too often.
    LockedOut(Duration),
    /// Too many pairing dialogs are open (from this source, or overall).
    Busy,
    /// The host has started too many pairings recently.
    RateLimited(Duration),
}

impl std::fmt::Display for PairDenied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::LockedOut(d) => write!(f, "too many wrong PINs from this device; try again in {} s", d.as_secs() + 1),
            Self::Busy => write!(f, "another pairing is already in progress; try again shortly"),
            Self::RateLimited(d) => write!(f, "too many pairing attempts; try again in {} s", d.as_secs() + 1),
        }
    }
}

#[derive(Default)]
pub struct PairLimiter {
    sources: HashMap<IpAddr, SourceRecord>,
    attempts: VecDeque<Instant>,
    active: u32,
}

impl PairLimiter {
    /// May `source` start pairing now? On `Ok` the attempt is counted and
    /// must be ended with [`finish`](Self::finish).
    pub fn begin(&mut self, source: IpAddr, now: Instant) -> Result<(), PairDenied> {
        self.prune(now);
        if let Some(rec) = self.sources.get(&source) {
            if let Some(until) = rec.locked_until.filter(|u| *u > now) {
                return Err(PairDenied::LockedOut(until - now));
            }
            if rec.active >= MAX_PAIRING_PER_SOURCE {
                return Err(PairDenied::Busy);
            }
        } else if self.sources.len() >= MAX_SOURCES {
            return Err(PairDenied::Busy);
        }
        if self.active >= MAX_PAIRING {
            return Err(PairDenied::Busy);
        }
        if self.attempts.len() >= GLOBAL_ATTEMPTS {
            let oldest = self.attempts[0];
            return Err(PairDenied::RateLimited(GLOBAL_WINDOW.saturating_sub(now - oldest)));
        }
        self.attempts.push_back(now);
        self.active += 1;
        let rec = self.sources.entry(source).or_insert_with(|| SourceRecord::new(now));
        rec.active += 1;
        rec.last_seen = now;
        Ok(())
    }

    /// End an attempt started with [`begin`](Self::begin).
    pub fn finish(&mut self, source: IpAddr, ok: bool, now: Instant) {
        self.active = self.active.saturating_sub(1);
        let rec = self.sources.entry(source).or_insert_with(|| SourceRecord::new(now));
        rec.active = rec.active.saturating_sub(1);
        rec.last_seen = now;
        if ok {
            rec.failures = 0;
            rec.locked_until = None;
            return;
        }
        rec.failures += 1;
        if rec.failures >= FREE_FAILURES {
            let doublings = (rec.failures - FREE_FAILURES).min(10);
            let lockout = (BASE_LOCKOUT * 2u32.pow(doublings)).min(MAX_LOCKOUT);
            rec.locked_until = Some(now + lockout);
        }
    }

    fn prune(&mut self, now: Instant) {
        while self.attempts.front().is_some_and(|t| now.duration_since(*t) >= GLOBAL_WINDOW) {
            self.attempts.pop_front();
        }
        // Forget a source after a quiet hour (and once any lockout is over);
        // its failure count starts again from zero.
        self.sources.retain(|_, r| {
            r.active > 0 || r.locked_until.is_some_and(|u| u > now) || now.duration_since(r.last_seen) < MAX_LOCKOUT
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn ipv6_sources_are_their_64() {
        assert_eq!(source(ip("2001:db8:1:2:aaaa::1")), ip("2001:db8:1:2::"));
        assert_eq!(source(ip("::ffff:192.168.1.5")), ip("192.168.1.5"));
    }

    #[test]
    fn one_source_cannot_take_every_pending_slot() {
        let p = Pending::new();
        let attacker: Vec<_> = (0..10).filter_map(|_| p.try_acquire(ip("10.0.0.66"))).collect();
        assert_eq!(attacker.len(), MAX_PENDING_PER_SOURCE);
        assert!(p.try_acquire(ip("10.0.0.5")).is_some(), "a legitimate client still gets in");
        drop(attacker);
        assert!(p.try_acquire(ip("10.0.0.66")).is_some(), "slots come back when connections end");
    }

    #[test]
    fn pending_is_bounded_overall() {
        let p = Pending::new();
        let held: Vec<_> = (0..64u8).filter_map(|i| p.try_acquire(ip(&format!("10.0.{i}.1")))).collect();
        assert_eq!(held.len(), MAX_PENDING);
    }

    #[test]
    fn bad_guesses_lock_out_only_their_source() {
        let mut l = PairLimiter::default();
        let now = Instant::now();
        let bad = ip("10.0.0.66");
        for _ in 0..FREE_FAILURES {
            l.begin(bad, now).unwrap();
            l.finish(bad, false, now);
        }
        assert!(matches!(l.begin(bad, now), Err(PairDenied::LockedOut(_))));
        // Somebody else can still pair.
        l.begin(ip("10.0.0.5"), now).unwrap();
        l.finish(ip("10.0.0.5"), true, now);
    }

    #[test]
    fn lockout_backs_off_and_expires() {
        let mut l = PairLimiter::default();
        let t0 = Instant::now();
        let bad = ip("10.0.0.66");
        for _ in 0..FREE_FAILURES {
            l.begin(bad, t0).unwrap();
            l.finish(bad, false, t0);
        }
        let t1 = t0 + BASE_LOCKOUT + Duration::from_secs(1);
        l.begin(bad, t1).unwrap();
        l.finish(bad, false, t1);
        // The next lockout is twice as long.
        let Err(PairDenied::LockedOut(left)) = l.begin(bad, t1) else { panic!("expected a lockout") };
        assert!(left > BASE_LOCKOUT && left <= BASE_LOCKOUT * 2);
        assert!(l.begin(bad, t1 + BASE_LOCKOUT * 2 + Duration::from_secs(1)).is_ok());
    }

    #[test]
    fn one_open_dialog_per_source() {
        let mut l = PairLimiter::default();
        let now = Instant::now();
        l.begin(ip("10.0.0.66"), now).unwrap();
        assert_eq!(l.begin(ip("10.0.0.66"), now), Err(PairDenied::Busy));
        l.finish(ip("10.0.0.66"), false, now);
        assert!(l.begin(ip("10.0.0.66"), now).is_ok());
    }

    #[test]
    fn rotating_sources_hit_the_global_rate() {
        let mut l = PairLimiter::default();
        let now = Instant::now();
        for i in 0..GLOBAL_ATTEMPTS {
            let s = ip(&format!("10.1.{i}.1"));
            l.begin(s, now).unwrap();
            l.finish(s, false, now);
        }
        assert!(matches!(l.begin(ip("10.2.0.1"), now), Err(PairDenied::RateLimited(_))));
        assert!(l.begin(ip("10.2.0.1"), now + GLOBAL_WINDOW).is_ok(), "the rate recovers");
    }

    #[test]
    fn source_table_is_bounded() {
        let mut l = PairLimiter::default();
        let now = Instant::now();
        for i in 0..(MAX_SOURCES as u32 + 100) {
            let s = IpAddr::V4(std::net::Ipv4Addr::from(0x0a00_0000 + i));
            // Fail once each without hitting the global rate.
            l.attempts.clear();
            if l.begin(s, now).is_ok() {
                l.finish(s, false, now);
            }
        }
        assert!(l.sources.len() <= MAX_SOURCES);
        // And it drains: an hour later, new sources are welcome again.
        assert!(l.begin(ip("10.200.0.1"), now + MAX_LOCKOUT + Duration::from_secs(1)).is_ok());
    }
}
