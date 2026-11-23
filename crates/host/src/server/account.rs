//! The host's side of an account: staying reachable through the rendezvous
//! server, answering the tunnels it announces, and handing out invites.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use linuxplay_net::rendezvous::Account;
use linuxplay_net::ws::{self, Route};
use linuxplay_proto::rendezvous::{Caller, HostNotice, InviteLink};
use tokio_rustls::TlsAcceptor;
use tracing::{debug, info, warn};

use super::Host;

/// Longest wait between attempts to reconnect to the rendezvous server.
const MAX_RECONNECT_WAIT: Duration = Duration::from_secs(60);
/// Longest an invite may be valid, in minutes: a week, as the server allows.
const MAX_INVITE_MINUTES: u32 = 7 * 24 * 60;

impl Host {
    /// Keep this host reachable through the account's rendezvous server:
    /// stay connected, and answer every tunnel it announces.
    pub(super) async fn stay_reachable(self: Arc<Self>, account: Account) {
        let acceptor = match self.identity.server_config() {
            Ok(config) => TlsAcceptor::from(Arc::new(config)),
            Err(e) => return warn!("this host can't be reached through its account: {e:#}"),
        };
        let mut wait = Duration::from_secs(1);
        loop {
            match account.go_online().await {
                Ok(mut presence) => {
                    info!("reachable as {} through {}", account.username(), account.server().address());
                    wait = Duration::from_secs(1);
                    let stun = account.server().stun().await;
                    while let Some(HostNotice::Connect { tunnel, from }) = presence.next().await {
                        let (host, account, acceptor) = (self.clone(), account.clone(), acceptor.clone());
                        tokio::spawn(async move {
                            if let Err(e) = host.answer(&account, &acceptor, &tunnel, from, stun).await {
                                debug!("tunnel {tunnel}: {e:#}");
                            }
                        });
                    }
                    warn!("lost the connection to {}; reconnecting", account.server().address());
                }
                Err(e) => warn!("can't reach {}: {e:#}; trying again in {wait:?}", account.server().address()),
            }
            tokio::time::sleep(wait).await;
            wait = (wait * 2).min(MAX_RECONNECT_WAIT);
        }
    }

    /// Pick up a tunnel from the rendezvous server and treat what comes
    /// through it like any other connection: the same TLS, pairing and
    /// approval. Being introduced by the server earns no trust.
    pub(super) async fn answer(
        self: &Arc<Self>,
        account: &Account,
        acceptor: &TlsAcceptor,
        tunnel: &str,
        from: Caller,
        stun: Option<SocketAddr>,
    ) -> Result<()> {
        let route = Route::Relayed { stun };
        let Some(permit) = self.pending.try_acquire(route.peer_ip()) else {
            bail!("refused, too many pending connections");
        };
        let io = account.accept_tunnel(tunnel).await?;
        let mut acc = ws::accept_io(io, route, acceptor).await?;
        if let Caller::Invite { id } = from {
            acc.invite = Some(id);
        }
        self.handle(acc, permit).await
    }

    /// Make an invite and the link that carries it.
    pub async fn invite(&self, minutes: u32, control: bool) -> Result<InviteLink> {
        let account = self.cfg.account.as_ref().context("sign in to an account first; invites go through it")?;
        if minutes == 0 || minutes > MAX_INVITE_MINUTES {
            bail!("an invite can be valid for up to a week");
        }
        let valid_for = Duration::from_secs(minutes as u64 * 60);
        let invite = account.create_invite(valid_for).await?;
        let Some(grant) = self.invites.add(&invite.id, control, valid_for) else {
            let _ = account.revoke_invite(&invite.id).await;
            bail!("this host already has as many invites as it can keep; withdraw one first");
        };
        Ok(InviteLink { server: account.server().address().to_string(), id: invite.id, secret: grant.secret })
    }

    /// Withdraw an invite: nobody else gets in by it, and whoever did is
    /// disconnected. Returns how many sessions ended.
    pub async fn uninvite(&self, id: &str) -> Result<usize> {
        if !self.invites.revoke(id) {
            bail!("no such invite");
        }
        if let Some(account) = &self.cfg.account
            && let Err(e) = account.revoke_invite(id).await {
                debug!("withdrawing invite {id} on the server: {e:#}");
            }
        let guests: Vec<u64> = self.guests.lock().unwrap().iter().filter(|(_, i)| *i == id).map(|(s, _)| *s).collect();
        Ok(guests.into_iter().filter(|session| self.sessions.kill(*session)).count())
    }
}
