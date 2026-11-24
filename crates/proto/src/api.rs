//! The engine's local API: how a front end drives LinuxPlay.
//!
//! The engine (`linuxplay service`) listens on a Unix socket that only the
//! same user can open, [`SOCKET_NAME`] in `$XDG_RUNTIME_DIR/linuxplay/`. The
//! protocol is JSON-RPC 2.0, one message per line:
//!
//! ```text
//! → {"jsonrpc":"2.0","id":1,"method":"host.start","params":{"unattended":false}}
//! ← {"jsonrpc":"2.0","id":1,"result":{…the new State…}}
//! ← {"jsonrpc":"2.0","id":2,"error":{"code":-32000,"message":"Sign in first."}}
//! ```
//!
//! After `subscribe`, the engine also sends a [`method::STATE_CHANGED`]
//! notification with the whole [`State`] whenever anything in it changes: a
//! front end can simply redraw from the latest one.
//!
//! | method | params | result |
//! |---|---|---|
//! | `state` | | [`State`] |
//! | `subscribe` | | [`State`], then notifications |
//! | `account.create`, `account.login` | [`SignIn`] | [`State`] |
//! | `account.logout` | | [`State`] |
//! | `account.devices` | | list of [`rendezvous::Device`](crate::rendezvous::Device) |
//! | `account.remove_device` | [`ById`] (text id) | `null` |
//! | `host.start` | [`StartHosting`] | [`State`] |
//! | `host.stop` | | [`State`] |
//! | `host.decide` | [`Decide`] | `null` |
//! | `host.disconnect` | [`Disconnect`] | number of sessions ended |
//! | `host.invite` | [`NewInvite`] | [`InviteMade`] |
//! | `host.uninvite` | [`ById`] (text id) | `null` |
//! | `host.trusted` | | list of [`TrustedDevice`] |
//! | `host.revoke` | [`Revoke`] | `null` |
//! | `connect` | [`Connect`] | [`Connection`] |
//! | `disconnect` | [`ById`] (number id) | `null` |

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// File name of the API socket inside `$XDG_RUNTIME_DIR/linuxplay/`.
pub const SOCKET_NAME: &str = "api.sock";
/// Largest request line the engine reads, in bytes.
pub const MAX_REQUEST: usize = 64 * 1024;

/// Method names.
pub mod method {
    pub const STATE: &str = "state";
    pub const SUBSCRIBE: &str = "subscribe";
    pub const ACCOUNT_CREATE: &str = "account.create";
    pub const ACCOUNT_LOGIN: &str = "account.login";
    pub const ACCOUNT_LOGOUT: &str = "account.logout";
    pub const ACCOUNT_DEVICES: &str = "account.devices";
    pub const ACCOUNT_REMOVE_DEVICE: &str = "account.remove_device";
    pub const HOST_START: &str = "host.start";
    pub const HOST_STOP: &str = "host.stop";
    pub const HOST_DECIDE: &str = "host.decide";
    pub const HOST_DISCONNECT: &str = "host.disconnect";
    pub const HOST_INVITE: &str = "host.invite";
    pub const HOST_UNINVITE: &str = "host.uninvite";
    pub const HOST_TRUSTED: &str = "host.trusted";
    pub const HOST_REVOKE: &str = "host.revoke";
    pub const CONNECT: &str = "connect";
    pub const DISCONNECT: &str = "disconnect";
    /// Notification sent to subscribers; its params are the new `State`.
    pub const STATE_CHANGED: &str = "state";
}

/// JSON-RPC error codes the engine uses.
pub mod code {
    pub const PARSE_ERROR: i32 = -32700;
    pub const INVALID_REQUEST: i32 = -32600;
    pub const METHOD_NOT_FOUND: i32 = -32601;
    pub const INVALID_PARAMS: i32 = -32602;
    /// The request was understood and could not be carried out; the message
    /// says why, in words fit to show a person.
    pub const FAILED: i32 = -32000;
}

/// A request. `id` comes back in the response; without one the request is a
/// notification and gets no answer.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Request {
    pub jsonrpc: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub id: Option<Value>,
    pub method: String,
    #[serde(default)]
    pub params: Value,
}

impl Request {
    pub fn new(id: u64, method: &str, params: Value) -> Self {
        Self { jsonrpc: "2.0".into(), id: Some(id.into()), method: method.into(), params }
    }
}

/// A response: exactly one of `result` and `error`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Response {
    pub jsonrpc: String,
    pub id: Value,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<Value>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<RpcError>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct RpcError {
    pub code: i32,
    pub message: String,
}

/// Something the engine tells subscribers without being asked.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
pub struct Notification {
    pub jsonrpc: String,
    pub method: String,
    pub params: Value,
}

