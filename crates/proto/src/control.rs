use serde::{Deserialize, Serialize};

/// Session control messages on [`crate::CONTROL_CHANNEL`], in either direction.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub enum ControlMsg {
    /// Host: the captured surface changed size.
    VideoSize { width: u32, height: u32 },
    /// Either side: new clipboard text.
    Clipboard { text: String },
    /// Client: round-trip and clock probe. `t` is the client's own clock and
    /// comes back unchanged in the [`Pong`](Self::Pong).
    Ping { t: u64 },
    /// Host: answer to a `Ping`, stamped with the host's monotonic clock in
    /// microseconds. From `t`, the arrival time and `host_us` the client
    /// estimates how far the two clocks are apart.
    Pong { t: u64, host_us: u64 },
    /// Host: where the video's RTP clock sits on the host's monotonic clock.
    /// A frame with RTP time `r` was captured at `base_us + r / 90 kHz`, so
    /// together with the clock offset from `Pong` the client can tell how old
    /// every frame is when it reaches the screen.
    VideoClock { base_us: u64 },
    /// Host: what the encoder is doing, for the client's stats overlay.
    Stats(HostStats),
    /// Host: play a rumble effect on one of the client's gamepads.
    Rumble(Rumble),
}

/// A periodic report from the host's video pipeline.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct HostStats {
    /// Encoded bitrate actually produced, kbit/s.
    pub bitrate_kbps: u32,
    /// Frames encoded per second.
    pub fps: u32,
    /// The encoder in use, e.g. `vah264enc`.
    pub encoder: String,
    /// Average time from a frame being captured to it being handed to the
    /// network, in microseconds.
    pub encode_us: u32,
}

/// A rumble effect for the gamepad the client attached as `pad`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rumble {
    pub pad: u8,
    /// Strength of the low-frequency (heavy) motor.
    pub low: u16,
    /// Strength of the high-frequency (light) motor.
    pub high: u16,
    /// How long to play it, in milliseconds; `0` stops the motors.
    pub duration_ms: u16,
}
