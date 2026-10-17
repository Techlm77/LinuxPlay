//! The signalling connection: JSON messages on a WebSocket over mutual TLS.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, anyhow, bail};
use futures_util::{SinkExt, StreamExt};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::net::TcpStream;
use tokio_rustls::{TlsAcceptor, TlsConnector};
use tokio_tungstenite::WebSocketStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::protocol::WebSocketConfig;

use crate::tls::{Fingerprint, Identity, peer_fingerprint};

const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(10);

fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .max_message_size(Some(1 << 20))
        .max_frame_size(Some(1 << 20))
}

pub struct Signal<S> {
    ws: WebSocketStream<S>,
}

impl<S: AsyncRead + AsyncWrite + Unpin> Signal<S> {
    pub async fn send<T: Serialize>(&mut self, msg: &T) -> Result<()> {
        let text = serde_json::to_string(msg)?;
        self.ws.send(Message::text(text)).await?;
        Ok(())
    }

    /// The next message, or `None` once the peer has closed the connection.
    pub async fn recv<T: DeserializeOwned>(&mut self) -> Result<Option<T>> {
        loop {
            match self.ws.next().await {
                None | Some(Ok(Message::Close(_))) => return Ok(None),
                Some(Ok(Message::Text(text))) => {
                    return serde_json::from_str(&text)
                        .map(Some)
                        .with_context(|| format!("unexpected message: {text}"));
                }
                Some(Ok(_)) => continue,
                Some(Err(e)) => return Err(e.into()),
            }
        }
    }

    /// Like [`recv`](Self::recv), but a closed connection is an error.
    pub async fn expect<T: DeserializeOwned>(&mut self) -> Result<T> {
        self.recv().await?.ok_or_else(|| anyhow!("connection closed"))
    }

    pub async fn close(&mut self) {
        let _ = self.ws.close(None).await;
    }
}

/// An accepted, TLS-authenticated signalling connection on the host.
pub struct Accepted {
    pub signal: Signal<tokio_rustls::server::TlsStream<TcpStream>>,
    pub client_fp: Fingerprint,
    /// Our address on the interface the client reached us through.
    pub local_ip: IpAddr,
    pub peer: SocketAddr,
}

pub async fn accept(tcp: TcpStream, acceptor: &TlsAcceptor) -> Result<Accepted> {
    let local_ip = tcp.local_addr()?.ip();
    let peer = tcp.peer_addr()?;
    tcp.set_nodelay(true)?;
    let fut = async {
        let tls = acceptor.accept(tcp).await.context("TLS handshake")?;
        let client_fp = peer_fingerprint(tls.get_ref().1.peer_certificates())
            .ok_or_else(|| anyhow!("client sent no certificate"))?;
        let ws = tokio_tungstenite::accept_async_with_config(tls, Some(ws_config()))
            .await
            .context("WebSocket handshake")?;
        Ok::<_, anyhow::Error>((ws, client_fp))
    };
    let (ws, client_fp) = tokio::time::timeout(HANDSHAKE_TIMEOUT, fut)
        .await
        .map_err(|_| anyhow!("handshake timed out"))??;
    Ok(Accepted { signal: Signal { ws }, client_fp, local_ip, peer })
}

/// A connected signalling connection on the client.
pub struct Connected {
    pub signal: Signal<tokio_rustls::client::TlsStream<TcpStream>>,
    pub host_fp: Fingerprint,
    /// Our address on the interface that reaches the host.
    pub local_ip: IpAddr,
}

/// Split `host`, `host:port`, `[v6]` or `[v6]:port`, defaulting the port.
pub fn parse_address(input: &str, default_port: u16) -> Result<(String, u16)> {
    let input = input.trim();
    if input.is_empty() {
        bail!("no address given");
    }
    if let Some(rest) = input.strip_prefix('[') {
        let (host, tail) = rest.split_once(']').ok_or_else(|| anyhow!("missing ']' in {input}"))?;
        let port = match tail.strip_prefix(':') {
            Some(p) => p.parse().context("bad port")?,
            None if tail.is_empty() => default_port,
            None => bail!("unexpected text after ']' in {input}"),
        };
        return Ok((host.to_string(), port));
    }
    match input.rsplit_once(':') {
        // More than one colon without brackets: a bare IPv6 address.
        Some((host, _)) if host.contains(':') => Ok((input.to_string(), default_port)),
        Some((host, port)) => Ok((host.to_string(), port.parse().context("bad port")?)),
        None => Ok((input.to_string(), default_port)),
    }
}

pub async fn connect(host: &str, port: u16, identity: &Identity) -> Result<Connected> {
    let fut = async {
        let tcp = TcpStream::connect((host, port))
            .await
            .with_context(|| format!("connecting to {host}:{port}"))?;
        tcp.set_nodelay(true)?;
        let local_ip = tcp.local_addr()?.ip();
        let connector = TlsConnector::from(Arc::new(identity.client_config()?));
        // The name is not checked; the fingerprint is.
        let name = rustls::pki_types::ServerName::try_from("linuxplay.host")?.to_owned();
        let tls = connector.connect(name, tcp).await.context("TLS handshake")?;
        let host_fp = peer_fingerprint(tls.get_ref().1.peer_certificates())
            .ok_or_else(|| anyhow!("host sent no certificate"))?;
        let url = format!("wss://linuxplay.host:{port}/");
        let (ws, _) = tokio_tungstenite::client_async_with_config(url, tls, Some(ws_config()))
            .await
            .context("WebSocket handshake")?;
        Ok::<_, anyhow::Error>(Connected { signal: Signal { ws }, host_fp, local_ip })
    };
    tokio::time::timeout(HANDSHAKE_TIMEOUT, fut)
        .await
        .map_err(|_| anyhow!("timed out connecting to {host}:{port}"))?
}

#[cfg(test)]
mod tests {
    use super::parse_address;

    #[test]
    fn addresses() {
        let p = |s| parse_address(s, 47800).unwrap();
        assert_eq!(p("192.168.8.184"), ("192.168.8.184".into(), 47800));
        assert_eq!(p("server:1234"), ("server".into(), 1234));
        assert_eq!(p("fe80::1"), ("fe80::1".into(), 47800));
        assert_eq!(p("[fe80::1]:99"), ("fe80::1".into(), 99));
        assert_eq!(p("[::1]"), ("::1".into(), 47800));
        assert!(parse_address("", 1).is_err());
    }
}
