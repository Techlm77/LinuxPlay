//! Virtual gamepads: one kernel input device for every gamepad the client
//! has plugged in, so games on this computer see a controller of their own.
//!
//! The devices are made through `/dev/uinput`. They present the layout of an
//! Xbox 360 pad as Linux's own driver for it reports it, which is the layout
//! every game and mapping database knows.
//!
//! Each device has a thread of its own that owns it: it applies the client's
//! updates, and hands rumble effects that games play on it to [`Feedback`],
//! which sends them back to the client's real controller.

use std::collections::HashMap;
use std::os::fd::AsRawFd;
use std::sync::mpsc::{self, RecvTimeoutError};
use std::sync::{Arc, OnceLock};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use evdev::uinput::VirtualDevice;
use evdev::{
    AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventSummary, EventType, FFEffectCode, FFEffectKind, InputId,
    KeyCode, UInputCode, UinputAbsSetup,
};
use linuxplay_proto::control::Rumble;
use linuxplay_proto::input::{GamepadButton, GamepadState, InputEvent, MAX_GAMEPADS};
use tracing::{debug, info, warn};

/// How often a gamepad's thread looks for rumble effects while idle.
const FEEDBACK_POLL: Duration = Duration::from_millis(4);
/// Rumble effects a game may keep on one pad at once.
const MAX_EFFECTS: u32 = 16;
/// The longest the client plays one rumble command; effects that run longer
/// (or forever) are sent again before it runs out.
const RUMBLE_SLICE_MS: u16 = 2_000;

/// Where rumble for the client's controllers goes. Created with the capture,
/// connected to the session once the session exists.
#[derive(Clone, Default)]
pub struct Feedback(Arc<OnceLock<RumbleSink>>);

/// Sends one rumble command to the client.
type RumbleSink = Box<dyn Fn(Rumble) + Send + Sync>;

impl Feedback {
    /// Send rumble with `send` from now on.
    pub fn connect(&self, send: impl Fn(Rumble) + Send + Sync + 'static) {
        let _ = self.0.set(Box::new(send));
    }

    fn rumble(&self, effect: Rumble) {
        if let Some(send) = self.0.get() {
            send(effect);
        }
    }
}

/// USB identity of the Xbox 360 wired pad, which decides how games map the
/// buttons below.
const VENDOR: u16 = 0x045e;
const PRODUCT: u16 = 0x028e;
const VERSION: u16 = 0x0114;

const STICK_MIN: i32 = i16::MIN as i32;
const STICK_MAX: i32 = i16::MAX as i32;
/// Noise filter and dead zone the real pad's driver declares for its sticks.
const STICK_FUZZ: i32 = 16;
const STICK_FLAT: i32 = 128;
const TRIGGER_MAX: i32 = 255;

/// The key code each button is reported as. West and North look swapped
/// against the kernel's names because that is what the real driver does.
fn key(button: GamepadButton) -> Option<KeyCode> {
    Some(match button {
        GamepadButton::South => KeyCode::BTN_SOUTH,
        GamepadButton::East => KeyCode::BTN_EAST,
        GamepadButton::West => KeyCode::BTN_NORTH,
        GamepadButton::North => KeyCode::BTN_WEST,
        GamepadButton::LeftShoulder => KeyCode::BTN_TL,
        GamepadButton::RightShoulder => KeyCode::BTN_TR,
        GamepadButton::Back => KeyCode::BTN_SELECT,
        GamepadButton::Start => KeyCode::BTN_START,
        GamepadButton::Guide => KeyCode::BTN_MODE,
        GamepadButton::LeftStick => KeyCode::BTN_THUMBL,
        GamepadButton::RightStick => KeyCode::BTN_THUMBR,
        // The directional pad is a pair of axes, not keys.
        GamepadButton::DpadUp | GamepadButton::DpadDown | GamepadButton::DpadLeft | GamepadButton::DpadRight => {
            return None;
        }
    })
}

/// One control changing value: `(event type, code, value)`.
type Change = (EventType, u16, i32);

/// A trigger's `0..=u16::MAX` on the wire as the pad's `0..=255`.
fn trigger(value: u16) -> i32 {
    (value as i32 * TRIGGER_MAX + u16::MAX as i32 / 2) / u16::MAX as i32
}

/// The directional pad as an axis: -1, 0 or 1.
fn hat(negative: bool, positive: bool) -> i32 {
    positive as i32 - negative as i32
}

