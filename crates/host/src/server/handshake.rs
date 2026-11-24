//! From an accepted connection to an approved one: `Hello`, pairing, the
//! decision of the person at the host, and the choice of display.

use std::net::IpAddr;
use std::sync::{Arc, Mutex};
use std::time::Instant;

use anyhow::{Context, Result, anyhow, bail};
use linuxplay_net::tls;
use linuxplay_net::ws::Accepted;
use linuxplay_net::pair;
use linuxplay_proto::PROTOCOL_VERSION;
use linuxplay_proto::signal::{ClientMsg, HostMsg, MonitorInfo, VideoCodec};
use tracing::info;

use super::stream::StreamGuard;
use super::{Host, MAX_STREAMS};
use crate::approval::Decision;
use crate::limits::{self, PairLimiter, PendingPermit};
use crate::quality::ClientCaps;
use crate::sessions::{Grant, Ticket};

/// What the client said in `Hello`.
pub(super) struct HelloInfo {
    pub(super) name: String,
    pub(super) caps: ClientCaps,
    pub(super) audio: bool,
}

/// Ends a pairing attempt in the limiter, as a failure unless marked ok.
struct PairAttempt {
    limiter: Arc<Mutex<PairLimiter>>,
    source: IpAddr,
    ok: bool,
}

impl Drop for PairAttempt {
    fn drop(&mut self) {
        self.limiter.lock().unwrap().finish(self.source, self.ok, Instant::now());
    }
}

impl Host {
    /// `Hello`, pairing if needed, `Welcome`.
    pub(super) async fn authenticate(&self, acc: &mut Accepted) -> Result<HelloInfo> {
        let ClientMsg::Hello { version, name, codecs, hw_codecs, max_fps, audio, pair } =
            acc.signal.expect_within(self.timeouts.hello, "Hello").await?
        else {
            bail!("expected Hello");
        };
        let name: String = name.chars().filter(|c| !c.is_control()).take(64).collect();
        if version != PROTOCOL_VERSION {
            let message = format!("this host speaks protocol {PROTOCOL_VERSION}, the client {version}; update both");
            acc.signal.send(&HostMsg::Error { message: message.clone() }).await?;
            bail!(message);
        }
        // A guest proves the invite's secret every time; a device of the
        // owner's pairs once and is trusted from then on.
        if acc.invite.is_some() || pair || !self.is_trusted(&acc.client_fp) {
            self.pair(acc, &name).await?;
        }
        acc.signal.send(&HostMsg::Welcome { host_name: self.cfg.name.clone() }).await?;
        let few = |v: Vec<VideoCodec>| v.into_iter().take(8).collect::<Vec<_>>();
        let caps = ClientCaps {
            codecs: few(codecs),
            hw_codecs: few(hw_codecs),
            max_fps: if max_fps == 0 { 60 } else { max_fps.min(240) },
        };
        Ok(HelloInfo { name, caps, audio })
    }

    pub(super) async fn pair(&self, acc: &mut Accepted, name: &str) -> Result<()> {
        let source = limits::source(acc.route.peer_ip());
        let begun = self.pairing.lock().unwrap().begin(source, Instant::now());
        if let Err(denied) = begun {
            let message = denied.to_string();
            acc.signal.send(&HostMsg::Error { message: message.clone() }).await?;
            bail!(message);
        }
        let mut attempt = PairAttempt { limiter: self.pairing.clone(), source, ok: false };

        // What the client has to know: an invite's secret, this host's
        // pairing password, or a PIN shown here just for this attempt.
        let invite = match &acc.invite {
            Some(id) => Some(self.invites.get(id).context("this invite has expired or was withdrawn")?),
            None => None,
        };
        let (pin, typed_password, _shown) = match (&invite, &self.cfg.password) {
            (Some(grant), _) => (grant.secret.clone(), true, None),
            (None, Some(p)) => (p.clone(), true, None),
            (None, None) => {
                let pin = pair::new_pin();
                let shown = self.pins.show(name, acc.route.peer_ip(), &pin).await;
                (pin, false, Some(shown))
            }
        };
        acc.signal
            .send(&HostMsg::PairingRequired { host_name: self.cfg.name.clone(), password: typed_password })
            .await?;

        let ClientMsg::PairStart { spake } = acc.signal.expect_within(self.timeouts.pair, "the PIN").await? else {
            bail!("expected PairStart");
        };
        let fail = |e: anyhow::Error| e.context(format!("pairing with {name} failed"));
        let (state, reply, mac) =
            pair::host_respond(&pin, &spake, &self.identity.fingerprint, &acc.client_fp).map_err(fail)?;
        acc.signal.send(&HostMsg::PairReply { spake: reply, mac }).await?;
        // A client that saw a wrong PIN hangs up instead of answering.
        let ClientMsg::PairFinish { mac } =
            acc.signal.expect_within(self.timeouts.pair_finish, "PairFinish").await.map_err(fail)?
        else {
            bail!("expected PairFinish");
        };
        state.verify(&mac).map_err(fail)?;

        attempt.ok = true;
        if invite.is_some() {
            // A guest: let in for this visit, never added to the trusted devices.
            info!("{name} came by invite");
        } else {
            self.trust.lock().unwrap().add(&acc.client_fp, name)?;
            info!("paired with {name}");
        }
        Ok(())
    }

