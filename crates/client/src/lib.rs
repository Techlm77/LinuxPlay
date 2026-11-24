//! The LinuxPlay client: connects to a host and shows its screen in a window
//! that sends keyboard, mouse and gamepad input back.

mod audio;
pub mod connect;
mod decode;
mod gl;
mod input;
pub mod known_hosts;
mod latency;
mod layout;
pub mod tailnet;
pub mod viewer;

use anyhow::{Context as _, Result};

pub use viewer::{ViewerOptions, run as run_viewer};

/// Start the libraries the client is built on. Call once, before
/// [`run_viewer`].
pub fn init() -> Result<()> {
    gstreamer::init().context("GStreamer failed to start")?;
    linuxplay_net::init_crypto();
    Ok(())
}
