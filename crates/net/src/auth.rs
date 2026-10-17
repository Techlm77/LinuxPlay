//! The client's half of authentication, as an explicit state machine.
//!
//! A host the client has not pinned proves itself only by completing PIN
//! pairing: the client must have checked the host's key-confirmation MAC
//! (which binds both certificates) before it accepts `Welcome`. A host that
//! skips straight to `Welcome` is impersonating one and is refused. A pinned
//! host proves itself by the TLS certificate matching the pin, which is
//! checked before anything is sent.

use std::future::Future;
use std::time::Duration;

use anyhow::{Result, anyhow, bail};
use linuxplay_proto::signal::{ClientMsg, HostMsg};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::pair::{self, ClientPairing};
use crate::tls::Fingerprint;
use crate::ws::Signal;

/// How long to wait for each message from the host while authenticating.
/// (Typing the PIN is the user's time and isn't counted.)
const HOST_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// Where the client stands with the host.
enum State {
    /// Not pinned, nothing proven: only pairing can make it trusted.
    Unknown,
    /// Pinned, and the TLS certificate matched the pin.
    KnownHostVerified,
    /// `PairStart` sent; waiting for the host's `PairReply`.
    PairingInProgress(ClientPairing),
    /// The host's MAC checked out and ours is sent; only `Welcome` may follow.
    PairingProven,
}

/// The host is authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authenticated {
    pub host_name: String,
    /// True when this connection paired, so the host's certificate is newly
    /// proven and should be pinned.
    pub paired: bool,
}

/// What the client says about itself in `Hello` (the `pair` flag is decided
/// here, from whether the host is pinned).
pub struct Hello {
    pub version: u32,
    pub name: String,
    pub codecs: Vec<linuxplay_proto::signal::VideoCodec>,
    pub hw_codecs: Vec<linuxplay_proto::signal::VideoCodec>,
    pub max_fps: u32,
    pub audio: bool,
}

/// Run the client side up to `Welcome`. `host_fp` is the certificate the TLS
/// handshake saw; `pinned` the one remembered for this host, if any.
/// `ask_pin(host_name, is_password)` asks the user; `None` cancels.
///
/// Persist `host_fp` only after this returns `Ok` with `paired` set.
pub async fn authenticate<S, F, Fut>(
    signal: &mut Signal<S>,
    host_fp: &Fingerprint,
    client_fp: &Fingerprint,
    pinned: Option<&Fingerprint>,
    hello: Hello,
    mut ask_pin: F,
) -> Result<Authenticated>
where
    S: AsyncRead + AsyncWrite + Unpin,
    F: FnMut(String, bool) -> Fut,
    Fut: Future<Output = Option<String>>,
{
    let mut state = match pinned {
        Some(p) if p == host_fp => State::KnownHostVerified,
        Some(_) => bail!(
            "This host's identity has changed since you paired with it. If it was reinstalled, \
             forget it and pair again; otherwise someone may be intercepting the connection."
        ),
        None => State::Unknown,
    };
    signal
        .send(&ClientMsg::Hello {
            version: hello.version,
            name: hello.name,
            codecs: hello.codecs,
            hw_codecs: hello.hw_codecs,
            max_fps: hello.max_fps,
            audio: hello.audio,
            pair: matches!(state, State::Unknown),
        })
        .await?;

    loop {
        let msg: HostMsg = signal.expect_within(HOST_REPLY_TIMEOUT, "the host").await?;
        state = match (state, msg) {
            (State::Unknown | State::KnownHostVerified, HostMsg::PairingRequired { host_name, password }) => {
                let pin = ask_pin(host_name, password).await.ok_or_else(|| anyhow!("pairing cancelled"))?;
                let (pairing, start) = pair::client_start(&pin);
                signal.send(&ClientMsg::PairStart { spake: start }).await?;
                State::PairingInProgress(pairing)
            }
            (State::PairingInProgress(pairing), HostMsg::PairReply { spake, mac }) => {
                // Fails unless the host knew the PIN and saw our certificate.
                let mac = pairing.finish(&spake, &mac, host_fp, client_fp)?;
                signal.send(&ClientMsg::PairFinish { mac }).await?;
                State::PairingProven
            }
            (State::KnownHostVerified, HostMsg::Welcome { host_name }) => {
                return Ok(Authenticated { host_name, paired: false });
            }
            (State::PairingProven, HostMsg::Welcome { host_name }) => {
                return Ok(Authenticated { host_name, paired: true });
            }
            (State::Unknown | State::PairingInProgress(_), HostMsg::Welcome { .. }) => bail!(
                "the host skipped pairing, so its identity is unproven; refusing it \
                 (someone may be impersonating it)"
            ),
            (_, HostMsg::Error { message }) => bail!(message),
            (_, m) => bail!("unexpected message while authenticating: {}", kind(&m)),
        };
    }
}

fn kind(m: &HostMsg) -> &'static str {
    match m {
        HostMsg::PairingRequired { .. } => "pairing_required",
        HostMsg::PairReply { .. } => "pair_reply",
        HostMsg::Welcome { .. } => "welcome",
        HostMsg::AwaitingApproval => "awaiting_approval",
        HostMsg::Approved { .. } => "approved",
        HostMsg::Monitors { .. } => "monitors",
        HostMsg::Offer { .. } => "offer",
        HostMsg::Error { .. } => "error",
    }
}
