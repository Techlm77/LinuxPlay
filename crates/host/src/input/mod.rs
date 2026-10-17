//! Injecting the client's input into this desktop.
//!
//! Everything a client sends is untrusted. It passes through an
//! [`InputGate`] (permission, rate limit, validation) and a bounded queue
//! before an [`Injector`] sees it.

pub mod x11;

use std::collections::HashSet;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Instant;

use linuxplay_proto::input::{InputEvent, KEY_MAX, MouseButton};
use linuxplay_proto::{MAX_INPUT_MESSAGE, decode};
use tokio::sync::mpsc;
use tokio::sync::mpsc::error::TrySendError;
use tracing::debug;

/// Events waiting for the injector.
const INPUT_QUEUE: usize = 512;
/// Largest relative motion accepted in one event, in pixels.
const MAX_REL_MOTION: f32 = 4096.0;
/// Largest scroll accepted in one event, in 1/120ths of a notch.
pub const MAX_SCROLL: i32 = 120 * 10;
/// Sustained input events per second a client may send, and the burst.
const RATE_PER_SEC: f64 = 1000.0;
const RATE_BURST: f64 = 2000.0;

/// Can this event be dropped when the backend falls behind? Motion and
/// scrolling can; key and button transitions must arrive, or keys stick.
fn lossy(ev: &InputEvent) -> bool {
    matches!(ev, InputEvent::PointerAbs { .. } | InputEvent::PointerRel { .. } | InputEvent::Scroll { .. })
}

/// The injector's queue could not take a key or button transition.
#[derive(Debug)]
pub struct Overflow;

/// Sending side of an injector's bounded queue.
#[derive(Clone)]
pub struct InputTx(mpsc::Sender<InputEvent>);

impl InputTx {
    /// Queue `ev`. A full queue drops motion (`Ok(false)`) but is an
    /// [`Overflow`] for transitions, which the caller treats as abuse.
    pub fn push(&self, ev: InputEvent) -> Result<bool, Overflow> {
        match self.0.try_send(ev) {
            Ok(()) => Ok(true),
            Err(TrySendError::Full(ev)) if lossy(&ev) => Ok(false),
            Err(TrySendError::Full(_)) => Err(Overflow),
            Err(TrySendError::Closed(_)) => Ok(false),
        }
    }
}

pub fn channel() -> (InputTx, mpsc::Receiver<InputEvent>) {
    let (tx, rx) = mpsc::channel(INPUT_QUEUE);
    (InputTx(tx), rx)
}

/// Reject or clamp what no real input device produces.
pub fn sanitize(ev: InputEvent) -> Option<InputEvent> {
    Some(match ev {
        InputEvent::PointerRel { dx, dy } => {
            if !dx.is_finite() || !dy.is_finite() {
                return None;
            }
            InputEvent::PointerRel {
                dx: dx.clamp(-MAX_REL_MOTION, MAX_REL_MOTION),
                dy: dy.clamp(-MAX_REL_MOTION, MAX_REL_MOTION),
            }
        }
        InputEvent::Scroll { dx, dy } => {
            InputEvent::Scroll { dx: dx.clamp(-MAX_SCROLL, MAX_SCROLL), dy: dy.clamp(-MAX_SCROLL, MAX_SCROLL) }
        }
        InputEvent::Key { code, .. } if code > KEY_MAX => return None,
        other => other,
    })
}

/// What the gate did with one data-channel message.
#[derive(Debug, PartialEq, Eq)]
pub enum Verdict {
    Injected,
    Dropped,
    /// The client is flooding; end the session.
    Abuse(&'static str),
}

/// The server-side check on every input message: is this session allowed
/// to control the desktop right now, is the message sane, and is the client
/// within its rate? Runs on the WebRTC thread, so it never blocks.
pub struct InputGate {
    /// `None` for a view-only session: input is never injected.
    tx: Option<InputTx>,
    /// Cleared the moment the session is revoked.
    active: Arc<AtomicBool>,
    tokens: f64,
    last: Instant,
    warned: bool,
}

impl InputGate {
    pub fn new(tx: Option<InputTx>, active: Arc<AtomicBool>) -> Self {
        Self { tx, active, tokens: RATE_BURST, last: Instant::now(), warned: false }
    }

    pub fn handle(&mut self, bytes: &[u8]) -> Verdict {
        let Some(tx) = &self.tx else {
            if !self.warned {
                self.warned = true;
                debug!("ignoring input from a view-only session");
            }
            return Verdict::Dropped;
        };
        if !self.active.load(Ordering::Acquire) || bytes.len() > MAX_INPUT_MESSAGE {
            return Verdict::Dropped;
        }
        let Some(ev) = decode::<InputEvent>(bytes).ok().and_then(sanitize) else {
            return Verdict::Dropped;
        };

        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * RATE_PER_SEC).min(RATE_BURST);
        if self.tokens < 1.0 {
            return if lossy(&ev) { Verdict::Dropped } else { Verdict::Abuse("input flood") };
        }
        self.tokens -= 1.0;