/// What has to be reported to get from `old` to `new`.
fn changes(old: &GamepadState, new: &GamepadState) -> Vec<Change> {
    let mut out = Vec::new();
    for button in GamepadButton::ALL {
        if let (Some(key), true) = (key(button), old.pressed(button) != new.pressed(button)) {
            out.push((EventType::KEY, key.code(), new.pressed(button) as i32));
        }
    }
    let axes = |s: &GamepadState| {
        use GamepadButton::*;
        [
            (AbsoluteAxisCode::ABS_X, s.left_x as i32),
            (AbsoluteAxisCode::ABS_Y, s.left_y as i32),
            (AbsoluteAxisCode::ABS_RX, s.right_x as i32),
            (AbsoluteAxisCode::ABS_RY, s.right_y as i32),
            (AbsoluteAxisCode::ABS_Z, trigger(s.left_trigger)),
            (AbsoluteAxisCode::ABS_RZ, trigger(s.right_trigger)),
            (AbsoluteAxisCode::ABS_HAT0X, hat(s.pressed(DpadLeft), s.pressed(DpadRight))),
            (AbsoluteAxisCode::ABS_HAT0Y, hat(s.pressed(DpadUp), s.pressed(DpadDown))),
        ]
    };
    for ((axis, before), (_, after)) in axes(old).into_iter().zip(axes(new)) {
        if before != after {
            out.push((EventType::ABSOLUTE, axis.0, after));
        }
    }
    out
}

/// Create the kernel device for the client's gamepad number `pad`.
fn create(pad: u8) -> std::io::Result<VirtualDevice> {
    let keys: AttributeSet<KeyCode> = GamepadButton::ALL.into_iter().filter_map(key).collect();
    let axis = |code, min, max, fuzz, flat| UinputAbsSetup::new(code, AbsInfo::new(0, min, max, fuzz, flat, 0));
    let stick = |code| axis(code, STICK_MIN, STICK_MAX, STICK_FUZZ, STICK_FLAT);
    let rumble: AttributeSet<FFEffectCode> = [FFEffectCode::FF_RUMBLE, FFEffectCode::FF_GAIN].into_iter().collect();
    VirtualDevice::builder()?
        .name(&format!("LinuxPlay Gamepad {}", pad + 1))
        .input_id(InputId::new(BusType::BUS_USB, VENDOR, PRODUCT, VERSION))
        .with_keys(&keys)?
        .with_absolute_axis(&stick(AbsoluteAxisCode::ABS_X))?
        .with_absolute_axis(&stick(AbsoluteAxisCode::ABS_Y))?
        .with_absolute_axis(&stick(AbsoluteAxisCode::ABS_RX))?
        .with_absolute_axis(&stick(AbsoluteAxisCode::ABS_RY))?
        .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_Z, 0, TRIGGER_MAX, 0, 0))?
        .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_RZ, 0, TRIGGER_MAX, 0, 0))?
        .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_HAT0X, -1, 1, 0, 0))?
        .with_absolute_axis(&axis(AbsoluteAxisCode::ABS_HAT0Y, -1, 1, 0, 0))?
        .with_ff(&rumble)?
        .with_ff_effects_max(MAX_EFFECTS)
        .build()
}

/// The rumble effects a game has uploaded to one pad, and what is playing.
#[derive(Default)]
struct Effects {
    /// Strong and weak motor strength and length in ms (0 = until stopped),
    /// by the id the kernel gave the effect.
    uploaded: HashMap<i16, (u16, u16, u16)>,
    /// The effect playing, when it started, and when it was last sent.
    playing: Option<(i16, Instant, Instant)>,
    /// The game's overall strength, 0..=0xffff.
    gain: u16,
}

impl Effects {
    fn new() -> Self {
        Self { gain: u16::MAX, ..Default::default() }
    }

    /// The command for the client when effect `id` starts (or `None` to stop).
    fn play(&mut self, pad: u8, id: i16, start: bool, now: Instant) -> Option<Rumble> {
        if !start {
            if self.playing.is_some_and(|(playing, ..)| playing == id) {
                self.playing = None;
                return Some(Rumble { pad, low: 0, high: 0, duration_ms: 0 });
            }
            return None;
        }
        let &(strong, weak, length) = self.uploaded.get(&id)?;
        self.playing = Some((id, now, now));
        Some(self.command(pad, strong, weak, length, 0))
    }

