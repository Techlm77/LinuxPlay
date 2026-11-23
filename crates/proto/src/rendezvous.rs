//! The rendezvous server's API: what a device says to sign in, find the
//! account's other devices, and reach one of them or an invite's host.
//!
//! The server is a directory and a pipe. It knows who is signed in and passes
//! bytes between two devices that asked to be connected; inside that pipe
//! the devices run the same mutually authenticated TLS session they would
//! run directly, so the server can neither read nor alter what they say, and
//! being told "this is your other device" by the server never makes a host
//! trust anyone: that still takes pairing or approval on the host itself.
//!
//! Requests and replies are JSON over HTTPS under [`API_PREFIX`]; signed-in
//! requests carry `Authorization: Bearer <token>`.

use serde::{Deserialize, Serialize};

/// TCP port the account server listens on unless told otherwise.
pub const DEFAULT_PORT: u16 = 47900;

/// Path prefix of every endpoint.
pub const API_PREFIX: &str = "/v1";

pub const USERNAME_MIN: usize = 3;
pub const USERNAME_MAX: usize = 32;
pub const PASSWORD_MIN: usize = 10;
pub const PASSWORD_MAX: usize = 256;
pub const DEVICE_NAME_MAX: usize = 64;

/// How a device describes itself when signing in.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct DeviceInfo {
    /// Shown in the account's device list.
    pub name: String,
    /// Hex SHA-256 of the device's certificate. The server only passes this
    /// on; hosts check the certificate a peer actually presents.
    pub fingerprint: String,
}

/// Body of `POST /accounts` (create an account) and `POST /sessions` (sign in).
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct Credentials {
    pub username: String,
    pub password: String,
    pub device: DeviceInfo,
}

/// A signed-in device.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Session {
    /// Secret proving this sign-in; sent as the bearer token.
    pub token: String,
    /// This device's id within the account.
    pub device_id: String,
    pub username: String,
}

/// One of the account's devices, from `GET /devices`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Device {
    pub id: String,
    pub name: String,
    pub fingerprint: String,
    /// It is running as a host and can be connected to right now.
    pub online: bool,
    /// Unix seconds.
    pub last_seen: u64,
    /// This is the device asking.
    pub current: bool,
}

/// Body of `POST /invites`, sent by a host.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewInvite {
    /// How long the invite can be used, in seconds.
    pub valid_for_secs: u64,
}

/// An invite as the server knows it: a name for reaching one host. The
/// secret that lets a guest in is never sent to the server; the guest proves
/// it to the host directly.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Invite {
    pub id: String,
    /// Unix seconds.
    pub expires_at: u64,
}

/// What a device needs to know about the server, from `GET /info`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct ServerInfo {
    /// UDP port of the server's STUN service, where devices learn their
    /// public address; `None` if it runs none.
    pub stun_port: Option<u16>,
    /// Set when the server can be reached from the internet, saying what to
    /// do about it. Devices show it to their user.
    #[serde(default)]
    pub warning: Option<String>,
}

/// Body of every failed request.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// Stable, machine-readable: `bad_request`, `unauthorized`, `not_found`,
    /// `conflict`, `offline`, `rate_limited` or `internal`.
    pub error: String,
    /// For people.
    pub message: String,
}

/// What the server tells a host on its presence connection (`GET /host`).
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HostNotice {
    /// Someone wants to connect: accept the tunnel to talk to them.
    Connect { tunnel: String, from: Caller },
}

/// Who asked for a tunnel, as far as the server can tell.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Caller {
    /// A device signed in to the same account.
    Device { id: String, name: String, fingerprint: String },
    /// Somebody holding this invite's link.
    Invite { id: String },
}

/// Whom a tunnel should reach.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TunnelTarget {
    /// One of the caller's own devices, by id (needs to be signed in).
    Device(String),
    /// The host behind an invite, by the invite's id.
    Invite(String),
}

impl TunnelTarget {
    /// The query string of `GET /tunnel` for this target. Ids are made of
    /// URL-safe characters, so they go in as they are.
    pub fn query(&self) -> String {
        match self {
            Self::Device(id) => format!("device={id}"),
            Self::Invite(id) => format!("invite={id}"),
        }
    }
}

/// What a host hands a guest: where to find the host, and the secret that
/// lets the guest in.
///
/// As text it is `<server>/join#<id>.<secret>`. Everything after the `#` is
/// a URL fragment, which browsers and apps never send to a server; the guest
/// proves the secret to the host itself, with the same exchange that checks
/// a pairing PIN, so the server cannot use or learn it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InviteLink {
    /// The rendezvous server's address, e.g. `https://play.example.org`.
    pub server: String,
    pub id: String,
    pub secret: String,
}

const INVITE_PATH: &str = "/join#";

impl InviteLink {
    /// Read a link as a host printed it; `None` if it isn't one.
    pub fn parse(text: &str) -> Option<Self> {
        let (server, rest) = text.trim().split_once(INVITE_PATH)?;
        let (id, secret) = rest.split_once('.')?;
        let token = |s: &str| !s.is_empty() && s.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_');
        let scheme_ok = server.starts_with("https://") || server.starts_with("http://");
        (scheme_ok && token(id) && token(secret)).then(|| Self {
            server: server.trim_end_matches('/').to_string(),
            id: id.to_string(),
            secret: secret.to_string(),
        })
    }
}

impl std::fmt::Display for InviteLink {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}{INVITE_PATH}{}.{}", self.server.trim_end_matches('/'), self.id, self.secret)
    }
}

/// Is this an acceptable username: letters, digits, `.`, `_` and `-`?
pub fn valid_username(name: &str) -> bool {
    (USERNAME_MIN..=USERNAME_MAX).contains(&name.len())
        && name.bytes().all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'))
}

/// Is this a certificate fingerprint as devices report it: 64 hex digits?
pub fn valid_fingerprint(fingerprint: &str) -> bool {
    fingerprint.len() == 64 && fingerprint.bytes().all(|b| b.is_ascii_hexdigit())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn usernames_are_checked() {
        assert!(valid_username("dominic") && valid_username("a.b_c-9"));
        assert!(!valid_username("ab") && !valid_username("has space") && !valid_username("émile"));
        assert!(!valid_username(&"x".repeat(USERNAME_MAX + 1)));
    }

    #[test]
    fn invite_links_round_trip_and_junk_is_refused() {
        let link = InviteLink { server: "https://play.example.org".into(), id: "abc-123".into(), secret: "s3cr3t_x".into() };
        let text = link.to_string();
        assert_eq!(text, "https://play.example.org/join#abc-123.s3cr3t_x");
        assert_eq!(InviteLink::parse(&format!("  {text}\n")), Some(link.clone()));
        assert_eq!(InviteLink::parse("https://play.example.org//join#abc-123.s3cr3t_x"), Some(link));
        for bad in ["", "192.168.1.5", "https://x/join#noseparator", "https://x/join#.secret", "ftp://x/join#a.b", "https://x/join#a.b c"] {
            assert_eq!(InviteLink::parse(bad), None, "{bad}");
        }
    }

    #[test]
    fn notices_have_a_stable_shape() {
        let notice = HostNotice::Connect { tunnel: "t1".into(), from: Caller::Invite { id: "i1".into() } };
        let json = serde_json::to_string(&notice).unwrap();
        assert_eq!(json, r#"{"type":"connect","tunnel":"t1","from":{"kind":"invite","id":"i1"}}"#);
        assert_eq!(serde_json::from_str::<HostNotice>(&json).unwrap(), notice);
    }
}
