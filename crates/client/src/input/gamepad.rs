//! Gamepads plugged into this computer, forwarded to the host.
//!
//! SDL recognises the controller and normalises its layout; each one is given
//! a slot, announced to the host (which creates a matching virtual gamepad),
//! and from then on every change sends that pad's complete state.

use std::collections::HashMap;
use std::path::Path;

use linuxplay_proto::control::Rumble;
use linuxplay_proto::input::{GamepadButton, GamepadKind, GamepadState, InputEvent, MAX_GAMEPADS};
use sdl3::GamepadSubsystem;
use sdl3::event::Event;
use sdl3::gamepad::{Axis, Button, Gamepad, GamepadType};
use sdl3::joystick::JoystickId;
use tracing::{info, warn};

/// How a host names the virtual gamepads it creates. When host and client
/// are the same computer those must not be forwarded back to the host, which
/// would answer each one with another.
const HOST_GAMEPAD_NAME: &str = "LinuxPlay Gamepad";

/// The longest a single rumble effect may play, so a lost "stop" can't leave
/// a controller buzzing.
const MAX_RUMBLE_MS: u32 = 5_000;

struct Pad {
    /// Keeps the device open; also used for rumble.
    device: Gamepad,
    kind: GamepadKind,
    state: GamepadState,
}

pub struct Gamepads {
    subsystem: GamepadSubsystem,
    pads: HashMap<JoystickId, Pad>,
}

impl Gamepads {
    pub fn new(subsystem: GamepadSubsystem) -> Self {
        Self { subsystem, pads: HashMap::new() }
    }

    /// Everything the host needs to know about the pads already connected,
    /// for when the input channel opens after they were plugged in.
    pub fn announce(&self) -> Vec<InputEvent> {
        let mut pads: Vec<&Pad> = self.pads.values().collect();
        pads.sort_by_key(|p| p.state.pad);
        pads.into_iter()
            .flat_map(|p| [InputEvent::GamepadAttach { pad: p.state.pad, kind: p.kind }, InputEvent::Gamepad(p.state)])
            .collect()
    }

    /// Turn a gamepad event into what the host should be told, if anything.
    pub fn handle(&mut self, event: &Event) -> Option<InputEvent> {
        match *event {
            Event::GamepadAdded { which, .. } => self.attach(which),
            Event::GamepadRemoved { which, .. } => {
                let pad = self.pads.remove(&which)?;
                info!("gamepad {} unplugged", pad.state.pad + 1);
                Some(InputEvent::GamepadDetach { pad: pad.state.pad })
            }
            Event::GamepadButtonDown { which, button, .. } => self.button(which, button, true),
            Event::GamepadButtonUp { which, button, .. } => self.button(which, button, false),
            Event::GamepadAxisMotion { which, axis, value, .. } => {
                let pad = self.pads.get_mut(&which)?;
                let before = pad.state;
                set_axis(&mut pad.state, axis, value);
                (pad.state != before).then_some(InputEvent::Gamepad(pad.state))
            }
            _ => None,
        }
    }

    /// Play a rumble effect the host asked for.
    pub fn rumble(&mut self, effect: Rumble) {
        let Some(pad) = self.pads.values_mut().find(|p| p.state.pad == effect.pad) else { return };
        // Not every controller has motors; that is not worth reporting.
        let _ = pad.device.set_rumble(effect.low, effect.high, (effect.duration_ms as u32).min(MAX_RUMBLE_MS));
    }

