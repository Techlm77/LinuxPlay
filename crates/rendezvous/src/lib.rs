//! LinuxPlay's rendezvous server.
//!
//! It keeps accounts and their device lists, knows which devices are running
//! as hosts right now, and joins a caller to a host with a tunnel that only
//! carries bytes. See [`linuxplay_proto::rendezvous`] for the API and for
//! what the server is, and is not, trusted with.

mod accounts;
mod connect;
mod error;
mod exposure;
mod limits;
mod relay;
mod secrets;
mod store;
pub mod stun;

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{ConnectInfo, FromRequestParts, State};
use axum::{Json, Router};
use axum::http::request::Parts;
use axum::routing::{delete, get, post};
use linuxplay_proto::rendezvous::{API_PREFIX, ServerInfo};

use linuxplay_net::exposure::Exposure;

use self::error::Failure;
use self::limits::RateLimiter;
use self::relay::Relay;
pub use self::store::Store;
use self::store::Identity;

/// Sign-in attempts allowed per address, and per username, in ten minutes.
const SIGN_IN_ATTEMPTS: u32 = 10;
/// Accounts one address may create in an hour.
const ACCOUNTS_PER_HOUR: u32 = 5;
/// Tunnels one address may ask for in a minute.
const CALLS_PER_MINUTE: u32 = 30;

/// How the server is run.
#[derive(Debug, Clone, Default)]
pub struct Config {
    /// Believe the `X-Forwarded-For` header for the caller's address. Only
    /// for when a reverse proxy that sets it is the only way in.
    pub trust_forwarded_for: bool,
    /// UDP port the STUN service is reachable on, if one runs.
    pub stun_port: Option<u16>,
    /// Every address the server listens on, to warn if the internet can
    /// reach any of them.
    pub listening: Vec<SocketAddr>,
}

/// Everything the request handlers share.
pub struct App {
    store: Store,
    relay: Arc<Relay>,
    config: Config,
    exposure: Exposure,
    /// A real password hash that matches nothing anyone will type.
    stand_in_hash: String,
    sign_ins: RateLimiter,
    new_accounts: RateLimiter,
    calls: RateLimiter,
}

impl App {
    pub fn new(store: Store, config: Config) -> Arc<Self> {
        let stand_in_hash = secrets::hash_password(&secrets::new_token()).expect("hashing with a fresh salt works");
        let exposure = Exposure::new(&config.listening);
        Arc::new(Self {
            exposure,
            store,
            relay: Arc::default(),
            config,
            stand_in_hash,
            sign_ins: RateLimiter::new(SIGN_IN_ATTEMPTS, Duration::from_secs(600)),
            new_accounts: RateLimiter::new(ACCOUNTS_PER_HOUR, Duration::from_secs(3600)),
            calls: RateLimiter::new(CALLS_PER_MINUTE, Duration::from_secs(60)),
        })
    }

    /// Run database work off the async threads.
    async fn db<T: Send + 'static>(
        &self,
        work: impl FnOnce(&Store) -> rusqlite::Result<T> + Send + 'static,
    ) -> Result<T, Failure> {
        let store = self.store.clone();
        tokio::task::spawn_blocking(move || work(&store))
            .await
            .map_err(Failure::internal)?
            .map_err(Failure::internal)
    }
}

/// The server's routes. Serve it with connection info, as [`serve`] does.
pub fn router(app: Arc<App>) -> Router {
    let api = Router::new()
        .route("/health", get(|| async { "ok" }))
        .route("/info", get(info))
        .route("/accounts", post(accounts::create))
        .route("/sessions", post(accounts::sign_in))
        .route("/sessions/current", delete(accounts::sign_out))
        .route("/devices", get(accounts::devices))
        .route("/devices/{id}", delete(accounts::remove_device))
        .route("/invites", post(connect::create_invite))
        .route("/invites/{id}", delete(connect::revoke_invite))
        .route("/host", get(connect::host_presence))
        .route("/tunnel", get(connect::call))
        .route("/tunnel/{id}/accept", get(connect::accept));
    Router::new()
        .nest(API_PREFIX, api)
        .layer(axum::middleware::from_fn_with_state(app.clone(), exposure::note_caller))
        .with_state(app)
}

/// The account server running on some addresses; dropping it stops it.
pub struct Listening {
    tasks: Vec<tokio::task::JoinHandle<std::io::Result<()>>>,
    pub addresses: Vec<SocketAddr>,
}

impl Drop for Listening {
    fn drop(&mut self) {
        for task in &self.tasks {
            task.abort();
        }
    }
}

/// Serve the API on every one of `addresses`.
pub async fn listen(addresses: &[SocketAddr], app: Arc<App>) -> std::io::Result<Listening> {
    let mut listening = Listening { tasks: Vec::new(), addresses: Vec::new() };
    for addr in addresses {
        let listener = tokio::net::TcpListener::bind(addr).await?;
        listening.addresses.push(listener.local_addr()?);
        listening.tasks.push(tokio::spawn(serve(listener, app.clone())));
    }
    Ok(listening)
}

/// `GET /info`: what devices need to know about this server.
async fn info(State(app): State<Arc<App>>) -> Json<ServerInfo> {
    Json(ServerInfo { stun_port: app.config.stun_port, warning: app.exposure.warning() })
}

/// Serve the API on `listener` until the task is dropped.
pub async fn serve(listener: tokio::net::TcpListener, app: Arc<App>) -> std::io::Result<()> {
    axum::serve(listener, router(app).into_make_service_with_connect_info::<SocketAddr>()).await
}

/// The signed-in device making a request, from its bearer token.
pub struct Authed(pub Identity);

impl FromRequestParts<Arc<App>> for Authed {
    type Rejection = Failure;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Failure> {
        let token = parts
            .headers
            .get(axum::http::header::AUTHORIZATION)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.strip_prefix("Bearer "))
            .ok_or_else(|| Failure::unauthorized("Sign in first."))?
            .to_string();
        let identity = app.db(move |store| store.identify(&token)).await?;
        identity.map(Authed).ok_or_else(|| Failure::unauthorized("This sign-in is no longer valid. Sign in again."))
    }
}

/// The signed-in device, when the request carries a token at all.
pub struct MaybeAuthed(pub Option<Identity>);

impl FromRequestParts<Arc<App>> for MaybeAuthed {
    type Rejection = Failure;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Failure> {
        if !parts.headers.contains_key(axum::http::header::AUTHORIZATION) {
            return Ok(Self(None));
        }
        Authed::from_request_parts(parts, app).await.map(|Authed(identity)| Self(Some(identity)))
    }
}

/// The address a request came from, for rate limits.
pub struct ClientIp(pub IpAddr);

impl FromRequestParts<Arc<App>> for ClientIp {
    type Rejection = Failure;

    async fn from_request_parts(parts: &mut Parts, app: &Arc<App>) -> Result<Self, Failure> {
        if app.config.trust_forwarded_for {
            // The last entry is the one our own proxy added.
            let forwarded = parts
                .headers
                .get("x-forwarded-for")
                .and_then(|v| v.to_str().ok())
                .and_then(|v| v.rsplit(',').next())
                .and_then(|ip| ip.trim().parse().ok());
            if let Some(ip) = forwarded {
                return Ok(Self(ip));
            }
        }
        let ConnectInfo(peer) = ConnectInfo::<SocketAddr>::from_request_parts(parts, app)
            .await
            .map_err(|_| Failure::internal("the server was started without connection info"))?;
        Ok(Self(peer.ip().to_canonical()))
    }
}
