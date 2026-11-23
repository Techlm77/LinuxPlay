//! A device's side of the rendezvous server: signing in, the account's
//! device list, invites, being reachable as a host, and tunnels.
//!
//! See [`linuxplay_proto::rendezvous`] for the API and for what the server
//! is trusted with (reaching devices) and what it is not (vouching for them).

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::StreamExt;
use linuxplay_proto::rendezvous::{
    API_PREFIX, DEFAULT_PORT, ApiError, Credentials, Device, DeviceInfo, HostNotice, Invite, NewInvite, ServerInfo, Session, TunnelTarget,
};
use reqwest::{Method, StatusCode, Url};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use tokio::sync::OnceCell;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;
use tokio_tungstenite::{MaybeTlsStream, WebSocketStream};

use crate::fsutil;
use crate::netif::is_tailscale;
use crate::tls::config_dir;
use crate::tunnel::WsIo;
use crate::ws::BoxIo;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(15);

type Socket = WebSocketStream<MaybeTlsStream<tokio::net::TcpStream>>;

/// A rendezvous server, by its address.
#[derive(Clone)]
pub struct Server {
    base: Url,
    http: reqwest::Client,
    /// For plain `http://` to a name: checked once, before anything is sent,
    /// that the name leads to Tailscale.
    private_route: Arc<OnceCell<()>>,
}

impl Server {
    /// `address` is where the server is: its Tailscale name or address
    /// (`office-pc`, `office-pc.tail1234.ts.net`, `100.101.102.103`, with or
    /// without `http://` and a port), or an `https://` URL.
    ///
    /// Plain `http://` is only accepted to Tailscale or this computer:
    /// passwords travel over this connection, and Tailscale encrypts it.
    pub fn new(address: &str) -> Result<Self> {
        let address = address.trim().trim_end_matches('/');
        let with_scheme =
            if address.contains("://") { address.to_string() } else { format!("http://{address}") };
        let mut base = Url::parse(&with_scheme).with_context(|| format!("{address} is not a server address"))?;
        match base.scheme() {
            "https" => {}
            "http" => {
                if base.port().is_none() {
                    base.set_port(Some(DEFAULT_PORT)).map_err(|()| anyhow!("{address} can't take a port"))?;
                }
                if let Some(refusal) = plain_http_refusal(&base) {
                    bail!(refusal);
                }
            }
            other => bail!("{other}:// is not a kind of address a LinuxPlay server has"),
        }
        let http = reqwest::Client::builder().timeout(REQUEST_TIMEOUT).build().context("setting up HTTPS")?;
        Ok(Self { base, http, private_route: Arc::default() })
    }

    /// Before the first request over plain `http://` to a name, make sure the
    /// name leads to Tailscale (or this computer), not across a local network
    /// or the internet where the password could be read.
    async fn check_route(&self) -> Result<()> {
        if self.base.scheme() != "http" || !matches!(self.base.host(), Some(url::Host::Domain(_))) {
            return Ok(());
        }
        self.private_route
            .get_or_try_init(|| async {
                let host = self.base.host_str().unwrap_or_default().to_string();
                let port = self.base.port().unwrap_or(DEFAULT_PORT);
                let found: Vec<_> = tokio::net::lookup_host((host.as_str(), port))
                    .await
                    .with_context(|| format!("can't find {host}; is Tailscale connected?"))?
                    .collect();
                if found.is_empty() || !found.iter().all(|a| is_tailscale(a.ip()) || a.ip().is_loopback()) {
                    bail!(
                        "{host} is not a Tailscale address, so signing in to it would send your password \
                         unencrypted. Use the server's Tailscale name (or https://)."
                    );
                }
                Ok(())
            })
            .await
            .map(drop)
    }

    pub fn address(&self) -> &str {
        self.base.as_str().trim_end_matches('/')
    }

    fn url(&self, path: &str) -> Url {
        let mut url = self.base.clone();
        url.set_path(&format!("{}{API_PREFIX}{path}", self.base.path().trim_end_matches('/')));
        url
    }

    async fn send<T: DeserializeOwned>(&self, request: reqwest::RequestBuilder) -> Result<T> {
        self.check_route().await?;
        let response = request.send().await.with_context(|| format!("reaching {}", self.address()))?;
        let status = response.status();
        if status.is_success() {
            return response.json().await.context("the server's answer made no sense");
        }
        Err(failure(status, response.text().await.unwrap_or_default()))
    }

    /// For requests whose success carries no body.
    async fn send_empty(&self, request: reqwest::RequestBuilder) -> Result<()> {
        self.check_route().await?;
        let response = request.send().await.with_context(|| format!("reaching {}", self.address()))?;
        let status = response.status();
        if status.is_success() { Ok(()) } else { Err(failure(status, response.text().await.unwrap_or_default())) }
    }

    pub async fn info(&self) -> Result<ServerInfo> {
        self.send(self.http.get(self.url("/info"))).await
    }

