//! End-to-end tests of what the host enforces: a real host on loopback with
//! the built-in test capture, and clients speaking the real protocol over
//! TLS. The tests that stream need a video encoder and skip without one.

use std::collections::BTreeSet;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Once};
use std::time::Duration;

use anyhow::{Result, bail};
use gstreamer as gst;
use linuxplay_net::auth::{self, Authenticated, Hello};
use linuxplay_net::tls::{Fingerprint, Identity};
use linuxplay_net::ws::{self, Connected};
use linuxplay_net::{Channel, SessionEvent, SessionHandle, pair, rtc};
use linuxplay_proto::input::{InputEvent, MouseButton};
use linuxplay_proto::signal::{ClientMsg, HostMsg, VideoCodec};
use linuxplay_proto::{PROTOCOL_VERSION, encode};
use tokio::net::{TcpListener, TcpSocket};
use tokio::sync::{Notify, mpsc};

use crate::approval::Decision;
use crate::capture::CaptureKind;
use crate::server::{Host, HostConfig, Timeouts};
use crate::trust::TrustStore;

fn init() {
    static ONCE: Once = Once::new();
    ONCE.call_once(|| {
        gst::init().unwrap();
        linuxplay_net::init_crypto();
    });
}

struct Rig {
    host: Arc<Host>,
    addr: SocketAddr,
    dir: PathBuf,
    _quit: Arc<Notify>,
}