    /// A slice of a long effect that is about to run out on the client.
    fn refresh(&mut self, pad: u8, now: Instant) -> Option<Rumble> {
        let (id, started, sent) = self.playing?;
        let &(strong, weak, length) = self.uploaded.get(&id)?;
        let elapsed = now.duration_since(started).as_millis() as u64;
        if length != 0 && elapsed >= length as u64 {
            self.playing = None;
            return None;
        }
        if now.duration_since(sent) < Duration::from_millis(RUMBLE_SLICE_MS as u64 * 3 / 4) {
            return None;
        }
        self.playing = Some((id, started, now));
        Some(self.command(pad, strong, weak, length, elapsed))
    }

    fn command(&self, pad: u8, strong: u16, weak: u16, length: u16, elapsed: u64) -> Rumble {
        let scale = |m: u16| (m as u32 * self.gain as u32 / u16::MAX as u32) as u16;
        let left = if length == 0 { RUMBLE_SLICE_MS } else { (length as u64).saturating_sub(elapsed).min(RUMBLE_SLICE_MS as u64) as u16 };
        Rumble { pad, low: scale(strong), high: scale(weak), duration_ms: left }
    }
}

/// A command for a gamepad's thread.
enum PadCommand {
    State(GamepadState),
}

/// One virtual gamepad and the thread that owns it. Dropping it removes the
/// device.
struct Pad {
    commands: Option<mpsc::Sender<PadCommand>>,
    thread: Option<JoinHandle<()>>,
}

impl Pad {
    fn start(pad: u8, mut device: VirtualDevice, feedback: Feedback) -> std::io::Result<Self> {
        // Reads of rumble requests must never block the updates.
        // SAFETY: fcntl on a descriptor this device owns.
        unsafe {
            let fd = device.as_raw_fd();
            libc::fcntl(fd, libc::F_SETFL, libc::fcntl(fd, libc::F_GETFL) | libc::O_NONBLOCK);
        }
        let (commands, inbox) = mpsc::channel();
        let thread = std::thread::Builder::new().name(format!("linuxplay-pad-{}", pad + 1)).spawn(move || {
            let mut state = GamepadState { pad, ..Default::default() };
            let mut effects = Effects::new();
            loop {
                match inbox.recv_timeout(FEEDBACK_POLL) {
                    Ok(PadCommand::State(new)) => {
                        apply(&mut device, &state, &new);
                        state = new;
                    }
                    Err(RecvTimeoutError::Timeout) => {}
                    Err(RecvTimeoutError::Disconnected) => break,
                }
                serve_feedback(&mut device, pad, &mut effects, &feedback);
            }
            // Stop the client's motors if the pad goes mid-rumble.
            if effects.playing.is_some() {
                feedback.rumble(Rumble { pad, low: 0, high: 0, duration_ms: 0 });
            }
        })?;
        Ok(Self { commands: Some(commands), thread: Some(thread) })
    }

    fn update(&self, state: GamepadState) {
        if let Some(commands) = &self.commands {
            let _ = commands.send(PadCommand::State(state));
        }
    }
}