    fn attach(&mut self, id: JoystickId) -> Option<InputEvent> {
        if self.pads.contains_key(&id) {
            return None;
        }
        let Some(slot) = (0..MAX_GAMEPADS).find(|s| self.pads.values().all(|p| p.state.pad != *s)) else {
            warn!("ignoring a gamepad: {MAX_GAMEPADS} are already in use");
            return None;
        };
        if self.is_host_gamepad(id) {
            return None;
        }
        let device = self.subsystem.open(id).inspect_err(|e| warn!("couldn't open a gamepad: {e}")).ok()?;
        let kind = kind_of(device.r#type());
        info!("gamepad {}: {}", slot + 1, device.name().unwrap_or_else(|| "unnamed".into()));
        self.pads.insert(id, Pad { device, kind, state: GamepadState { pad: slot, ..Default::default() } });
        Some(InputEvent::GamepadAttach { pad: slot, kind })
    }

    /// Is this one of the virtual gamepads a host on this computer created?
    /// SDL shows those under a name from its own database, so ask the kernel
    /// what the device calls itself.
    fn is_host_gamepad(&self, id: JoystickId) -> bool {
        let Ok(node) = self.subsystem.path_for_id(id) else { return false };
        let Some(event) = Path::new(&node).file_name() else { return false };
        let name = std::fs::read_to_string(Path::new("/sys/class/input").join(event).join("device/name"));
        name.is_ok_and(|name| name.starts_with(HOST_GAMEPAD_NAME))
    }

    fn button(&mut self, id: JoystickId, button: Button, pressed: bool) -> Option<InputEvent> {
        let pad = self.pads.get_mut(&id)?;
        let before = pad.state;
        pad.state.set(map_button(button)?, pressed);
        (pad.state != before).then_some(InputEvent::Gamepad(pad.state))
    }
}

fn kind_of(kind: GamepadType) -> GamepadKind {
    match kind {
        GamepadType::PS3 | GamepadType::PS4 | GamepadType::PS5 => GamepadKind::PlayStation,
        GamepadType::NintendoSwitchPro
        | GamepadType::NintendoSwitchJoyconLeft
        | GamepadType::NintendoSwitchJoyconRight
        | GamepadType::NintendoSwitchJoyconPair => GamepadKind::Nintendo,
        _ => GamepadKind::Xbox,
    }
}

fn map_button(button: Button) -> Option<GamepadButton> {
    Some(match button {
        Button::South => GamepadButton::South,
        Button::East => GamepadButton::East,
        Button::West => GamepadButton::West,
        Button::North => GamepadButton::North,
        Button::Back => GamepadButton::Back,
        Button::Guide => GamepadButton::Guide,
        Button::Start => GamepadButton::Start,
        Button::LeftStick => GamepadButton::LeftStick,
        Button::RightStick => GamepadButton::RightStick,
        Button::LeftShoulder => GamepadButton::LeftShoulder,
        Button::RightShoulder => GamepadButton::RightShoulder,
        Button::DPadUp => GamepadButton::DpadUp,
        Button::DPadDown => GamepadButton::DpadDown,
        Button::DPadLeft => GamepadButton::DpadLeft,
        Button::DPadRight => GamepadButton::DpadRight,
        // Paddles, touchpads and share buttons have no place on the virtual pad.
        _ => return None,
    })
}

fn set_axis(state: &mut GamepadState, axis: Axis, value: i16) {
    match axis {
        Axis::LeftX => state.left_x = value,
        Axis::LeftY => state.left_y = value,
        Axis::RightX => state.right_x = value,
        Axis::RightY => state.right_y = value,
        Axis::TriggerLeft => state.left_trigger = trigger(value),
        Axis::TriggerRight => state.right_trigger = trigger(value),
    }
}

/// SDL reports triggers as `0..=i16::MAX`; the wire uses the full `u16` range.
fn trigger(value: i16) -> u16 {
    (value.max(0) as u32 * u16::MAX as u32 / i16::MAX as u32) as u16
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn triggers_span_the_full_range() {
        assert_eq!(trigger(0), 0);
        assert_eq!(trigger(i16::MAX), u16::MAX);
        assert_eq!(trigger(-5), 0);
        assert!((trigger(i16::MAX / 2) as i32 - u16::MAX as i32 / 2).abs() <= 2);
    }

    #[test]
    fn sticks_and_buttons_update_the_state() {
        let mut s = GamepadState::default();
        set_axis(&mut s, Axis::LeftX, -32768);
        set_axis(&mut s, Axis::RightY, 1234);
        s.set(map_button(Button::South).unwrap(), true);
        assert_eq!((s.left_x, s.right_y), (-32768, 1234));
        assert!(s.pressed(GamepadButton::South));
        assert_eq!(map_button(Button::Touchpad), None);
    }

    #[test]
    fn layouts_are_recognised() {
        assert_eq!(kind_of(GamepadType::PS5), GamepadKind::PlayStation);
        assert_eq!(kind_of(GamepadType::NintendoSwitchPro), GamepadKind::Nintendo);
        assert_eq!(kind_of(GamepadType::Xbox360), GamepadKind::Xbox);
        assert_eq!(kind_of(GamepadType::Unknown), GamepadKind::Xbox);
    }
}
