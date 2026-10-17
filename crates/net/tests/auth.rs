//! First-pairing and reconnect authentication against mock hosts over real
//! TLS and WebSocket on loopback. A host that can't prove itself must never
//! be accepted, so it can never be pinned.

use std::future::Future;
use std::sync::{Arc, Once};

use anyhow::{Result, bail};
use linuxplay_net::auth::{self, Authenticated, Hello};
use linuxplay_net::tls::{Fingerprint, Identity};
use linuxplay_net::ws::{self, Accepted};
use linuxplay_net::pair;
use linuxplay_proto::PROTOCOL_VERSION;
use linuxplay_proto::signal::{ClientMsg, HostMsg, VideoCodec};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

const PIN: &str = "424242";

fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(linuxplay_net::init_crypto);
}

fn hello() -> Hello {
    Hello {
        version: PROTOCOL_VERSION,
        name: "test client".into(),
        codecs: vec![VideoCodec::H264],
        hw_codecs: vec![],
        max_fps: 60,
        audio: false,
    }
}

/// Listen on loopback and run `script` for the first connection.
async fn host<F, Fut>(identity: Identity, script: F) -> (u16, Fingerprint, JoinHandle<Result<()>>)
where
    F: FnOnce(Accepted, Arc<Identity>) -> Fut + Send + 'static,
    Fut: Future<Output = Result<()>> + Send,
{
    let identity = Arc::new(identity);
    let acceptor = TlsAcceptor::from(Arc::new(identity.server_config().unwrap()));
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let fp = identity.fingerprint;
    let task = tokio::spawn(async move {
        let (tcp, _) = listener.accept().await?;
        let acc = ws::accept(tcp, &acceptor).await?;
        script(acc, identity).await
    });
    (port, fp, task)
}

/// The honest host side of PIN pairing, then `Welcome`.
async fn honest(mut acc: Accepted, me: Arc<Identity>, pin: &str) -> Result<()> {
    let ClientMsg::Hello { .. } = acc.signal.expect().await? else { bail!("expected Hello") };
    acc.signal.send(&HostMsg::PairingRequired { host_name: "real".into(), password: false }).await?;
    let ClientMsg::PairStart { spake } = acc.signal.expect().await? else { bail!("expected PairStart") };
    let (state, reply, mac) = pair::host_respond(pin, &spake, &me.fingerprint, &acc.client_fp)?;
    acc.signal.send(&HostMsg::PairReply { spake: reply, mac }).await?;
    let ClientMsg::PairFinish { mac } = acc.signal.expect().await? else { bail!("expected PairFinish") };
    state.verify(&mac)?;
    acc.signal.send(&HostMsg::Welcome { host_name: "real".into() }).await?;
    Ok(())
}

async fn client(port: u16, pinned: Option<Fingerprint>, pin: &str) -> Result<Authenticated> {
    let me = Identity::generate("client")?;
    let mut conn = ws::connect("127.0.0.1", port, &me).await?;
    let pin = pin.to_string();
    auth::authenticate(&mut conn.signal, &conn.host_fp, &me.fingerprint, pinned.as_ref(), hello(), |_, _| {
        std::future::ready(Some(pin.clone()))
    })
    .await
}

#[tokio::test]
async fn unsolicited_welcome_from_unknown_host_is_refused() {
    init();
    let (port, _, task) = host(Identity::generate("impostor").unwrap(), |mut acc, _| async move {
        let _hello: ClientMsg = acc.signal.expect().await?;
        acc.signal.send(&HostMsg::Welcome { host_name: "trust me".into() }).await?;
        let _ = acc.signal.recv::<ClientMsg>().await;
        Ok(())
    })
    .await;
    let err = client(port, None, PIN).await.unwrap_err();
    assert!(format!("{err:#}").contains("skipped pairing"), "{err:#}");
    let _ = task.await;
}

