//! OpenGL: the context shared with GStreamer, and drawing the picture and
//! the text over it.

mod bridge;
mod renderer;
mod text;

pub use bridge::{GlApi, GlBridge, wayland_display};
pub use renderer::{Renderer, Rgba};
pub use text::{Font, FontFile, TextStyle};