    /// Create an account and sign this device in to it.
    pub async fn create_account(&self, username: &str, password: &str, device: DeviceInfo) -> Result<Account> {
        self.start("/accounts", username, password, device).await
    }

    /// Sign this device in.
    pub async fn sign_in(&self, username: &str, password: &str, device: DeviceInfo) -> Result<Account> {
        self.start("/sessions", username, password, device).await
    }

    async fn start(&self, path: &str, username: &str, password: &str, device: DeviceInfo) -> Result<Account> {
        let body = Credentials { username: username.into(), password: password.into(), device };
        let session: Session = self.send(self.http.post(self.url(path)).json(&body)).await?;
        Ok(Account { server: self.clone(), session })
    }

    /// Ask for a tunnel to the host behind an invite. Needs no account.
    pub async fn call_invite(&self, invite_id: &str) -> Result<BoxIo> {
        self.call(&TunnelTarget::Invite(invite_id.to_string()), None).await
    }

    async fn call(&self, target: &TunnelTarget, token: Option<&str>) -> Result<BoxIo> {
        let socket = self.websocket(&format!("/tunnel?{}", target.query()), token).await?;
        Ok(Box::new(WsIo::new(socket)))
    }

    async fn websocket(&self, path_and_query: &str, token: Option<&str>) -> Result<Socket> {
        self.check_route().await?;
        let (path, query) = path_and_query.split_once('?').map_or((path_and_query, None), |(p, q)| (p, Some(q)));
        let mut url = self.url(path);
        url.set_query(query);
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme).map_err(|()| anyhow!("can't make a WebSocket address from {url}"))?;
        let mut request = url.as_str().into_client_request()?;
        if let Some(token) = token {
            request.headers_mut().insert("Authorization", format!("Bearer {token}").parse()?);
        }
        match tokio::time::timeout(REQUEST_TIMEOUT, tokio_tungstenite::connect_async(request)).await {
            Err(_) => bail!("{} did not answer", self.address()),
            Ok(Ok((socket, _))) => Ok(socket),
            Ok(Err(tokio_tungstenite::tungstenite::Error::Http(response))) => {
                let body = response.body().as_deref().map(|b| String::from_utf8_lossy(b).into_owned()).unwrap_or_default();
                Err(failure(response.status(), body))
            }
            Ok(Err(e)) => Err(anyhow!(e).context(format!("reaching {}", self.address()))),
        }
    }

    /// Where to ask for this device's public address, if the server says.
    pub async fn stun(&self) -> Option<SocketAddr> {
        let port = self.info().await.ok()?.stun_port?;
        let host = self.base.host_str()?.trim_matches(['[', ']']).to_string();
        
        tokio::net::lookup_host((host, port)).await.ok()?.next()
    }
}

/// Why plain `http://` to `url` is refused, if it is. Names are let through
/// here and checked by [`Server::check_route`] once they can be looked up.
fn plain_http_refusal(url: &Url) -> Option<String> {
    let private = match url.host()? {
        url::Host::Ipv4(ip) => ip.is_loopback() || is_tailscale(ip.into()),
        url::Host::Ipv6(ip) => ip.is_loopback() || is_tailscale(ip.into()),
        url::Host::Domain(_) => true,
    };
    (!private).then(|| {
        format!(
            "{} is not a Tailscale address, so signing in to it would send your password unencrypted. \
             Use the server's Tailscale name or address (or https://).",
            url.host_str().unwrap_or_default()
        )
    })
}

/// Turn a failed reply into an error that reads well: the server's own
/// message when it sent one.
fn failure(status: StatusCode, body: String) -> anyhow::Error {
    match serde_json::from_str::<ApiError>(&body) {
        Ok(e) => anyhow!(e.message),
        Err(_) => anyhow!("the server answered {status}"),
    }
}

/// This device, signed in.
#[derive(Clone)]
pub struct Account {
    server: Server,
    session: Session,
}

/// What is kept on disk between runs.
#[derive(Serialize, Deserialize)]
struct Saved {
    server: String,
    username: String,
    device_id: String,
    token: String,
}

impl Account {
    /// Where the sign-in is kept.
    pub fn default_path() -> PathBuf {
        config_dir("account").join("account.toml")
    }

    /// The saved sign-in, if there is one.
    pub fn load(path: &Path) -> Result<Option<Self>> {
        let Some(text) = fsutil::read_private_text(path)? else { return Ok(None) };
        let saved: Saved = toml::from_str(&text).with_context(|| format!("{} is damaged", path.display()))?;
        let session = Session { token: saved.token, device_id: saved.device_id, username: saved.username };
        Ok(Some(Self { server: Server::new(&saved.server)?, session }))
    }

