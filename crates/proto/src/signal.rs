//! Signalling messages, sent as JSON text frames on the TLS WebSocket.
//!
//! Both ends present self-signed certificates and identify each other by the
//! SHA-256 fingerprint of that certificate. A client that is new to the host,
//! or a host the client has not pinned yet, must pair first:
//!
//! 1. client `Hello { pair: true }`
//! 2. host `PairingRequired` (and shows a PIN, or uses its set password)
//! 3. client `PairStart` with its SPAKE2 message
//! 4. host `PairReply` with its SPAKE2 message and a key-confirmation MAC
//! 5. client `PairFinish` with its own MAC
//! 6. host `Welcome`
//!
//! The MACs cover both certificate fingerprints, so a man in the middle, who
//! must present different certificates to each side, cannot complete pairing
//! without knowing the PIN. After `Welcome` the host sends `Offer` and the
//! client replies with `Answer`; the WebRTC DTLS fingerprints in those SDPs are
//! therefore authenticated too.

use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Hash)]
#[serde(rename_all = "lowercase")]
pub enum VideoCodec {
    H264,
    H265,
    Av1,
}

impl VideoCodec {
    pub fn name(self) -> &'static str {
        match self {
            Self::H264 => "H.264",
            Self::H265 => "H.265",
            Self::Av1 => "AV1",
        }
    }
}

/// A display the host can share.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct MonitorInfo {
    /// Stable within one connection; what the client echoes in `SelectMonitor`.
    pub index: u32,
    /// Human name, e.g. `HDMI-0` or `Display 1`.
    pub name: String,
    pub width: u32,
    pub height: u32,
    /// Position in the host's desktop layout.
    pub x: i32,
    pub y: i32,
    pub primary: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ClientMsg {
    Hello {
        version: u32,
        /// Shown on the host when pairing and in its list of trusted clients.
        name: String,
        /// Codecs the client can decode, best first.
        codecs: Vec<VideoCodec>,
        /// The subset of `codecs` the client decodes in hardware.
        #[serde(default)]
        hw_codecs: Vec<VideoCodec>,
        /// The client's monitor refresh rate; the host won't exceed it.
        #[serde(default)]
        max_fps: u32,
        audio: bool,
        /// The client has not pinned this host, so it insists on pairing.
        pair: bool,
    },
    /// SPAKE2 side A message, base64.
    PairStart { spake: String },
    /// HMAC proving the client derived the same key, hex.
    PairFinish { mac: String },
    /// Which display to stream (an `index` from [`HostMsg::Monitors`]).
    SelectMonitor { index: u32 },
    Answer { sdp: String },
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostMsg {
    /// The client must pair; the host is showing a PIN, or expects its
    /// permanent password when `password` is set.
    PairingRequired { host_name: String, password: bool },
    /// SPAKE2 side B message (base64) and the host's key-confirmation MAC (hex).
    PairReply { spake: String, mac: String },
    /// Authenticated; the displays this host can share follow.
    Welcome { host_name: String },
    /// The displays available to stream; the client replies with
    /// [`ClientMsg::SelectMonitor`].
    Monitors { monitors: Vec<MonitorInfo> },
    Offer {
        sdp: String,
        codec: VideoCodec,
        width: u32,
        height: u32,
        /// The framerate the host settled on for this link.
        #[serde(default)]
        fps: u32,
    },
    Error { message: String },
}

/// Label mixed into the pairing MACs; the role keeps the two MACs distinct.
pub fn pairing_mac_input(role: &str, host_fp: &[u8], client_fp: &[u8]) -> Vec<u8> {
    let mut v = Vec::with_capacity(32 + role.len() + host_fp.len() + client_fp.len());
    v.extend_from_slice(b"linuxplay-pair-v1/");
    v.extend_from_slice(role.as_bytes());
    v.push(0);
    v.extend_from_slice(host_fp);
    v.extend_from_slice(client_fp);
    v
}
