use serde::{Deserialize, Serialize};

/// Largest value of a normalised pointer coordinate.
pub const POINTER_MAX: u16 = u16::MAX;

/// Highest Linux evdev key code (`KEY_MAX`); anything above is not a key.
pub const KEY_MAX: u16 = 0x2ff;

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MouseButton {
    Left,
    Middle,
    Right,
    Back,
    Forward,
}

impl MouseButton {
    /// From an X11/GDK button number.
    pub fn from_x11(button: u32) -> Option<Self> {
        Some(match button {
            1 => Self::Left,
            2 => Self::Middle,
            3 => Self::Right,
            8 => Self::Back,
            9 => Self::Forward,
            _ => return None,
        })
    }

    /// The X11/XTest button number.
    pub fn x11(self) -> u8 {
        match self {
            Self::Left => 1,
            Self::Middle => 2,
            Self::Right => 3,
            Self::Back => 8,
            Self::Forward => 9,
        }
    }

    /// The Linux evdev code (`BTN_*`), as used by uinput and the portals.
    pub fn evdev(self) -> u16 {
        match self {
            Self::Left => 0x110,
            Self::Right => 0x111,
            Self::Middle => 0x112,
            Self::Back => 0x116,
            Self::Forward => 0x115,
        }
    }
}

/// Input sent from the client to the host on [`crate::INPUT_CHANNEL`].
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq)]
pub enum InputEvent {
    /// Absolute pointer position over the streamed surface, each axis
    /// normalised to `0..=POINTER_MAX`.
    PointerAbs { x: u16, y: u16 },
    /// Relative pointer motion in host pixels (pointer-locked mode).
    PointerRel { dx: f32, dy: f32 },
    Button { button: MouseButton, pressed: bool },
    /// Scroll in 1/120ths of a wheel notch; positive `dy` scrolls down.
    Scroll { dx: i32, dy: i32 },
    /// Linux evdev key code (`KEY_*`), so the host's own layout applies.
    Key { code: u16, pressed: bool },
    /// Release every key and button the client may have left held, e.g. when
    /// the client window loses focus mid-press.
    ReleaseAll,
    /// A gamepad was plugged in on the client; the host creates a matching
    /// virtual one. `pad` identifies it in later events.
    GamepadAttach { pad: u8, kind: GamepadKind },
    /// The complete state of one gamepad. Sent whenever anything changes, so
    /// a lost or dropped update is repaired by the next one.
    Gamepad(GamepadState),
    GamepadDetach { pad: u8 },
}

/// Most gamepads one client may attach.
pub const MAX_GAMEPADS: u8 = 4;

/// The button layout the host's virtual gamepad should present.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum GamepadKind {
    /// A/B/X/Y pad with two sticks and analogue triggers.
    #[default]
    Xbox,
    /// Cross/Circle/Square/Triangle pad.
    PlayStation,
    /// A/B swapped relative to Xbox.
    Nintendo,
}

/// Every control of a gamepad at one instant.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GamepadState {
    pub pad: u8,
    /// Held buttons, one bit per [`GamepadButton`].
    pub buttons: u32,
    /// Sticks: negative is left/up, as on Linux evdev.
    pub left_x: i16,
    pub left_y: i16,
    pub right_x: i16,
    pub right_y: i16,
    /// Triggers, `0` released to `u16::MAX` fully pressed.
    pub left_trigger: u16,
    pub right_trigger: u16,
}

/// A gamepad button, named by position (as on an Xbox pad) so every layout
/// maps onto the same bits.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum GamepadButton {
    South,
    East,
    West,
    North,
    Back,
    Guide,
    Start,
    LeftStick,
    RightStick,
    LeftShoulder,
    RightShoulder,
    DpadUp,
    DpadDown,
    DpadLeft,
    DpadRight,
}

impl GamepadButton {
    pub const ALL: [Self; 15] = [
        Self::South,
        Self::East,
        Self::West,
        Self::North,
        Self::Back,
        Self::Guide,
        Self::Start,
        Self::LeftStick,
        Self::RightStick,
        Self::LeftShoulder,
        Self::RightShoulder,
        Self::DpadUp,
        Self::DpadDown,
        Self::DpadLeft,
        Self::DpadRight,
    ];

    /// This button's bit in [`GamepadState::buttons`].
    pub const fn bit(self) -> u32 {
        1 << self as u8
    }
}

impl GamepadState {
    pub fn pressed(&self, button: GamepadButton) -> bool {
        self.buttons & button.bit() != 0
    }

    pub fn set(&mut self, button: GamepadButton, pressed: bool) {
        if pressed {
            self.buttons |= button.bit();
        } else {
            self.buttons &= !button.bit();
        }
    }
}
