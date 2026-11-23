//! Turning what happens in the window into input for the host.

pub mod gamepad;
pub mod keymap;

use linuxplay_proto::input::{InputEvent, MouseButton};
use sdl3::event::Event;
use sdl3::mouse::{MouseButton as SdlButton, MouseWheelDirection};

use crate::layout::Rect;

/// How mouse movement is sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MouseMode {
    /// The pointer goes where it is over the picture: for desktop use.
    Absolute,
    /// The pointer is locked to the window and raw movement is sent: for
    /// games that steer a camera with the mouse.
    Relative,
}

/// One notch of a mouse wheel, in the units the host expects.
const WHEEL_NOTCH: f32 = 120.0;

/// Translates keyboard and mouse events.
pub struct InputMapper {
    pub mode: MouseMode,
    /// Where the picture is in the window, in window coordinates.
    pub picture: Option<Rect>,
}

impl InputMapper {
    pub fn new() -> Self {
        Self { mode: MouseMode::Absolute, picture: None }
    }

    /// The input event for a window event, if it is one the host should get.
    pub fn translate(&self, event: &Event) -> Option<InputEvent> {
        match *event {
            // The host repeats held keys itself.
            Event::KeyDown { scancode, repeat: false, .. } => {
                Some(InputEvent::Key { code: keymap::evdev(scancode?)?, pressed: true })
            }
            Event::KeyUp { scancode, .. } => Some(InputEvent::Key { code: keymap::evdev(scancode?)?, pressed: false }),
            Event::MouseMotion { x, y, xrel, yrel, .. } => match self.mode {
                MouseMode::Relative => Some(InputEvent::PointerRel { dx: xrel, dy: yrel }),
                MouseMode::Absolute => {
                    let (x, y) = self.picture?.to_remote(x, y)?;
                    Some(InputEvent::PointerAbs { x, y })
                }
            },
            Event::MouseButtonDown { mouse_btn, .. } => {
                Some(InputEvent::Button { button: map_button(mouse_btn)?, pressed: true })
            }
            Event::MouseButtonUp { mouse_btn, .. } => {
                Some(InputEvent::Button { button: map_button(mouse_btn)?, pressed: false })
            }
            Event::MouseWheel { x, y, direction, .. } => {
                let flip = if direction == MouseWheelDirection::Flipped { -1.0 } else { 1.0 };
                // SDL counts scrolling away from the user as positive; the
                // host counts scrolling down as positive.
                let dx = (x * flip * WHEEL_NOTCH).round() as i32;
                let dy = (-y * flip * WHEEL_NOTCH).round() as i32;
                (dx != 0 || dy != 0).then_some(InputEvent::Scroll { dx, dy })
            }
            _ => None,
        }
    }
}

impl Default for InputMapper {
    fn default() -> Self {
        Self::new()
    }
}

fn map_button(button: SdlButton) -> Option<MouseButton> {
    Some(match button {
        SdlButton::Left => MouseButton::Left,
        SdlButton::Middle => MouseButton::Middle,
        SdlButton::Right => MouseButton::Right,
        SdlButton::X1 => MouseButton::Back,
        SdlButton::X2 => MouseButton::Forward,
        SdlButton::Unknown => return None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn mouse_buttons_map_including_the_side_buttons() {
        assert_eq!(map_button(SdlButton::Left), Some(MouseButton::Left));
        assert_eq!(map_button(SdlButton::X1), Some(MouseButton::Back));
        assert_eq!(map_button(SdlButton::X2), Some(MouseButton::Forward));
        assert_eq!(map_button(SdlButton::Unknown), None);
    }
}
