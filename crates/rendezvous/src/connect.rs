//! Reaching a host: its presence connection, invites, and the two ends of a
//! tunnel.

use std::sync::Arc;

use axum::Json;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::Response;
use futures_util::{SinkExt, StreamExt};
use linuxplay_proto::rendezvous::{Caller, HostNotice, Invite, NewInvite, TunnelTarget};
use serde::Deserialize;
use tracing::{debug, info};

use crate::error::{ApiResult, Failure};
use crate::relay::{self, ACCEPT_TIMEOUT, MAX_TUNNEL_MESSAGE};
use crate::store::{self, Identity};
use crate::{App, Authed, ClientIp, MaybeAuthed};

/// The query of `GET /tunnel`: exactly one of the two.
#[derive(Deserialize)]
pub struct TargetQuery {
    device: Option<String>,
    invite: Option<String>,
}

impl TargetQuery {
    fn target(self) -> ApiResult<TunnelTarget> {
        match (self.device, self.invite) {
            (Some(id), None) => Ok(TunnelTarget::Device(id)),
            (None, Some(id)) => Ok(TunnelTarget::Invite(id)),
            _ => Err(Failure::bad_request("Say which device or which invite to reach.")),
        }
    }
}

/// Longest an invite may stay valid: a week.
const MAX_INVITE_SECS: u64 = 7 * 24 * 3600;

/// `GET /host` (WebSocket): a host announces it can be connected to and
/// receives [`HostNotice`]s as JSON text while the connection lasts.
pub async fn host_presence(State(app): State<Arc<App>>, Authed(me): Authed, ws: WebSocketUpgrade) -> Response {
    ws.on_upgrade(move |socket| presence(app, me, socket))
}

async fn presence(app: Arc<App>, me: Identity, socket: WebSocket) {
    let mut online = app.relay.go_online(&me.device_id);
    let device = me.device_id.clone();
    let _ = app.db(move |store| store.touch(&device)).await;
    info!("host {} online", me.device_id);
    let (mut tx, mut rx) = socket.split();
    loop {
        tokio::select! {
            notice = online.notices.recv() => {
                // No more notices: a newer connection of this device took over.
                let Some(notice) = notice else { break };
                let text = serde_json::to_string(&notice).expect("notices always serialise");
                if tx.send(Message::Text(text.into())).await.is_err() {
                    break;
                }
            }
            // Hosts say nothing here; reading notices when one goes away.
            incoming = rx.next() => match incoming {
                Some(Ok(Message::Close(_))) | Some(Err(_)) | None => break,
                Some(Ok(_)) => {}
            },
        }
    }
    drop(online);
    let device = me.device_id.clone();
    let _ = app.db(move |store| store.touch(&device)).await;
    info!("host {} offline", me.device_id);
}

/// `POST /invites`: a host makes a name by which guests can reach it.
pub async fn create_invite(
    State(app): State<Arc<App>>,
    Authed(me): Authed,
    Json(new): Json<NewInvite>,
) -> ApiResult<(StatusCode, Json<Invite>)> {
    if new.valid_for_secs == 0 || new.valid_for_secs > MAX_INVITE_SECS {
        return Err(Failure::bad_request("An invite can be valid for up to a week."));
    }
    let expires_at = store::now() + new.valid_for_secs;
    let id = app.db(move |store| store.create_invite(&me.device_id, expires_at)).await?;
    Ok((StatusCode::CREATED, Json(Invite { id, expires_at })))
}

/// `DELETE /invites/{id}`: a host withdraws one of its invites.
pub async fn revoke_invite(State(app): State<Arc<App>>, Authed(me): Authed, Path(id): Path<String>) -> ApiResult<StatusCode> {
    if !app.db(move |store| store.revoke_invite(&me.device_id, &id)).await? {
        return Err(Failure::not_found("No such invite."));
    }
    Ok(StatusCode::NO_CONTENT)
}

/// `GET /tunnel?device=ID` or `?invite=ID` (WebSocket): ask to be connected
/// to one of the caller's own devices, or to the host behind an invite.
/// Once the host accepts, binary messages pass between the two.
pub async fn call(
    State(app): State<Arc<App>>,
    ClientIp(ip): ClientIp,
    MaybeAuthed(me): MaybeAuthed,
    Query(query): Query<TargetQuery>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    if !app.calls.allow(&ip.to_string()) {
        return Err(Failure::rate_limited());
    }
    let (host_device, from) = match query.target()? {
        TunnelTarget::Device(device_id) => {
            let me = me.ok_or_else(|| Failure::unauthorized("Sign in to reach your devices."))?;
            let wanted = device_id.clone();
            let host = app.db(move |store| store.device(&wanted)).await?;
            // Someone else's device looks exactly like one that doesn't exist.
            if !host.is_some_and(|h| h.account_id == me.account_id) {
                return Err(Failure::not_found("No such device in this account."));
            }
            let mine = me.device_id.clone();
            let caller = app.db(move |store| store.device(&mine)).await?.ok_or_else(|| Failure::internal("caller vanished"))?;
            (device_id, Caller::Device { id: caller.id, name: caller.name, fingerprint: caller.fingerprint })
        }
        TunnelTarget::Invite(invite_id) => {
            let wanted = invite_id.clone();
            let host = app.db(move |store| store.invite_host(&wanted)).await?;
            let host = host.ok_or_else(|| Failure::not_found("This invite doesn't exist or has expired."))?;
            (host, Caller::Invite { id: invite_id })
        }
    };

    let (tunnel, host_socket) = app
        .relay
        .call(&host_device, |tunnel| HostNotice::Connect { tunnel, from })
        .ok_or_else(|| Failure::offline("That computer isn't available right now."))?;
    Ok(ws.max_message_size(MAX_TUNNEL_MESSAGE).on_upgrade(move |caller| async move {
        match tokio::time::timeout(ACCEPT_TIMEOUT, host_socket).await {
            Ok(Ok(host)) => relay::splice(caller, host).await,
            _ => {
                debug!("tunnel {tunnel}: the host didn't pick up");
                app.relay.forget(&tunnel);
            }
        }
    }))
}

/// `GET /tunnel/{id}/accept` (WebSocket): a host picks up a tunnel it was
/// told about.
pub async fn accept(
    State(app): State<Arc<App>>,
    Authed(me): Authed,
    Path(tunnel): Path<String>,
    ws: WebSocketUpgrade,
) -> ApiResult<Response> {
    if !app.relay.is_waiting_for(&tunnel, &me.device_id) {
        return Err(Failure::not_found("No such tunnel is waiting for this device."));
    }
    Ok(ws.max_message_size(MAX_TUNNEL_MESSAGE).on_upgrade(move |socket| async move {
        // If the caller gave up in the meantime the socket is simply dropped.
        app.relay.accept(&tunnel, &me.device_id, socket);
    }))
}
