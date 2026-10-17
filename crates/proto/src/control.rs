use serde::{Deserialize, Serialize};

/// Session control messages on [`crate::CONTROL_CHANNEL`], in either direction.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum ControlMsg {
    /// Host: the captured surface changed size.
    VideoSize { width: u32, height: u32 },
    /// Either side: new clipboard text.
    Clipboard { text: String },
    /// Client: round-trip probe; the host echoes `t` back in a `Pong`.
    Ping { t: u64 },
    Pong { t: u64 },
    /// Host: what the encoder is doing, for the client's stats overlay.
    Stats { bitrate_kbps: u32, fps: u32, encoder: String },
}