    /// Everything after `Welcome`: approval, display choice, streaming.
    pub(super) async fn serve(&self, acc: &mut Accepted, ticket: &Ticket, permit: PendingPermit, hello: &HelloInfo) -> Result<()> {
        // It may have been revoked between authenticating and registering.
        let invite = match &acc.invite {
            Some(id) => Some(self.invites.get(id).context("this invite has expired or was withdrawn")?),
            None => None,
        };
        if invite.is_none() && !self.is_trusted(&acc.client_fp) {
            bail!("this device is no longer trusted");
        }
        let mut grant = self.approve(acc, ticket, &hello.name).await?;
        if invite.is_some_and(|i| !i.control) && grant.control {
            // The invite was for watching only, whatever was clicked here.
            grant.control = false;
            ticket.approve(grant.clone());
        }
        // Approved: it stops counting as pending, and counts as a session.
        drop(permit);
        let _quota = self
            .session_quota
            .clone()
            .try_acquire_owned()
            .map_err(|_| anyhow!("this host has too many connections open"))?;
        acc.signal.send(&HostMsg::Approved { control: grant.control, audio: grant.audio }).await?;

        // Only the displays this session may see (this is also what opens
        // the Wayland share dialog).
        let monitors: Vec<MonitorInfo> =
            self.monitors().await?.into_iter().filter(|m| grant.allows_display(m.index)).collect();
        if monitors.is_empty() {
            bail!("no display is shared with this device");
        }
        acc.signal.send(&HostMsg::Monitors { monitors: monitors.clone() }).await?;
        let index = match acc.signal.expect_within::<ClientMsg>(self.timeouts.select, "a display choice").await? {
            ClientMsg::SelectMonitor { index } => index,
            _ => bail!("expected SelectMonitor"),
        };
        if !monitors.iter().any(|m| m.index == index) {
            bail!("display {index} isn't shared with this device");
        }

        // Reserve the display: one stream each, up to the overall cap.
        {
            let mut active = self.active.lock().unwrap();
            if active.contains(&index) {
                bail!("that display is already being streamed");
            } else if active.len() >= MAX_STREAMS {
                bail!("this host is already streaming {MAX_STREAMS} displays");
            }
            active.insert(index);
        }
        let _guard = StreamGuard { host: self, monitor: index };
        ticket.set_display(index);
        info!(
            "{} ({}) streaming display {index}{}",
            hello.name,
            acc.route,
            if grant.control { "" } else { ", view only" }
        );
        self.stream(acc, hello, &grant, index, ticket.active.clone()).await
    }

    /// Decide what this session may do. Attended (the default): ask the
    /// person at the host, unless this device is already in an approved
    /// visit (another of its windows is open). Unattended: allow, and say so.
    pub(super) async fn approve(&self, acc: &mut Accepted, ticket: &Ticket, name: &str) -> Result<Grant> {
        let full = Grant { control: true, audio: self.cfg.audio, displays: self.cfg.displays.clone() };
        let grant = if self.cfg.unattended {
            self.announce_connected(name, acc.route.peer_ip()).await;
            full
        } else if let Some(g) = self.sessions.grant_of(&acc.client_fp, ticket.id) {
            g
        } else {
            acc.signal.send(&HostMsg::AwaitingApproval).await?;
            let device: String = tls::fingerprint_display(&acc.client_fp).chars().take(19).collect();
            let decision = tokio::select! {
                d = self.approver.ask(ticket.id, name, acc.route.peer_ip(), &device) => d,
                _ = acc.signal.recv::<ClientMsg>() => bail!("the client left before the host answered"),
            };
            match decision {
                Decision::Deny => bail!("the host declined the connection"),
                Decision::ViewOnly => Grant { control: false, ..full },
                Decision::Control => full,
            }
        };
        ticket.approve(grant.clone());
        Ok(grant)
    }
}
