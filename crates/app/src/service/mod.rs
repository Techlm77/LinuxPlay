//! The engine: one long-running process that keeps this computer signed in,
//! shares it when asked, opens stream windows, and reports everything a
//! front end needs to show. A front end drives it through the local API
//! described in [`linuxplay_proto::api`].

mod hosting;
mod socket;
mod viewers;

use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use linuxplay_host::Decision;
use linuxplay_net::rendezvous::{Account, Server};
use linuxplay_net::tls::{self, Identity};
use linuxplay_proto::api::{self, RpcError, code, method};
use linuxplay_proto::rendezvous::DeviceInfo;
use serde::de::DeserializeOwned;
use serde_json::{Value, json};
use tokio::sync::{Mutex as AsyncMutex, watch};
use tracing::{info, warn};

use self::hosting::Running;
pub use self::socket::socket_path;
use self::viewers::Viewers;

/// How often the engine looks for changes to tell subscribers about.
const WATCH_INTERVAL: Duration = Duration::from_millis(250);

pub struct Engine {
    device: api::ThisDevice,
    account: Mutex<Option<Account>>,
    /// The running host. Starting and stopping take a while, so they queue
    /// here rather than overlap.
    hosting: AsyncMutex<Option<Running>>,
    /// What the running host was started with, to start it again the same way.
    hosting_options: Mutex<Option<api::StartHosting>>,
    /// What the host last reported, for `state` to read without waiting.
    hosting_status: Mutex<Option<api::Hosting>>,
    viewers: Viewers,
    state: watch::Sender<api::State>,
}

/// Run the engine until it is interrupted.
pub async fn run(name: String) -> Result<()> {
    gstreamer::init().context("GStreamer failed to start")?;
    linuxplay_net::init_crypto();
    let path = socket_path();
    let (listener, _socket) = socket::bind(&path).await?;
    let engine = Engine::new(name)?;
    info!("API on {}", path.display());
    tokio::spawn(socket::serve(listener, engine.clone()));
    tokio::spawn(engine.clone().watch_for_changes());

    tokio::signal::ctrl_c().await.context("waiting for the interrupt signal")?;
    info!("stopping");
    if let Some(running) = engine.hosting.lock().await.take() {
        running.stop().await;
    }
    Ok(())
}

impl Engine {
    fn new(name: String) -> Result<Arc<Self>> {
        let identity = Identity::load_or_create(&tls::config_dir("host"), "LinuxPlay host")
            .context("creating this device's identity")?;
        // A damaged sign-in file means signed out, not an engine that won't start.
        let account = Account::load(&Account::default_path()).inspect_err(|e| warn!("not signed in: {e:#}")).ok().flatten();
        let device = api::ThisDevice { name, fingerprint: hex::encode(identity.fingerprint) };
        let engine = Arc::new(Self {
            device,
            account: Mutex::new(account),
            hosting: AsyncMutex::new(None),
            hosting_options: Mutex::new(None),
            hosting_status: Mutex::new(None),
            viewers: Viewers::default(),
            state: watch::channel(api::State::default()).0,
        });
        engine.publish();
        Ok(engine)
    }

    /// A receiver that wakes whenever the state changes.
    pub fn watch(&self) -> watch::Receiver<api::State> {
        self.state.subscribe()
    }

    fn snapshot(&self) -> api::State {
        let account = self.account.lock().unwrap().as_ref().map(|a| api::SignedIn {
            server: a.server().address().to_string(),
            username: a.username().to_string(),
            device_id: a.device_id().to_string(),
        });
        api::State {
            device: self.device.clone(),
            account,
            hosting: self.hosting_status.lock().unwrap().clone(),
            connections: self.viewers.list(),
        }
    }

    /// Tell subscribers if anything changed.
    fn publish(&self) {
        let now = self.snapshot();
        self.state.send_if_modified(|current| {
            let changed = *current != now;
            if changed {
                *current = now;
            }
            changed
        });
    }

