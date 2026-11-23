//! Running the host inside the engine: starting it with what the front end
//! asked for, and stopping it again.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow};
use linuxplay_host::{Host, HostConfig, TrustStore, password};
use linuxplay_net::rendezvous::Account;
use linuxplay_net::fsutil;
use linuxplay_net::netif::tailscale_addresses;
use linuxplay_net::tls::{self, Identity};
use linuxplay_proto::rendezvous;
use linuxplay_rendezvous::{App, Config, Listening, Store};
use linuxplay_proto::api;
use tokio::task::JoinHandle;
use tracing::{info, warn};

/// How long a starting host gets to fail (a port in use, another host
/// already running) before it counts as started.
const STARTUP_GRACE: Duration = Duration::from_millis(400);
/// How long a stopping host gets to end its sessions.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(3);

/// A host that is sharing this computer.
pub struct Running {
    pub host: Arc<Host>,
    task: JoinHandle<Result<()>>,
    /// The account server, when this computer is one.
    accounts: Option<Listening>,
}

impl Running {
    /// Start sharing. `account` makes the host reachable through the
    /// account's rendezvous server.
    pub async fn start(name: String, options: api::StartHosting, account: Option<Account>) -> Result<Self> {
        let dir = tls::config_dir("host");
        let mut config = HostConfig::standard(name)?;
        config.unattended = options.unattended;
        config.audio = options.audio;
        config.virtual_display = options.virtual_display;
        config.account = account;
        if options.unattended {
            // Nobody is there to read a PIN: new devices pair with the
            // password set by `linuxplay-host set-password`, if there is one.
            config.password = password::load(&password::default_path())?;
        }
        let identity = Identity::load_or_create(&dir, "LinuxPlay host")?;
        let trust = TrustStore::load(dir.join("trusted.toml"))?;
        let accounts = if options.account_server { Some(start_account_server().await?) } else { None };
        let host = Host::new(config, identity, trust).await?;
        let mut task = tokio::spawn(host.clone().run());
        match tokio::time::timeout(STARTUP_GRACE, &mut task).await {
            Err(_) => Ok(Self { host, task, accounts }),
            Ok(ended) => Err(ended.context("the host stopped unexpectedly")?.err().unwrap_or_else(|| anyhow!("the host stopped at once"))),
        }
    }

    /// What the host is doing, for the front end.
    pub fn status(&self) -> api::Hosting {
        let mut status = self.host.status();
        // The IPv4 address is the one people can type.
        status.account_server = self.accounts.as_ref().and_then(|a| {
            let addr = a.addresses.iter().find(|a| a.is_ipv4()).or(a.addresses.first())?;
            Some(format!("http://{addr}"))
        });
        status
    }

    /// Has the host stopped on its own?
    pub fn has_ended(&self) -> bool {
        self.task.is_finished()
    }

    /// Stop sharing and wait for the sessions to end.
    pub async fn stop(mut self) {
        self.accounts = None;
        self.host.stop();
        if tokio::time::timeout(SHUTDOWN_GRACE, &mut self.task).await.is_err() {
            warn!("the host did not stop in time; ending it");
            self.task.abort();
        }
    }
}

/// Serve accounts on this computer's Tailscale addresses, and nowhere else.
async fn start_account_server() -> Result<Listening> {
    let ips = tailscale_addresses().context("the account server runs over Tailscale; install Tailscale and connect it")?;
    let addresses: Vec<SocketAddr> = ips.into_iter().map(|ip| SocketAddr::new(ip, rendezvous::DEFAULT_PORT)).collect();
    let dir = tls::config_dir("accounts");
    fsutil::private_dir(&dir)?;
    let store = Store::open(&dir.join("accounts.sqlite")).context("opening the account database")?;
    let config = Config { trust_forwarded_for: false, stun_port: None, listening: addresses.clone() };
    let listening = linuxplay_rendezvous::listen(&addresses, App::new(store, config))
        .await
        .context("starting the account server (is another one already running?)")?;
    for addr in &listening.addresses {
        info!("account server on http://{addr}");
    }
    Ok(listening)
}