impl Drop for Pad {
    fn drop(&mut self) {
        self.commands = None;
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// Report what changed between two states to the kernel.
fn apply(device: &mut VirtualDevice, old: &GamepadState, new: &GamepadState) {
    let events: Vec<evdev::InputEvent> =
        changes(old, new).into_iter().map(|(kind, code, value)| evdev::InputEvent::new(kind.0, code, value)).collect();
    if !events.is_empty()
        && let Err(e) = device.emit(&events) {
            warn!("gamepad {}: {e}", new.pad + 1);
        }
}

/// Answer the kernel's rumble requests: games upload effects, then play and
/// stop them.
fn serve_feedback(device: &mut VirtualDevice, pad: u8, effects: &mut Effects, feedback: &Feedback) {
    let now = Instant::now();
    if let Some(command) = effects.refresh(pad, now) {
        feedback.rumble(command);
    }
    let events: Vec<evdev::InputEvent> = match device.fetch_events() {
        Ok(events) => events.collect(),
        Err(e) if e.kind() == std::io::ErrorKind::WouldBlock => return,
        Err(e) => return debug!("gamepad {}: {e}", pad + 1),
    };
    for event in events {
        match event.destructure() {
            EventSummary::UInput(event, UInputCode::UI_FF_UPLOAD, _) => {
                let Ok(mut upload) = device.process_ff_upload(event) else { continue };
                let effect = upload.effect();
                if let FFEffectKind::Rumble { strong_magnitude, weak_magnitude } = effect.kind {
                    effects.uploaded.insert(upload.effect_id(), (strong_magnitude, weak_magnitude, effect.replay.length));
                    upload.set_retval(0);
                } else {
                    // Only rumble is offered; anything else is refused.
                    upload.set_retval(-libc::EINVAL);
                }
            }
            EventSummary::UInput(event, UInputCode::UI_FF_ERASE, _) => {
                if let Ok(erase) = device.process_ff_erase(event) {
                    effects.uploaded.remove(&(erase.effect_id() as i16));
                }
            }
            EventSummary::ForceFeedback(_, FFEffectCode::FF_GAIN, value) => effects.gain = value.clamp(0, u16::MAX as i32) as u16,
            EventSummary::ForceFeedback(_, FFEffectCode(id), value) => {
                if let Some(command) = effects.play(pad, id as i16, value > 0, now) {
                    feedback.rumble(command);
                }
            }
            _ => {}
        }
    }
}

/// The virtual gamepads of one session. Dropping it removes them all.
#[derive(Default)]
pub struct Gamepads {
    pads: [Option<Pad>; MAX_GAMEPADS as usize],
    /// Creating a device failed once; don't retry (or warn) for every event.
    unavailable: bool,
    feedback: Feedback,
}

impl Gamepads {
    /// Gamepads whose rumble goes to `feedback`.
    pub fn new(feedback: Feedback) -> Self {
        Self { feedback, ..Default::default() }
    }

    /// Act on `event` if it concerns a gamepad; returns whether it did.
    pub fn handle(&mut self, event: &InputEvent) -> bool {
        match *event {
            InputEvent::GamepadAttach { pad, .. } => self.attach(pad),
            InputEvent::Gamepad(state) => self.update(state),
            InputEvent::GamepadDetach { pad } => {
                if self.slot(pad).is_some_and(|slot| slot.take().is_some()) {
                    info!("gamepad {} removed", pad + 1);
                }
            }
            _ => return false,
        }
        true
    }

    fn slot(&mut self, pad: u8) -> Option<&mut Option<Pad>> {
        self.pads.get_mut(pad as usize)
    }

    fn attach(&mut self, pad: u8) {
        if self.unavailable {
            return;
        }
        let Some(slot) = self.pads.get_mut(pad as usize) else { return };
        if slot.is_some() {
            return;
        }
        match create(pad).and_then(|device| Pad::start(pad, device, self.feedback.clone())) {
            Ok(started) => {
                info!("gamepad {} connected", pad + 1);
                *slot = Some(started);
            }
            Err(e) => {
                self.unavailable = true;
                warn!(
                    "the client's gamepads can't be used here: creating a virtual gamepad through \
                     /dev/uinput failed ({e}). Give the logged-in user access with LinuxPlay's udev \
                     rule, 70-linuxplay-uinput.rules (see the README)."
                );
            }
        }
    }

    fn update(&mut self, state: GamepadState) {
        if let Some(Some(pad)) = self.slot(state.pad) {
            pad.update(state);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn state(edit: impl FnOnce(&mut GamepadState)) -> GamepadState {
        let mut s = GamepadState::default();
        edit(&mut s);
        s
    }

    #[test]
    fn only_what_changed_is_reported() {
        let idle = GamepadState::default();
        assert!(changes(&idle, &idle).is_empty());

        let pressed = state(|s| {
            s.set(GamepadButton::South, true);
            s.left_x = -1000;
        });
        assert_eq!(
            changes(&idle, &pressed),
            vec![(EventType::KEY, KeyCode::BTN_SOUTH.code(), 1), (EventType::ABSOLUTE, AbsoluteAxisCode::ABS_X.0, -1000)]
        );
        assert_eq!(changes(&pressed, &state(|s| s.left_x = -1000)), vec![(EventType::KEY, KeyCode::BTN_SOUTH.code(), 0)]);
    }

    #[test]
    fn the_directional_pad_is_a_pair_of_axes() {
        let idle = GamepadState::default();
        let up_left = state(|s| {
            s.set(GamepadButton::DpadUp, true);
            s.set(GamepadButton::DpadLeft, true);
        });
        assert_eq!(
            changes(&idle, &up_left),
            vec![(EventType::ABSOLUTE, AbsoluteAxisCode::ABS_HAT0X.0, -1), (EventType::ABSOLUTE, AbsoluteAxisCode::ABS_HAT0Y.0, -1)]
        );
        // Opposite directions held together cancel out.
        let both = state(|s| {
            s.set(GamepadButton::DpadLeft, true);
            s.set(GamepadButton::DpadRight, true);
        });
        assert!(changes(&idle, &both).is_empty());
    }

    #[test]
    fn triggers_scale_to_the_pads_range() {
        assert_eq!(trigger(0), 0);
        assert_eq!(trigger(u16::MAX), TRIGGER_MAX);
        assert_eq!(trigger(u16::MAX / 2), 127);
    }

    #[test]
    fn events_for_unknown_or_unattached_pads_are_ignored() {
        let mut pads = Gamepads { unavailable: true, ..Default::default() };
        assert!(pads.handle(&InputEvent::GamepadAttach { pad: 0, kind: Default::default() }));
        assert!(pads.handle(&InputEvent::Gamepad(state(|s| s.pad = 200))));
        assert!(pads.handle(&InputEvent::GamepadDetach { pad: 3 }));
        assert!(!pads.handle(&InputEvent::ReleaseAll));
    }

    #[test]
    fn effects_start_stop_and_stay_alive_on_the_client() {
        let mut effects = Effects::new();
        let t0 = Instant::now();
        effects.uploaded.insert(3, (40_000, 10_000, 300));
        effects.uploaded.insert(4, (u16::MAX, 0, 0));
        assert_eq!(effects.play(0, 9, true, t0), None, "an effect nobody uploaded");
        assert_eq!(effects.play(1, 3, true, t0), Some(Rumble { pad: 1, low: 40_000, high: 10_000, duration_ms: 300 }));
        assert_eq!(effects.play(1, 3, false, t0), Some(Rumble { pad: 1, low: 0, high: 0, duration_ms: 0 }));
        assert_eq!(effects.play(1, 3, false, t0), None, "stopping twice says nothing new");

        // One that runs until stopped is sent again before the client's copy ends.
        effects.play(0, 4, true, t0).unwrap();
        assert_eq!(effects.refresh(0, t0 + Duration::from_millis(100)), None);
        let again = effects.refresh(0, t0 + Duration::from_millis(1_600)).unwrap();
        assert_eq!((again.low, again.duration_ms), (u16::MAX, RUMBLE_SLICE_MS));

        // The game's gain scales the motors.
        effects.gain = u16::MAX / 2;
        assert_eq!(effects.play(0, 3, true, t0).unwrap().low, 19_999);
    }

    /// A real device, with a game's view of it: needs access to /dev/uinput
    /// and the input group. `cargo test -p linuxplay-host -- --ignored a_virtual_gamepad`.
    #[test]
    #[ignore]
    fn a_virtual_gamepad_appears_and_rumbles_like_a_real_one() {
        use evdev::{FFEffectData, FFReplay, FFTrigger};
        use std::sync::Mutex;

        let got: Arc<Mutex<Vec<Rumble>>> = Arc::default();
        let feedback = Feedback::default();
        let sink = got.clone();
        feedback.connect(move |r| sink.lock().unwrap().push(r));
        let mut pads = Gamepads::new(feedback);
        pads.handle(&InputEvent::GamepadAttach { pad: 0, kind: Default::default() });
        assert!(!pads.unavailable, "couldn't create a device through /dev/uinput");

        // Find it the way a game would.
        let mut found = None;
        for _ in 0..40 {
            found = evdev::enumerate().find(|(_, d)| d.name() == Some("LinuxPlay Gamepad 1"));
            if found.is_some() {
                break;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let (_, mut game) = found.expect("the kernel made no device for the pad");
        assert!(game.supported_ff().is_some_and(|ff| ff.contains(FFEffectCode::FF_RUMBLE)));

        let mut effect = game
            .upload_ff_effect(FFEffectData {
                direction: 0,
                trigger: FFTrigger::default(),
                replay: FFReplay { length: 300, delay: 0 },
                kind: FFEffectKind::Rumble { strong_magnitude: 40_000, weak_magnitude: 10_000 },
            })
            .expect("uploading a rumble effect");
        effect.play(1).unwrap();
        let wait = |count: usize| {
            for _ in 0..100 {
                if got.lock().unwrap().len() >= count {
                    return;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        };
        wait(1);
        assert_eq!(got.lock().unwrap().first(), Some(&Rumble { pad: 0, low: 40_000, high: 10_000, duration_ms: 300 }));
        effect.stop().unwrap();
        wait(2);
        assert_eq!(got.lock().unwrap().get(1).map(|r| r.duration_ms), Some(0));

        drop(effect);
        pads.handle(&InputEvent::GamepadDetach { pad: 0 });
        assert!(pads.pads[0].is_none());
    }
}