    /// Notice what changes without a request: pairing PINs, connections
    /// waiting for an answer, sessions and windows coming and going.
    async fn watch_for_changes(self: Arc<Self>) {
        let mut tick = tokio::time::interval(WATCH_INTERVAL);
        loop {
            tick.tick().await;
            self.refresh_hosting();
            self.publish();
        }
    }

    /// Read the host's status again. While a start or stop holds the host,
    /// that call publishes its result itself.
    fn refresh_hosting(&self) {
        let Ok(mut hosting) = self.hosting.try_lock() else { return };
        if hosting.as_ref().is_some_and(Running::has_ended) {
            warn!("the host stopped on its own");
            *hosting = None;
        }
        *self.hosting_status.lock().unwrap() = hosting.as_ref().map(Running::status);
    }

    /// Carry out one API call.
    pub async fn call(&self, name: &str, params: Value) -> Result<Value, RpcError> {
        let result = self.dispatch(name, params).await;
        self.refresh_hosting();
        self.publish();
        result
    }

    async fn dispatch(&self, name: &str, params: Value) -> Result<Value, RpcError> {
        match name {
            method::STATE | method::SUBSCRIBE => {
                self.refresh_hosting();
                self.state_value()
            }
            method::ACCOUNT_CREATE => self.sign_in(parse(params)?, true).await,
            method::ACCOUNT_LOGIN => self.sign_in(parse(params)?, false).await,
            method::ACCOUNT_LOGOUT => self.sign_out().await,
            method::ACCOUNT_DEVICES => done(self.account()?.devices().await),
            method::ACCOUNT_REMOVE_DEVICE => {
                let api::ById::<String> { id } = parse(params)?;
                done(self.account()?.remove_device(&id).await)
            }
            method::HOST_START => self.start_hosting(parse(params)?).await,
            method::HOST_STOP => self.stop_hosting().await,
            method::HOST_DECIDE => {
                let api::Decide { id, decision } = parse(params)?;
                let decision = match decision {
                    api::Decision::Control => Decision::Control,
                    api::Decision::View => Decision::ViewOnly,
                    api::Decision::Deny => Decision::Deny,
                };
                let hosting = self.hosting.lock().await;
                let host = &hosting.as_ref().ok_or_else(not_hosting)?.host;
                if host.decide(id, decision) { Ok(Value::Null) } else { Err(failed("That connection is no longer waiting.")) }
            }
            method::HOST_DISCONNECT => {
                let api::Disconnect { id } = parse(params)?;
                let hosting = self.hosting.lock().await;
                Ok(json!(hosting.as_ref().ok_or_else(not_hosting)?.host.disconnect(id)))
            }
            method::HOST_INVITE => {
                let api::NewInvite { minutes, control } = parse(params)?;
                let hosting = self.hosting.lock().await;
                let link = hosting.as_ref().ok_or_else(not_hosting)?.host.invite(minutes, control).await.map_err(failure)?;
                done(Ok(api::InviteMade { id: link.id.clone(), link: link.to_string() }))
            }
            method::HOST_UNINVITE => {
                let api::ById::<String> { id } = parse(params)?;
                let hosting = self.hosting.lock().await;
                done(hosting.as_ref().ok_or_else(not_hosting)?.host.uninvite(&id).await.map(drop))
            }
            method::HOST_TRUSTED => {
                let hosting = self.hosting.lock().await;
                done(Ok(hosting.as_ref().ok_or_else(not_hosting)?.host.trusted()))
            }
            method::HOST_REVOKE => {
                let api::Revoke { fingerprint } = parse(params)?;
                let hosting = self.hosting.lock().await;
                done(hosting.as_ref().ok_or_else(not_hosting)?.host.revoke(&fingerprint).map(drop))
            }
            method::CONNECT => done(self.viewers.open(&parse::<api::Connect>(params)?)),
            method::DISCONNECT => {
                let api::ById::<u64> { id } = parse(params)?;
                if self.viewers.close(id) { Ok(Value::Null) } else { Err(failed("That window is not open.")) }
            }
            other => Err(RpcError { code: code::METHOD_NOT_FOUND, message: format!("no method called {other:?}") }),
        }
    }