        match tx.push(ev) {
            Ok(true) => Verdict::Injected,
            Ok(false) => Verdict::Dropped,
            Err(Overflow) => Verdict::Abuse("input backlog"),
        }
    }
}

/// One input backend (XTest, the RemoteDesktop portal, uinput).
pub trait Injector: Send + 'static {
    /// Absolute position, each axis normalised to `0..=POINTER_MAX`.
    fn pointer_abs(&mut self, x: u16, y: u16);
    fn pointer_rel(&mut self, dx: f32, dy: f32);
    fn button(&mut self, button: MouseButton, pressed: bool);
    /// 1/120ths of a notch; positive `dy` is down.
    fn scroll(&mut self, dx: i32, dy: i32);
    /// Linux evdev key code.
    fn key(&mut self, code: u16, pressed: bool);
    /// Push queued events out.
    fn flush(&mut self) {}
}

/// Keys and buttons currently held down, so they can be released if the
/// client goes away mid-press.
#[derive(Default)]
struct Held {
    keys: HashSet<u16>,
    buttons: HashSet<MouseButton>,
}

fn apply(inj: &mut dyn Injector, held: &mut Held, ev: InputEvent) {
    match ev {
        InputEvent::PointerAbs { x, y } => inj.pointer_abs(x, y),
        InputEvent::PointerRel { dx, dy } => inj.pointer_rel(dx, dy),
        InputEvent::Button { button, pressed } => {
            // Drop repeats so the held set stays truthful.
            let changed = if pressed { held.buttons.insert(button) } else { held.buttons.remove(&button) };
            if changed {
                inj.button(button, pressed);
            }
        }
        InputEvent::Scroll { dx, dy } => inj.scroll(dx, dy),
        InputEvent::Key { code, pressed } => {
            // Key repeat arrives as repeated presses; let those through.
            if pressed {
                held.keys.insert(code);
                inj.key(code, true);
            } else if held.keys.remove(&code) {
                inj.key(code, false);
            }
        }
        InputEvent::ReleaseAll => release_all(inj, held),
    }
}

fn release_all(inj: &mut dyn Injector, held: &mut Held) {
    for code in held.keys.drain() {
        inj.key(code, false);
    }
    for button in held.buttons.drain() {
        inj.button(button, false);
    }
}

/// Run an injector on its own thread. Pointer motion that piles up while the
/// backend is busy is merged, so a slow backend can't fall behind the mouse.
/// Everything still held is released when the sender is dropped.
pub fn spawn(mut inj: impl Injector) -> InputTx {
    let (tx, mut rx) = channel();
    std::thread::Builder::new()
        .name("linuxplay-input".into())
        .spawn(move || {
            let mut held = Held::default();
            while let Some(first) = rx.blocking_recv() {
                let mut pending = Some(first);
                while let Some(ev) = pending.take() {
                    let mut ev = ev;
                    // Merge a run of absolute moves into the last one.
                    if matches!(ev, InputEvent::PointerAbs { .. }) {
                        while let Ok(next) = rx.try_recv() {
                            if matches!(next, InputEvent::PointerAbs { .. }) {
                                ev = next;
                            } else {
                                pending = Some(next);
                                break;
                            }
                        }
                    } else {
                        pending = rx.try_recv().ok();
                    }
                    apply(&mut inj, &mut held, ev);
                }
                inj.flush();
            }
            release_all(&mut inj, &mut held);
            inj.flush();
            debug!("input thread done");
        })
        .expect("spawning the input thread");
    tx
}

/// Scale a normalised coordinate to `0..size`.
pub fn scale(v: u16, size: u32) -> u32 {
    (v as u64 * size.saturating_sub(1) as u64 / linuxplay_proto::input::POINTER_MAX as u64) as u32
}

/// An injector that records what it is asked to do, for tests and the
/// built-in test capture.
pub struct Recorder(pub Arc<std::sync::Mutex<Vec<InputEvent>>>);