/// Everything a front end shows, in one value.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct State {
    /// This computer, as other devices see it.
    pub device: ThisDevice,
    /// The account this computer is signed in to, if any.
    pub account: Option<SignedIn>,
    /// Present while this computer is sharing its screen.
    pub hosting: Option<Hosting>,
    /// Stream windows this engine has open.
    pub connections: Vec<Connection>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct ThisDevice {
    pub name: String,
    /// Hex SHA-256 of this device's certificate.
    pub fingerprint: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct SignedIn {
    pub server: String,
    pub username: String,
    pub device_id: String,
}

/// What the host is doing right now.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq, Default)]
pub struct Hosting {
    /// Set when the internet can reach this host, saying what to do about it
    /// (close the router's ports, use Tailscale). Show it prominently.
    #[serde(default)]
    pub warning: Option<String>,
    /// Paired devices connect without being approved.
    pub unattended: bool,
    /// PINs to show: someone is pairing and has to type one.
    pub pins: Vec<PairingPin>,
    /// Connections waiting for `host.decide`.
    pub waiting: Vec<WaitingConnection>,
    pub sessions: Vec<Session>,
    pub invites: Vec<InviteInfo>,
    /// Where other devices sign in, while this computer is the account
    /// server: an address on the tailnet, e.g. `http://100.101.102.103:47900`.
    #[serde(default)]
    pub account_server: Option<String>,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct PairingPin {
    /// Who is pairing, e.g. `Laptop (192.168.1.7)`.
    pub who: String,
    pub pin: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct WaitingConnection {
    pub id: u64,
    /// Who is asking, with the start of their device's fingerprint.
    pub who: String,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Session {
    pub id: u64,
    pub name: String,
    /// Where the device connects from; `0.0.0.0` when it came through the
    /// rendezvous server.
    pub address: String,
    /// The display being streamed, once one is chosen.
    pub display: Option<u32>,
    /// It may use keyboard, mouse and gamepads; otherwise it only watches.
    pub control: bool,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct InviteInfo {
    pub id: String,
    pub control: bool,
    pub minutes_left: u64,
}

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Connection {
    pub id: u64,
    /// What was asked for: an invite link, a device's name or an address.
    pub target: String,
}

/// Params of `account.create` and `account.login`.
#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct SignIn {
    /// The rendezvous server, e.g. `https://play.example.org`.
    pub server: String,
    pub username: String,
    pub password: String,
}

/// Params of `host.start`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct StartHosting {
    /// Let paired devices connect without asking. Off unless asked for.
    #[serde(default)]
    pub unattended: bool,
    /// Share the desktop's sound too.
    #[serde(default = "yes")]
    pub audio: bool,
    /// Also be the account server for your devices, over Tailscale: they
    /// sign in to this computer by its Tailscale name.
    #[serde(default)]
    pub account_server: bool,
    /// On Wayland, also offer a new virtual screen in the share dialog.
    #[serde(default)]
    pub virtual_display: bool,
}

fn yes() -> bool {
    true
}

/// What to do with a waiting connection.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Decision {
    /// Let them see and control.
    Control,
    /// Let them watch only.
    View,
    Deny,
}

/// Params of `host.decide`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decide {
    pub id: u64,
    pub decision: Decision,
}

/// Params of `host.disconnect`: one session, or all of them without an id.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Disconnect {
    #[serde(default)]
    pub id: Option<u64>,
}

/// Params of `host.invite`.
#[derive(Serialize, Deserialize, Debug, Clone, Copy, PartialEq, Eq)]
pub struct NewInvite {
    pub minutes: u32,
    /// Let the guest control, not just watch.
    pub control: bool,
}

/// Result of `host.invite`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct InviteMade {
    pub id: String,
    /// The link to send to the guest.
    pub link: String,
}

/// Params naming one thing by its id.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct ById<T> {
    pub id: T,
}

/// A device this host lets connect, from `host.trusted`.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct TrustedDevice {
    pub name: String,
    pub fingerprint: String,
}

/// Params of `host.revoke`: forget the device whose fingerprint starts with
/// this (at least 8 hex digits) and end its sessions.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Revoke {
    pub fingerprint: String,
}

/// Params of `connect`: open a stream window.
#[derive(Serialize, Deserialize, Debug, Clone, PartialEq, Eq)]
pub struct Connect {
    /// An invite link, the name of one of the account's devices, or an address.
    pub target: String,
    /// Which of the host's displays; asks in the window when there are several.
    #[serde(default)]
    pub display: Option<u32>,
    /// A pairing PIN or password known in advance.
    #[serde(default)]
    pub pin: Option<String>,
    #[serde(default)]
    pub fullscreen: bool,
    #[serde(default)]
    pub stats: bool,
    #[serde(default)]
    pub game_mouse: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_and_responses_follow_json_rpc() {
        let request: Request =
            serde_json::from_str(r#"{"jsonrpc":"2.0","id":7,"method":"host.start","params":{"unattended":true}}"#).unwrap();
        assert_eq!(request.method, method::HOST_START);
        let params: StartHosting = serde_json::from_value(request.params).unwrap();
        assert_eq!(params, StartHosting { unattended: true, audio: true, account_server: false, virtual_display: false });

        // Params may be left out altogether.
        let bare: Request = serde_json::from_str(r#"{"jsonrpc":"2.0","id":"a","method":"state"}"#).unwrap();
        assert_eq!(bare.params, Value::Null);

        let ok = Response { jsonrpc: "2.0".into(), id: 7.into(), result: Some(Value::Null), error: None };
        assert_eq!(serde_json::to_string(&ok).unwrap(), r#"{"jsonrpc":"2.0","id":7,"result":null}"#);
    }

    #[test]
    fn decisions_and_defaults_read_naturally() {
        let d: Decide = serde_json::from_str(r#"{"id":3,"decision":"view"}"#).unwrap();
        assert_eq!(d, Decide { id: 3, decision: Decision::View });
        let c: Connect = serde_json::from_str(r#"{"target":"Office PC"}"#).unwrap();
        assert_eq!((c.display, c.fullscreen, c.pin), (None, false, None));
        assert_eq!(serde_json::from_str::<Disconnect>("{}").unwrap(), Disconnect { id: None });
    }
}