    fn state_value(&self) -> Result<Value, RpcError> {
        done(Ok(self.snapshot()))
    }

    fn account(&self) -> Result<Account, RpcError> {
        self.account.lock().unwrap().clone().ok_or_else(|| failed("Sign in first."))
    }

    async fn sign_in(&self, form: api::SignIn, create: bool) -> Result<Value, RpcError> {
        let server = Server::new(&form.server).map_err(failure)?;
        let device = DeviceInfo { name: self.device.name.clone(), fingerprint: self.device.fingerprint.clone() };
        let account = if create {
            server.create_account(&form.username, &form.password, device).await
        } else {
            server.sign_in(&form.username, &form.password, device).await
        }
        .map_err(failure)?;
        account.save(&Account::default_path()).map_err(failure)?;
        info!("signed in as {} on {}", account.username(), account.server().address());
        *self.account.lock().unwrap() = Some(account);
        self.rehost().await?;
        self.state_value()
    }

    async fn sign_out(&self) -> Result<Value, RpcError> {
        let account = self.account.lock().unwrap().take();
        if let Some(account) = account {
            // Signing out here must work even when the server can't be told.
            if let Err(e) = account.sign_out().await {
                warn!("the server wasn't told about the sign-out: {e:#}");
            }
            linuxplay_net::fsutil::remove(&Account::default_path()).map_err(failure)?;
        }
        self.rehost().await?;
        self.state_value()
    }

    async fn start_hosting(&self, options: api::StartHosting) -> Result<Value, RpcError> {
        let mut hosting = self.hosting.lock().await;
        if let Some(running) = hosting.take() {
            running.stop().await;
        }
        let account = self.account.lock().unwrap().clone();
        let running = Running::start(self.device.name.clone(), options, account).await.map_err(failure)?;
        *self.hosting_status.lock().unwrap() = Some(running.status());
        *hosting = Some(running);
        *self.hosting_options.lock().unwrap() = Some(options);
        drop(hosting);
        self.state_value()
    }

    async fn stop_hosting(&self) -> Result<Value, RpcError> {
        if let Some(running) = self.hosting.lock().await.take() {
            running.stop().await;
        }
        *self.hosting_status.lock().unwrap() = None;
        *self.hosting_options.lock().unwrap() = None;
        self.state_value()
    }

    /// The account changed: a running host has to start again to be (or stop
    /// being) reachable through it.
    async fn rehost(&self) -> Result<(), RpcError> {
        let Some(options) = *self.hosting_options.lock().unwrap() else { return Ok(()) };
        self.start_hosting(options).await.map(drop)
    }
}

fn parse<T: DeserializeOwned>(params: Value) -> Result<T, RpcError> {
    // Methods whose parameters are all optional may be called with none.
    let params = if params.is_null() { json!({}) } else { params };
    serde_json::from_value(params).map_err(|e| RpcError { code: code::INVALID_PARAMS, message: format!("wrong parameters: {e}") })
}

/// Turn a result into the API's shape.
fn done<T: serde::Serialize>(result: Result<T>) -> Result<Value, RpcError> {
    let value = result.map_err(failure)?;
    serde_json::to_value(value).map_err(|e| failure(anyhow!(e)))
}

fn failure(e: anyhow::Error) -> RpcError {
    failed(&format!("{e:#}"))
}

fn failed(message: &str) -> RpcError {
    RpcError { code: code::FAILED, message: message.to_string() }
}

fn not_hosting() -> RpcError {
    failed("This computer isn't sharing right now.")
}
