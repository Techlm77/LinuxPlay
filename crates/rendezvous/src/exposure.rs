//! Noting where requests come from, to warn when the account server can be
//! reached from the internet (see [`linuxplay_net::exposure`]).

use std::net::SocketAddr;
use std::sync::Arc;

use axum::extract::{ConnectInfo, Request, State};
use axum::middleware::Next;
use axum::response::Response;

use crate::App;

/// Middleware: note every caller. Behind a proxy the operator vouches for,
/// the proxy is the caller and says nothing about the internet.
pub async fn note_caller(State(app): State<Arc<App>>, request: Request, next: Next) -> Response {
    if !app.config.trust_forwarded_for
        && let Some(ConnectInfo(peer)) = request.extensions().get::<ConnectInfo<SocketAddr>>() {
            app.exposure.contact(peer.ip());
        }
    next.run(request).await
}
