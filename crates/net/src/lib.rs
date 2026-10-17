//! Networking shared by the LinuxPlay host and client: the WebRTC media
//! session, TLS identities, PIN pairing and the signalling WebSocket.

pub mod auth;
pub mod fsutil;
pub mod pair;
pub mod rtc;
pub mod tls;
pub mod ws;

pub use rtc::{Channel, Drops, SessionEvent, SessionHandle};

/// Install the process-wide crypto providers. Call once at startup.
pub fn init_crypto() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
    str0m::crypto::from_feature_flags().install_process_default();
}
