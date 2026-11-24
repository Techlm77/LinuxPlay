//! Translating the keys SDL reports into what the host injects.
//!
//! SDL names keys by their position (a "scancode", following the USB HID
//! tables); the host wants Linux evdev codes (`KEY_*`). Both describe physical
//! keys, so the host's own keyboard layout decides what each one types.

use sdl3::keyboard::Scancode;

/// The Linux evdev code for a physical key, or `None` for keys evdev has no
/// place for.
pub fn evdev(key: Scancode) -> Option<u16> {
    Some(match key {
        Scancode::Escape => 1,
        Scancode::_1 => 2,
        Scancode::_2 => 3,
        Scancode::_3 => 4,
        Scancode::_4 => 5,
        Scancode::_5 => 6,
        Scancode::_6 => 7,
        Scancode::_7 => 8,
        Scancode::_8 => 9,
        Scancode::_9 => 10,
        Scancode::_0 => 11,
        Scancode::Minus => 12,
        Scancode::Equals => 13,
        Scancode::Backspace => 14,
        Scancode::Tab => 15,
        Scancode::Q => 16,
        Scancode::W => 17,
        Scancode::E => 18,
        Scancode::R => 19,
        Scancode::T => 20,
        Scancode::Y => 21,
        Scancode::U => 22,
        Scancode::I => 23,
        Scancode::O => 24,
        Scancode::P => 25,
        Scancode::LeftBracket => 26,
        Scancode::RightBracket => 27,
        Scancode::Return => 28,
        Scancode::LCtrl => 29,
        Scancode::A => 30,
        Scancode::S => 31,
        Scancode::D => 32,
        Scancode::F => 33,
        Scancode::G => 34,
        Scancode::H => 35,
        Scancode::J => 36,
        Scancode::K => 37,
        Scancode::L => 38,
        Scancode::Semicolon => 39,
        Scancode::Apostrophe => 40,
        Scancode::Grave => 41,
        Scancode::LShift => 42,
        // The ISO key next to Return sends the same code as the ANSI backslash.
        Scancode::Backslash | Scancode::NonUsHash => 43,
        Scancode::Z => 44,
        Scancode::X => 45,
        Scancode::C => 46,
        Scancode::V => 47,
        Scancode::B => 48,
        Scancode::N => 49,
        Scancode::M => 50,
        Scancode::Comma => 51,
        Scancode::Period => 52,
        Scancode::Slash => 53,
        Scancode::RShift => 54,
        Scancode::KpMultiply => 55,
        Scancode::LAlt => 56,
        Scancode::Space => 57,
        Scancode::CapsLock => 58,
        Scancode::F1 => 59,
        Scancode::F2 => 60,
        Scancode::F3 => 61,
        Scancode::F4 => 62,
        Scancode::F5 => 63,
        Scancode::F6 => 64,
        Scancode::F7 => 65,
        Scancode::F8 => 66,
        Scancode::F9 => 67,
        Scancode::F10 => 68,
        Scancode::NumLockClear => 69,
        Scancode::ScrollLock => 70,
        Scancode::Kp7 => 71,
        Scancode::Kp8 => 72,
        Scancode::Kp9 => 73,
        Scancode::KpMinus => 74,
        Scancode::Kp4 => 75,
        Scancode::Kp5 => 76,
        Scancode::Kp6 => 77,
        Scancode::KpPlus => 78,
        Scancode::Kp1 => 79,
        Scancode::Kp2 => 80,
        Scancode::Kp3 => 81,
        Scancode::Kp0 => 82,
        Scancode::KpPeriod => 83,
        Scancode::Lang5 => 85,
        Scancode::NonUsBackslash => 86,
        Scancode::F11 => 87,
        Scancode::F12 => 88,
        Scancode::International1 => 89,
        Scancode::Lang3 => 90,
        Scancode::Lang4 => 91,
        Scancode::International4 => 92,
        Scancode::International2 => 93,
        Scancode::International5 => 94,
        Scancode::International6 => 95,
        Scancode::KpEnter => 96,
        Scancode::RCtrl => 97,
        Scancode::KpDivide => 98,
        Scancode::PrintScreen => 99,
        Scancode::RAlt => 100,
        Scancode::Home => 102,
        Scancode::Up => 103,
        Scancode::PageUp => 104,
        Scancode::Left => 105,
        Scancode::Right => 106,
        Scancode::End => 107,
        Scancode::Down => 108,
        Scancode::PageDown => 109,
        Scancode::Insert => 110,
        Scancode::Delete => 111,
        Scancode::Mute => 113,
        Scancode::VolumeDown => 114,
        Scancode::VolumeUp => 115,
        Scancode::Power => 116,
        Scancode::KpEquals => 117,
        Scancode::Pause => 119,
        Scancode::KpComma => 121,
        Scancode::Lang1 => 122,
        Scancode::Lang2 => 123,
        Scancode::International3 => 124,
        Scancode::LGui => 125,
        Scancode::RGui => 126,
        Scancode::Application => 127,
        Scancode::Stop | Scancode::AcStop => 128,
        Scancode::Again => 129,
        Scancode::Undo => 131,
        Scancode::Copy => 133,
        Scancode::Paste => 135,
        Scancode::Find => 136,
        Scancode::Cut => 137,
        Scancode::Help => 138,
        Scancode::Menu => 139,
        Scancode::Sleep => 142,
        Scancode::Wake => 143,
        Scancode::AcBookmarks => 156,
        Scancode::AcBack => 158,
        Scancode::AcForward => 159,
        Scancode::MediaEject => 161,
        Scancode::MediaNextTrack => 163,
        Scancode::MediaPlayPause => 164,
        Scancode::MediaPreviousTrack => 165,
        Scancode::MediaStop => 166,
        Scancode::MediaRecord => 167,
        Scancode::MediaRewind => 168,
        Scancode::AcHome => 172,
        Scancode::AcRefresh => 173,
        Scancode::F13 => 183,
        Scancode::F14 => 184,
        Scancode::F15 => 185,
        Scancode::F16 => 186,
        Scancode::F17 => 187,
        Scancode::F18 => 188,
        Scancode::F19 => 189,
        Scancode::F20 => 190,
        Scancode::F21 => 191,
        Scancode::F22 => 192,
        Scancode::F23 => 193,
        Scancode::F24 => 194,
        Scancode::MediaPause => 201,
        Scancode::MediaPlay => 207,
        Scancode::MediaFastForward => 208,
        Scancode::AcSearch => 217,
        Scancode::MediaSelect => 226,
        _ => return None,
    })
}

#[cfg(test)]
mod tests {
    use linuxplay_proto::input::KEY_MAX;

    use super::*;

    #[test]
    fn common_keys_map_to_their_evdev_codes() {
        assert_eq!(evdev(Scancode::A), Some(30));
        assert_eq!(evdev(Scancode::Return), Some(28));
        assert_eq!(evdev(Scancode::LGui), Some(125));
        assert_eq!(evdev(Scancode::F12), Some(88));
        assert_eq!(evdev(Scancode::Kp5), Some(76));
        assert_eq!(evdev(Scancode::Unknown), None);
    }

    /// Every code the table can produce must be one the host accepts.
    #[test]
    fn every_mapped_code_is_a_valid_key() {
        for raw in 0..512 {
            if let Some(code) = Scancode::from_i32(raw).and_then(evdev) {
                assert!(code > 0 && code <= KEY_MAX, "scancode {raw} maps to {code}");
            }
        }
    }
}
