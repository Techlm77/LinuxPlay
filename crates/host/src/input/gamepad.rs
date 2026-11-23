//! Virtual gamepads: one kernel input device for every gamepad the client
//! has plugged in, so games on this computer see a controller of their own.
//!
//! The devices are made through `/dev/uinput`. They present the layout of an
//! Xbox 360 pad as Linux's own driver for it reports it, which is the layout
//! every game and mapping database knows.

use evdev::uinput::VirtualDevice;
use evdev::{AbsInfo, AbsoluteAxisCode, AttributeSet, BusType, EventType, InputId, KeyCode, UinputAbsSetup};
use linuxplay_proto::input::{GamepadButton, GamepadState, InputEvent, MAX_GAMEPADS};
use tracing::{info, warn};

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
        .build()
}

struct Pad {
    device: VirtualDevice,
    state: GamepadState,
}

/// The virtual gamepads of one session. Dropping it removes them all.
#[derive(Default)]
pub struct Gamepads {
    pads: [Option<Pad>; MAX_GAMEPADS as usize],
    /// Creating a device failed once; don't retry (or warn) for every event.
    unavailable: bool,
}

impl Gamepads {
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
        match create(pad) {
            Ok(device) => {
                info!("gamepad {} connected", pad + 1);
                *slot = Some(Pad { device, state: GamepadState { pad, ..Default::default() } });
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
        let Some(Some(pad)) = self.slot(state.pad) else { return };
        let events: Vec<evdev::InputEvent> = changes(&pad.state, &state)
            .into_iter()
            .map(|(kind, code, value)| evdev::InputEvent::new(kind.0, code, value))
            .collect();
        pad.state = state;
        if events.is_empty() {
            return;
        }
        if let Err(e) = pad.device.emit(&events) {
            warn!("gamepad {}: {e}", state.pad + 1);
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

    /// Creates a real device, so it needs access to /dev/uinput:
    /// `cargo test -p linuxplay-host -- --ignored a_virtual_gamepad`.
    #[test]
    #[ignore]
    fn a_virtual_gamepad_appears_and_takes_input() {
        let mut pads = Gamepads::default();
        pads.handle(&InputEvent::GamepadAttach { pad: 0, kind: Default::default() });
        assert!(!pads.unavailable, "couldn't create a device through /dev/uinput");
        let device = &mut pads.pads[0].as_mut().unwrap().device;
        let node = device.enumerate_dev_nodes_blocking().unwrap().flatten().next();
        assert!(node.is_some(), "the kernel made no device node");
        pads.handle(&InputEvent::Gamepad(state(|s| s.set(GamepadButton::South, true))));
        pads.handle(&InputEvent::GamepadDetach { pad: 0 });
        assert!(pads.pads[0].is_none());
    }
}