#[tokio::test]
async fn welcome_before_pairing_completes_is_refused() {
    init();
    let (port, _, _task) = host(Identity::generate("impostor").unwrap(), |mut acc, _| async move {
        let _hello: ClientMsg = acc.signal.expect().await?;
        acc.signal.send(&HostMsg::PairingRequired { host_name: "x".into(), password: false }).await?;
        let _start: ClientMsg = acc.signal.expect().await?;
        acc.signal.send(&HostMsg::Welcome { host_name: "x".into() }).await?;
        Ok(())
    })
    .await;
    assert!(client(port, None, PIN).await.is_err());
}

#[tokio::test]
async fn pair_reply_without_pairing_required_is_refused() {
    init();
    let (port, _, _task) = host(Identity::generate("impostor").unwrap(), |mut acc, _| async move {
        let _hello: ClientMsg = acc.signal.expect().await?;
        acc.signal.send(&HostMsg::PairReply { spake: "AAAA".into(), mac: "00".into() }).await?;
        Ok(())
    })
    .await;
    assert!(client(port, None, PIN).await.is_err());
}

#[tokio::test]
async fn correct_pin_pairs() {
    init();
    let (port, _, task) = host(Identity::generate("host").unwrap(), |acc, me| honest(acc, me, PIN)).await;
    let ok = client(port, None, PIN).await.unwrap();
    assert_eq!(ok, Authenticated { host_name: "real".into(), paired: true });
    task.await.unwrap().unwrap();
}

#[tokio::test]
async fn wrong_pin_fails_on_both_sides() {
    init();
    let (port, _, task) = host(Identity::generate("host").unwrap(), |acc, me| honest(acc, me, PIN)).await;
    assert!(client(port, None, "000000").await.is_err());
    assert!(task.await.unwrap().is_err(), "the host must not accept a wrong PIN either");
}

#[tokio::test]
async fn pinned_host_reconnects_without_pairing() {
    init();
    let (port, fp, _task) = host(Identity::generate("host").unwrap(), |mut acc, _| async move {
        let ClientMsg::Hello { pair: false, .. } = acc.signal.expect().await? else { bail!("expected Hello without pairing") };
        acc.signal.send(&HostMsg::Welcome { host_name: "known".into() }).await?;
        Ok(())
    })
    .await;
    let ok = client(port, Some(fp), PIN).await.unwrap();
    assert!(!ok.paired);
}

#[tokio::test]
async fn changed_host_certificate_is_refused_before_anything_is_sent() {
    init();
    let (port, _, task) = host(Identity::generate("host").unwrap(), |mut acc, _| async move {
        match acc.signal.recv::<ClientMsg>().await {
            Ok(None) | Err(_) => Ok(()),
            Ok(Some(m)) => bail!("client sent {m:?} to a host whose certificate changed"),
        }
    })
    .await;
    let err = client(port, Some([7; 32]), PIN).await.unwrap_err();
    assert!(format!("{err:#}").contains("identity has changed"));
    task.await.unwrap().unwrap();
}

/// A relay in the middle, which shows each side its own certificate, can't
/// complete pairing even when the user types the real host's PIN into it.
#[tokio::test]
async fn man_in_the_middle_cannot_pair() {
    init();
    let (real_port, _, real) = host(Identity::generate("host").unwrap(), |acc, me| honest(acc, me, PIN)).await;
    let (proxy_port, _, _proxy) = host(Identity::generate("mitm").unwrap(), move |mut down, _| async move {
        let mitm_client = Identity::generate("mitm client")?;
        let mut up = ws::connect("127.0.0.1", real_port, &mitm_client).await?.signal;
        loop {
            tokio::select! {
                m = down.signal.recv::<serde_json::Value>() => match m? {
                    Some(m) => up.send(&m).await?,
                    None => return Ok(()),
                },
                m = up.recv::<serde_json::Value>() => match m? {
                    Some(m) => down.signal.send(&m).await?,
                    None => return Ok(()),
                },
            }
        }
    })
    .await;
    let err = client(proxy_port, None, PIN).await.unwrap_err();
    assert!(format!("{err:#}").contains("intercepted") || format!("{err:#}").contains("pairing failed"), "{err:#}");
    assert!(real.await.unwrap().is_err(), "the real host must not trust the relay");
}
