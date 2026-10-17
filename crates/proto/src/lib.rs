//! Wire protocol shared by the LinuxPlay host and client.
//!
//! There are two layers:
//! - [`signal`]: JSON messages on the TLS WebSocket used to authenticate and
//!   to exchange the WebRTC offer/answer.
//! - [`input`] and [`control`]: compact postcard messages on the WebRTC data
//!   channels once the session is up.

pub mod control;
pub mod input;
pub mod signal;

/// Bumped whenever a message changes incompatibly.
pub const PROTOCOL_VERSION: u32 = 3;

/// TCP port the host listens on for signalling.
pub const DEFAULT_PORT: u16 = 47800;
/// UDP ports the host uses for media by default, one per stream, so a
/// firewall or tailnet policy can allow exactly these.
pub const DEFAULT_MEDIA_PORTS: (u16, u16) = (47801, 47816);

/// Largest input event the host accepts on the data channel, in bytes.
pub const MAX_INPUT_MESSAGE: usize = 64;
/// Largest control message either side accepts, in bytes.
pub const MAX_CONTROL_MESSAGE: usize = 64 * 1024;

/// Data channel carrying [`input::InputEvent`]s (client to host).
pub const INPUT_CHANNEL: &str = "input";
/// Data channel carrying [`control::ControlMsg`]s (both ways).
pub const CONTROL_CHANNEL: &str = "control";

#[derive(Debug)]
pub struct DecodeError(postcard::Error);

impl std::fmt::Display for DecodeError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "bad message: {}", self.0)
    }
}

impl std::error::Error for DecodeError {}

/// Encode a data channel message.
pub fn encode<T: serde::Serialize>(msg: &T) -> Vec<u8> {
    // Serialising our own enums into a Vec cannot fail.
    postcard::to_stdvec(msg).expect("postcard encode")
}

/// Decode a data channel message.
pub fn decode<'a, T: serde::Deserialize<'a>>(bytes: &'a [u8]) -> Result<T, DecodeError> {
    postcard::from_bytes(bytes).map_err(DecodeError)
}

#[cfg(test)]
mod tests {
    use crate::control::ControlMsg;
    use crate::input::InputEvent;
    use crate::signal::{ClientMsg, HostMsg};

    /// A data-channel peer is untrusted: decoding attacker-controlled bytes
    /// must never panic, only return `Err`. Walk a pseudo-random sequence of
    /// buffers of every length through both postcard decoders.
    #[test]
    fn decoding_hostile_bytes_never_panics() {
        let mut state: u64 = 0x9e3779b97f4a7c15;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for len in 0..512usize {
            for _ in 0..8 {
                let buf: Vec<u8> = (0..len).map(|_| next() as u8).collect();
                let _ = super::decode::<InputEvent>(&buf);
                let _ = super::decode::<ControlMsg>(&buf);
            }
        }
    }

    /// Every real message must survive a round trip.
    #[test]
    fn roundtrip_data_channel_messages() {
        let events = [
            InputEvent::PointerAbs { x: 1, y: 2 },
            InputEvent::Scroll { dx: -3, dy: 120 },
            InputEvent::ReleaseAll,
        ];
        for e in events {
            assert_eq!(super::decode::<InputEvent>(&super::encode(&e)).unwrap(), e);
        }
        let msg = ControlMsg::Stats { bitrate_kbps: 9000, fps: 60, encoder: "vah264enc".into() };
        assert_eq!(super::decode::<ControlMsg>(&super::encode(&msg)).unwrap(), msg);
    }

    /// Signalling JSON comes from an untrusted peer too: mutated and truncated
    /// copies of every real message must parse or fail, never panic.
    #[test]
    fn mutated_signalling_never_panics() {
        let samples = [
            r#"{"type":"hello","version":3,"name":"pc","codecs":["h264"],"hw_codecs":[],"max_fps":60,"audio":true,"pair":true}"#,
            r#"{"type":"pair_start","spake":"QUJD"}"#,
            r#"{"type":"pair_finish","mac":"00ff"}"#,
            r#"{"type":"select_monitor","index":4294967295}"#,
            r#"{"type":"answer","sdp":"v=0"}"#,
            r#"{"type":"approved","control":false,"audio":true}"#,
            r#"{"type":"monitors","monitors":[{"index":0,"name":"A","width":1,"height":1,"x":-5,"y":0,"primary":true}]}"#,
            r#"{"type":"offer","sdp":"v=0","codec":"av1","width":1,"height":1}"#,
        ];
        let mut state: u64 = 0x2545f4914f6cdd1d;
        let mut next = || {
            state ^= state << 13;
            state ^= state >> 7;
            state ^= state << 17;
            state
        };
        for s in samples {
            for _ in 0..2000 {
                let mut b = s.as_bytes().to_vec();
                for _ in 0..(next() % 4 + 1) {
                    let i = (next() as usize) % b.len();
                    match next() % 3 {
                        0 => b[i] = next() as u8,
                        1 => b.truncate(i),
                        _ => b.insert(i, next() as u8),
                    }
                    if b.is_empty() {
                        break;
                    }
                }
                let text = String::from_utf8_lossy(&b);
                let _ = serde_json::from_str::<ClientMsg>(&text);
                let _ = serde_json::from_str::<HostMsg>(&text);
            }
        }
    }

    /// Signalling is JSON; malformed or truncated text must be an error, and
    /// an unknown message tag must be rejected (no catch-all variant runs).
    #[test]
    fn signalling_rejects_garbage_json() {
        assert!(serde_json::from_str::<ClientMsg>("").is_err());
        assert!(serde_json::from_str::<ClientMsg>("{").is_err());
        assert!(serde_json::from_str::<ClientMsg>(r#"{"type":"not_a_real_message"}"#).is_err());
        assert!(serde_json::from_str::<HostMsg>(r#"{"type":"offer"}"#).is_err()); // missing fields
        // A valid one still parses.
        let hello = r#"{"type":"answer","sdp":"v=0"}"#;
        assert!(matches!(serde_json::from_str::<ClientMsg>(hello), Ok(ClientMsg::Answer { .. })));
    }
}