impl Injector for Recorder {
    fn pointer_abs(&mut self, x: u16, y: u16) {
        self.0.lock().unwrap().push(InputEvent::PointerAbs { x, y });
    }
    fn pointer_rel(&mut self, dx: f32, dy: f32) {
        self.0.lock().unwrap().push(InputEvent::PointerRel { dx, dy });
    }
    fn button(&mut self, button: MouseButton, pressed: bool) {
        self.0.lock().unwrap().push(InputEvent::Button { button, pressed });
    }
    fn scroll(&mut self, dx: i32, dy: i32) {
        self.0.lock().unwrap().push(InputEvent::Scroll { dx, dy });
    }
    fn key(&mut self, code: u16, pressed: bool) {
        self.0.lock().unwrap().push(InputEvent::Key { code, pressed });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use linuxplay_proto::encode;

    fn gate(control: bool) -> (InputGate, mpsc::Receiver<InputEvent>, Arc<AtomicBool>) {
        let (tx, rx) = channel();
        let active = Arc::new(AtomicBool::new(true));
        (InputGate::new(control.then_some(tx), active.clone()), rx, active)
    }

    #[test]
    fn view_only_never_injects() {
        let (mut g, mut rx, _) = gate(false);
        for ev in [
            InputEvent::Key { code: 30, pressed: true },
            InputEvent::Button { button: MouseButton::Left, pressed: true },
            InputEvent::PointerAbs { x: 1, y: 1 },
        ] {
            assert_eq!(g.handle(&encode(&ev)), Verdict::Dropped);
        }
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn revoked_session_stops_injecting_at_once() {
        let (mut g, mut rx, active) = gate(true);
        assert_eq!(g.handle(&encode(&InputEvent::Key { code: 30, pressed: true })), Verdict::Injected);
        active.store(false, Ordering::Release);
        assert_eq!(g.handle(&encode(&InputEvent::Key { code: 31, pressed: true })), Verdict::Dropped);
        assert_eq!(rx.try_recv().unwrap(), InputEvent::Key { code: 30, pressed: true });
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn hostile_values_are_clamped_or_rejected() {
        assert_eq!(sanitize(InputEvent::PointerRel { dx: f32::NAN, dy: 0.0 }), None);
        assert_eq!(sanitize(InputEvent::PointerRel { dx: f32::INFINITY, dy: 0.0 }), None);
        assert_eq!(
            sanitize(InputEvent::PointerRel { dx: 1e30, dy: -1e30 }),
            Some(InputEvent::PointerRel { dx: MAX_REL_MOTION, dy: -MAX_REL_MOTION })
        );
        assert_eq!(
            sanitize(InputEvent::Scroll { dx: i32::MIN, dy: i32::MAX }),
            Some(InputEvent::Scroll { dx: -MAX_SCROLL, dy: MAX_SCROLL })
        );
        assert_eq!(sanitize(InputEvent::Key { code: u16::MAX, pressed: true }), None);
        assert!(sanitize(InputEvent::Key { code: KEY_MAX, pressed: true }).is_some());
    }

    #[test]
    fn oversized_messages_are_dropped() {
        let (mut g, _rx, _) = gate(true);
        assert_eq!(g.handle(&[0u8; MAX_INPUT_MESSAGE + 1]), Verdict::Dropped);
    }

    /// A stalled injector with a flood of input: motion is dropped, memory
    /// stays bounded by the queue, and a flood of key presses ends the session.
    #[test]
    fn floods_stay_bounded() {
        let (mut g, rx, _) = gate(true);
        let motion = encode(&InputEvent::PointerAbs { x: 5, y: 5 });
        let mut verdicts = std::collections::HashMap::new();
        for _ in 0..100_000 {
            *verdicts.entry(format!("{:?}", g.handle(&motion))).or_insert(0) += 1;
        }
        assert!(verdicts["Dropped"] > 90_000);
        assert!(rx.len() <= INPUT_QUEUE);

        let key = encode(&InputEvent::Key { code: 30, pressed: true });
        let mut abuse = false;
        for _ in 0..10_000 {
            if matches!(g.handle(&key), Verdict::Abuse(_)) {
                abuse = true;
                break;
            }
        }
        assert!(abuse, "a key flood against a stuck injector must end the session");
    }

    #[test]
    fn held_keys_are_released_when_the_client_goes() {
        let log = Arc::new(std::sync::Mutex::new(Vec::new()));
        let tx = spawn(Recorder(log.clone()));
        tx.push(InputEvent::Key { code: 30, pressed: true }).unwrap();
        tx.push(InputEvent::Button { button: MouseButton::Left, pressed: true }).unwrap();
        drop(tx);
        for _ in 0..100 {
            if log.lock().unwrap().len() >= 4 {
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let log = log.lock().unwrap();
        assert!(log.contains(&InputEvent::Key { code: 30, pressed: false }));
        assert!(log.contains(&InputEvent::Button { button: MouseButton::Left, pressed: false }));
    }
}