    /// Keep the sign-in for the next run. The file holds the token, so it is
    /// readable by this user only.
    pub fn save(&self, path: &Path) -> Result<()> {
        if let Some(dir) = path.parent() {
            fsutil::private_dir(dir)?;
        }
        let saved = Saved {
            server: self.server.address().to_string(),
            username: self.session.username.clone(),
            device_id: self.session.device_id.clone(),
            token: self.session.token.clone(),
        };
        fsutil::write_private(path, toml::to_string_pretty(&saved)?.as_bytes())
    }

    pub fn username(&self) -> &str {
        &self.session.username
    }

    pub fn device_id(&self) -> &str {
        &self.session.device_id
    }

    pub fn server(&self) -> &Server {
        &self.server
    }

    fn request(&self, method: Method, path: &str) -> reqwest::RequestBuilder {
        self.server.http.request(method, self.server.url(path)).bearer_auth(&self.session.token)
    }

    /// The account's devices.
    pub async fn devices(&self) -> Result<Vec<Device>> {
        self.server.send(self.request(Method::GET, "/devices")).await
    }

    /// Remove a device from the account.
    pub async fn remove_device(&self, device_id: &str) -> Result<()> {
        self.server.send_empty(self.request(Method::DELETE, &format!("/devices/{device_id}"))).await
    }

    /// End this sign-in on the server.
    pub async fn sign_out(&self) -> Result<()> {
        self.server.send_empty(self.request(Method::DELETE, "/sessions/current")).await
    }

    /// Make a name by which guests can reach this host for a while.
    pub async fn create_invite(&self, valid_for: Duration) -> Result<Invite> {
        let body = NewInvite { valid_for_secs: valid_for.as_secs() };
        self.server.send(self.request(Method::POST, "/invites").json(&body)).await
    }

    pub async fn revoke_invite(&self, invite_id: &str) -> Result<()> {
        self.server.send_empty(self.request(Method::DELETE, &format!("/invites/{invite_id}"))).await
    }

    /// Ask for a tunnel to another of the account's devices.
    pub async fn call_device(&self, device_id: &str) -> Result<BoxIo> {
        self.server.call(&TunnelTarget::Device(device_id.to_string()), Some(&self.session.token)).await
    }

    /// Announce this device as a host. It stays reachable for as long as the
    /// returned presence is kept and read.
    pub async fn go_online(&self) -> Result<Presence> {
        let socket = self.server.websocket("/host", Some(&self.session.token)).await?;
        Ok(Presence { socket })
    }

    /// Pick up a tunnel a [`HostNotice`] announced.
    pub async fn accept_tunnel(&self, tunnel: &str) -> Result<BoxIo> {
        let socket = self.server.websocket(&format!("/tunnel/{tunnel}/accept"), Some(&self.session.token)).await?;
        Ok(Box::new(WsIo::new(socket)))
    }
}

/// A host's standing connection to the server.
pub struct Presence {
    socket: Socket,
}

impl Presence {
    /// The next thing the server has to say; `None` once the connection is
    /// gone (the host is then offline until it connects again).
    pub async fn next(&mut self) -> Option<HostNotice> {
        loop {
            match self.socket.next().await? {
                Ok(Message::Text(text)) => match serde_json::from_str(&text) {
                    Ok(notice) => return Some(notice),
                    // A notice from a newer server that we don't know yet.
                    Err(_) => continue,
                },
                Ok(Message::Close(_)) | Err(_) => return None,
                Ok(_) => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn base(address: &str) -> Result<String> {
        crate::init_crypto();
        Server::new(address).map(|s| s.address().to_string())
    }

    #[test]
    fn a_tailscale_name_is_enough_to_find_the_server() {
        assert_eq!(base("office-pc").unwrap(), "http://office-pc:47900");
        assert_eq!(base("office-pc.tail1234.ts.net").unwrap(), "http://office-pc.tail1234.ts.net:47900");
        assert_eq!(base("100.99.122.56").unwrap(), "http://100.99.122.56:47900");
        assert_eq!(base("http://100.99.122.56:5000/").unwrap(), "http://100.99.122.56:5000");
        assert_eq!(base("[fd7a:115c:a1e0::1]").unwrap(), "http://[fd7a:115c:a1e0::1]:47900");
        assert_eq!(base("https://play.example.org").unwrap(), "https://play.example.org");
        assert!(base("http://127.0.0.1:47900").is_ok());
    }

    #[test]
    fn plain_http_across_a_network_is_refused() {
        for unsafe_address in ["192.168.8.184", "http://203.0.113.9", "[2001:db8::1]"] {
            let error = base(unsafe_address).unwrap_err().to_string();
            assert!(error.contains("Tailscale"), "{unsafe_address}: {error}");
        }
        assert!(base("ftp://office-pc").is_err());
    }

    #[tokio::test]
    async fn a_name_is_checked_before_anything_is_sent() {
        crate::init_crypto();
        assert!(Server::new("localhost").unwrap().check_route().await.is_ok());
        // A name that can't be looked up, like one that leads to a local
        // network, is refused rather than tried.
        assert!(Server::new("nowhere.invalid").unwrap().check_route().await.is_err());
    }
}
