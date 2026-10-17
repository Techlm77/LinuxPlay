use serde::{Deserialize, Serialize};

/// Largest value of a normalised pointer coordinate.
pub const POINTER_MAX: u16 = u16::MAX;

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
}