impl Drop for Rig {
    fn drop(&mut self) {
        self._quit.notify_one();
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

async fn rig(name: &str, unattended: bool, displays: Option<BTreeSet<u32>>) -> Rig {
    init();
    let dir = std::env::temp_dir().join(format!("linuxplay-e2e-{}-{name}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    let trust = TrustStore::load(dir.join("trusted.toml")).unwrap();
    let cfg = HostConfig {
        name: "test host".into(),
        listen: vec!["127.0.0.1".parse().unwrap()],
        exposure: "test".into(),
        port: 0,
        media_ports: None,
        capture: Some(CaptureKind::Test),
        encoder: None,
        fps: 30,
        start_kbps: 0,
        max_kbps: 0,
        audio: false,
        unattended,
        password: None,
        displays,
    };
    let mut host = Host::with_notifier(cfg, Identity::generate("host").unwrap(), trust, None).unwrap();
    Arc::get_mut(&mut host).unwrap().timeouts = Timeouts {
        hello: Duration::from_secs(2),
        pair: Duration::from_secs(5),
        pair_finish: Duration::from_millis(300),
        auth: Duration::from_secs(10),
        select: Duration::from_secs(5),
        answer: Duration::from_secs(5),
    };
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let quit = Arc::new(Notify::new());
    tokio::spawn(host.clone().accept_loop(vec![listener], quit.clone()));
    Rig { host, addr, dir, _quit: quit }
}

fn hello() -> Hello {
    Hello {
        version: PROTOCOL_VERSION,
        name: "test client".into(),
        codecs: vec![VideoCodec::H264, VideoCodec::H265, VideoCodec::Av1],
        hw_codecs: vec![],
        max_fps: 30,
        audio: false,
    }
}

/// Connect and authenticate, typing whatever PIN the host shows (or
/// refusing to pair if `pin` is false).
async fn connect(rig: &Rig, me: &Identity, pinned: Option<Fingerprint>, pin: bool) -> Result<(Connected, Authenticated)> {
    let mut conn = ws::connect("127.0.0.1", rig.addr.port(), me).await?;
    let host = rig.host.clone();
    let a = auth::authenticate(&mut conn.signal, &conn.host_fp, &me.fingerprint, pinned.as_ref(), hello(), move |_, _| {
        let host = host.clone();
        async move {
            if !pin {
                return None;
            }
            for _ in 0..200 {
                if let Some((_, pin)) = host.pins.list().into_iter().next() {
                    return Some(pin);
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
            None
        }
    })
    .await?;
    Ok((conn, a))
}

async fn next(conn: &mut Connected) -> Result<HostMsg> {
    conn.signal.expect_within(Duration::from_secs(10), "the host").await
}

/// Answer the (single) waiting approval request.
async fn decide(host: &Host, d: Decision) {
    for _ in 0..500 {
        if let Some((id, _)) = host.approver.waiting().first() {
            assert!(host.approver.resolve(*id, d));
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("nobody asked for approval");
}

#[tokio::test]
async fn an_unapproved_client_gets_no_displays() {
    let rig = rig("deny", false, None).await;
    let me = Identity::generate("client").unwrap();
    let (mut conn, auth) = connect(&rig, &me, None, true).await.unwrap();
    assert!(auth.paired);
    assert_eq!(next(&mut conn).await.unwrap(), HostMsg::AwaitingApproval);
    decide(&rig.host, Decision::Deny).await;
    match next(&mut conn).await.unwrap() {
        HostMsg::Error { message } => assert!(message.contains("declined"), "{message}"),
        m => panic!("expected a refusal, got {m:?}"),
    }
    assert!(conn.signal.recv::<HostMsg>().await.ok().flatten().is_none(), "nothing may follow a refusal");
}

#[tokio::test]
async fn view_only_approval_is_reported_to_the_client() {
    let rig = rig("view", false, None).await;
    let me = Identity::generate("client").unwrap();
    let (mut conn, _) = connect(&rig, &me, None, true).await.unwrap();
    assert_eq!(next(&mut conn).await.unwrap(), HostMsg::AwaitingApproval);
    decide(&rig.host, Decision::ViewOnly).await;
    assert_eq!(next(&mut conn).await.unwrap(), HostMsg::Approved { control: false, audio: false });
    let HostMsg::Monitors { monitors } = next(&mut conn).await.unwrap() else { panic!("expected displays") };
    assert_eq!(monitors.len(), 2);
}

#[tokio::test]
async fn a_second_window_joins_the_approved_visit() {
    let rig = rig("visit", false, None).await;
    let me = Identity::generate("client").unwrap();
    let (mut first, _) = connect(&rig, &me, None, true).await.unwrap();
    assert_eq!(next(&mut first).await.unwrap(), HostMsg::AwaitingApproval);
    decide(&rig.host, Decision::ViewOnly).await;
    assert!(matches!(next(&mut first).await.unwrap(), HostMsg::Approved { control: false, .. }));
    assert!(matches!(next(&mut first).await.unwrap(), HostMsg::Monitors { .. }));

    // Same device, while the first window is open: no new prompt, same grant.
    let (mut second, auth) = connect(&rig, &me, Some(first.host_fp), true).await.unwrap();
    assert!(!auth.paired);
    assert_eq!(next(&mut second).await.unwrap(), HostMsg::Approved { control: false, audio: false });
    assert!(rig.host.approver.waiting().is_empty());

    // Another device still has to ask.
    let other = Identity::generate("other").unwrap();
    let (mut third, _) = connect(&rig, &other, None, true).await.unwrap();
    assert_eq!(next(&mut third).await.unwrap(), HostMsg::AwaitingApproval);
}

#[tokio::test]
async fn a_display_outside_the_grant_is_refused() {
    let rig = rig("displays", true, Some([0].into())).await;
    let me = Identity::generate("client").unwrap();
    let (mut conn, _) = connect(&rig, &me, None, true).await.unwrap();
    assert!(matches!(next(&mut conn).await.unwrap(), HostMsg::Approved { control: true, .. }));
    let HostMsg::Monitors { monitors } = next(&mut conn).await.unwrap() else { panic!("expected displays") };
    assert_eq!(monitors.iter().map(|m| m.index).collect::<Vec<_>>(), vec![0]);
    conn.signal.send(&ClientMsg::SelectMonitor { index: 1 }).await.unwrap();
    match next(&mut conn).await.unwrap() {
        HostMsg::Error { message } => assert!(message.contains("isn't shared"), "{message}"),
        m => panic!("expected a refusal, got {m:?}"),
    }
}

#[tokio::test]
async fn a_stalled_pair_finish_times_out_trusts_nothing_and_frees_the_slot() {
    let rig = rig("stall", false, None).await;
    let me = Identity::generate("client").unwrap();
    let mut conn = ws::connect("127.0.0.1", rig.addr.port(), &me).await.unwrap();
    let h = hello();
    conn.signal
        .send(&ClientMsg::Hello {
            version: h.version,
            name: h.name,
            codecs: h.codecs,
            hw_codecs: h.hw_codecs,
            max_fps: h.max_fps,
            audio: h.audio,
            pair: true,
        })
        .await
        .unwrap();
    assert!(matches!(next(&mut conn).await.unwrap(), HostMsg::PairingRequired { .. }));
    let (_, start) = pair::client_start("000000");
    conn.signal.send(&ClientMsg::PairStart { spake: start }).await.unwrap();
    assert!(matches!(next(&mut conn).await.unwrap(), HostMsg::PairReply { .. }));
    // ...and never send PairFinish.
    tokio::time::sleep(Duration::from_millis(800)).await;
    assert!(!rig.host.is_trusted(&me.fingerprint));
    assert!(rig.host.pins.list().is_empty(), "the PIN is taken down when pairing ends");

    // The pairing slot for this source is free again.
    let (_, auth) = connect(&rig, &me, None, true).await.unwrap();
    assert!(auth.paired);
}

#[tokio::test]
async fn stalled_handshakes_cannot_lock_out_a_paired_client() {
    let rig = rig("flood", true, None).await;
    let me = Identity::generate("client").unwrap();
    let (first, _) = connect(&rig, &me, None, true).await.unwrap();
    let host_fp = first.host_fp;
    drop(first);

    // Another machine opens many connections and never speaks.
    let mut stalled = Vec::new();
    for _ in 0..32 {
        let s = TcpSocket::new_v4().unwrap();
        s.bind("127.0.0.2:0".parse().unwrap()).unwrap();
        if let Ok(c) = s.connect(rig.addr).await {
            stalled.push(c);
        }
    }
    tokio::time::sleep(Duration::from_millis(200)).await;
    let (mut conn, auth) = connect(&rig, &me, Some(host_fp), true).await.unwrap();
    assert!(!auth.paired);
    assert!(matches!(next(&mut conn).await.unwrap(), HostMsg::Approved { .. }));
}

/// Has this machine an encoder the test capture can feed?
fn can_stream() -> bool {
    init();
    let ok = !crate::video::available().is_empty();
    if !ok {
        eprintln!("skipping: no video encoder installed");
    }
    ok
}

/// Approve (if asked), pick display 0 and bring the WebRTC session up.
/// Returns the session and a channel of its events.
async fn start_stream(
    rig: &Rig,
    conn: &mut Connected,
    decision: Decision,
) -> Result<(SessionHandle, mpsc::UnboundedReceiver<SessionEvent>, bool)> {
    let mut msg = next(conn).await?;
    if msg == HostMsg::AwaitingApproval {
        decide(&rig.host, decision).await;
        msg = next(conn).await?;
    }
    let HostMsg::Approved { control, .. } = msg else { bail!("expected approval, got {msg:?}") };
    let HostMsg::Monitors { .. } = next(conn).await? else { bail!("expected displays") };
    conn.signal.send(&ClientMsg::SelectMonitor { index: 0 }).await?;
    let msg = next(conn).await?;
    let HostMsg::Offer { sdp, .. } = msg else { bail!("expected an offer, got {msg:?}") };
    let (tx, rx) = mpsc::unbounded_channel();
    let handler = Box::new(move |ev: SessionEvent| match ev {
        SessionEvent::Video { .. } | SessionEvent::Audio { .. } => {}
        other => drop(tx.send(other)),
    });
    let (session, answer) = rtc::accept_offer(conn.local_ip, &sdp, handler)?;
    conn.signal.send(&ClientMsg::Answer { sdp: answer }).await?;
    Ok((session, rx, control))
}

async fn wait_for_input_channel(rx: &mut mpsc::UnboundedReceiver<SessionEvent>) {
    let fut = async {
        while let Some(ev) = rx.recv().await {
            if matches!(ev, SessionEvent::ChannelOpen(Channel::Input)) {
                return;
            }
        }
        panic!("session ended before the input channel opened");
    };
    tokio::time::timeout(Duration::from_secs(15), fut).await.expect("input channel never opened");
}

fn some_input() -> Vec<InputEvent> {
    vec![
        InputEvent::PointerAbs { x: 100, y: 100 },
        InputEvent::Button { button: MouseButton::Left, pressed: true },
        InputEvent::Button { button: MouseButton::Left, pressed: false },
        InputEvent::Key { code: 30, pressed: true },
        InputEvent::Key { code: 30, pressed: false },
        InputEvent::Scroll { dx: 0, dy: i32::MAX },
    ]
}

#[tokio::test(flavor = "multi_thread")]
async fn a_view_only_session_never_injects_input() {
    if !can_stream() {
        return;
    }
    let rig = rig("viewonly-media", false, None).await;
    let me = Identity::generate("client").unwrap();
    let (mut conn, _) = connect(&rig, &me, None, true).await.unwrap();
    let (session, mut events, control) = start_stream(&rig, &mut conn, Decision::ViewOnly).await.unwrap();
    assert!(!control);
    wait_for_input_channel(&mut events).await;
    for _ in 0..20 {
        for ev in some_input() {
            session.send(Channel::Input, encode(&ev));
        }
    }
    tokio::time::sleep(Duration::from_millis(700)).await;
    assert!(rig.host.test_input.lock().unwrap().is_empty(), "a view-only session injected input");
    session.close();
}

#[tokio::test(flavor = "multi_thread")]
async fn revoking_a_device_ends_its_live_session_and_its_access() {
    if !can_stream() {
        return;
    }
    let rig = rig("revoke-media", false, None).await;
    let me = Identity::generate("client").unwrap();
    let (mut conn, _) = connect(&rig, &me, None, true).await.unwrap();
    let host_fp = conn.host_fp;
    let (session, mut events, control) = start_stream(&rig, &mut conn, Decision::Control).await.unwrap();
    assert!(control);
    wait_for_input_channel(&mut events).await;

    // Controls work while approved; hostile values arrive clamped.
    for ev in some_input() {
        session.send(Channel::Input, encode(&ev));
    }
    let injected = |rig: &Rig| rig.host.test_input.lock().unwrap().len();
    for _ in 0..200 {
        if injected(&rig) >= 6 {
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(rig.host.test_input.lock().unwrap().contains(&InputEvent::Key { code: 30, pressed: true }));
    assert!(rig.host.test_input.lock().unwrap().contains(&InputEvent::Scroll { dx: 0, dy: crate::input::MAX_SCROLL }));

    // `linuxplay-host revoke` runs as another process, then tells the host.
    let mut cli = TrustStore::load(rig.dir.join("trusted.toml")).unwrap();
    assert_eq!(cli.revoke(&hex::encode(me.fingerprint)[..16]).unwrap().len(), 1);
    assert_eq!(rig.host.enforce_trust(true), 1);

    let before = injected(&rig);
    session.send(Channel::Input, encode(&InputEvent::Key { code: 31, pressed: true }));
    // The signalling connection ends with a reason.
    let ended = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match conn.signal.recv::<HostMsg>().await {
                Ok(Some(HostMsg::Error { message })) => return Some(message),
                Ok(Some(_)) => continue,
                Ok(None) | Err(_) => return None,
            }
        }
    })
    .await
    .expect("the session must end promptly");
    assert!(ended.is_some_and(|m| m.contains("ended")));
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(injected(&rig), before, "no input after revocation");
    for _ in 0..100 {
        if rig.host.sessions.is_empty() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(rig.host.sessions.is_empty());
    session.close();

    // Reconnecting without pairing again is refused.
    let Err(err) = connect(&rig, &me, Some(host_fp), false).await else { panic!("a revoked device got back in") };
    assert!(format!("{err:#}").contains("pairing cancelled"), "{err:#}");
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_sharing_ends_every_session() {
    if !can_stream() {
        return;
    }
    let rig = rig("stop-media", true, None).await;
    let me = Identity::generate("client").unwrap();
    let (mut conn, _) = connect(&rig, &me, None, true).await.unwrap();
    let (session, mut events, _) = start_stream(&rig, &mut conn, Decision::Control).await.unwrap();
    wait_for_input_channel(&mut events).await;
    assert_eq!(rig.host.sessions.kill_all(), 1);
    let closed = tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            match conn.signal.recv::<HostMsg>().await {
                Ok(Some(HostMsg::Error { .. })) | Ok(None) | Err(_) => return,
                Ok(Some(_)) => continue,
            }
        }
    })
    .await;
    assert!(closed.is_ok());
    session.close();
}
