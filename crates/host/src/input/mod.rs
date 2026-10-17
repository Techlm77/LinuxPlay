//! Injecting the client's input into this desktop.

pub mod portal;
pub mod x11;

use std::collections::HashSet;

use linuxplay_proto::input::{InputEvent, MouseButton};
use tokio::sync::mpsc;
use tracing::debug;

pub type InputTx = mpsc::UnboundedSender<InputEvent>;

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
    let (tx, mut rx) = mpsc::unbounded_channel::<InputEvent>();
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
