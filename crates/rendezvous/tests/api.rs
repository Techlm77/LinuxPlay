//! The server end to end, over real sockets: accounts, device lists, invites
//! and tunnels.

use std::net::SocketAddr;

use futures_util::{SinkExt, StreamExt};
use linuxplay_proto::rendezvous::{ApiError, Caller, Credentials, Device, DeviceInfo, HostNotice, Invite, NewInvite, Session};
use linuxplay_rendezvous::{App, Config, Store};
use serde::Serialize;
use serde::de::DeserializeOwned;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;
use tokio_tungstenite::tungstenite::client::IntoClientRequest;

type Socket = tokio_tungstenite::WebSocketStream<TcpStream>;

const PASSWORD: &str = "a long enough password";

async fn server() -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let app = App::new(Store::in_memory().unwrap(), Config::default());
    tokio::spawn(linuxplay_rendezvous::serve(listener, app));
    addr
}

/// One HTTP request; returns the status and the body.
async fn request(addr: SocketAddr, method: &str, path: &str, token: Option<&str>, body: Option<String>) -> (u16, String) {
    let mut stream = TcpStream::connect(addr).await.unwrap();
    let body = body.unwrap_or_default();
    let auth = token.map(|t| format!("Authorization: Bearer {t}\r\n")).unwrap_or_default();
    let head = format!(
        "{method} /v1{path} HTTP/1.1\r\nHost: test\r\nConnection: close\r\nContent-Type: application/json\r\n\
         Content-Length: {}\r\n{auth}\r\n",
        body.len()
    );
    stream.write_all(format!("{head}{body}").as_bytes()).await.unwrap();
    let mut reply = String::new();
    stream.read_to_string(&mut reply).await.unwrap();
    let status = reply.split_whitespace().nth(1).unwrap().parse().unwrap();
    let body = reply.split_once("\r\n\r\n").map(|(_, b)| b.to_string()).unwrap_or_default();
    (status, body)
}

async fn post<T: Serialize, R: DeserializeOwned>(addr: SocketAddr, path: &str, token: Option<&str>, body: &T) -> Result<R, (u16, ApiError)> {
    let (status, text) = request(addr, "POST", path, token, Some(serde_json::to_string(body).unwrap())).await;
    if status < 300 { Ok(serde_json::from_str(&text).unwrap()) } else { Err((status, serde_json::from_str(&text).unwrap())) }
}

fn credentials(username: &str, device: &str, fingerprint_digit: char) -> Credentials {
    Credentials {
        username: username.into(),
        password: PASSWORD.into(),
        device: DeviceInfo { name: device.into(), fingerprint: fingerprint_digit.to_string().repeat(64) },
    }
}

async fn devices(addr: SocketAddr, token: &str) -> Vec<Device> {
    let (status, text) = request(addr, "GET", "/devices", Some(token), None).await;
    assert_eq!(status, 200, "{text}");
    serde_json::from_str(&text).unwrap()
}

async fn websocket(addr: SocketAddr, path: &str, token: Option<&str>) -> Result<Socket, u16> {
    let mut req = format!("ws://{addr}/v1{path}").into_client_request().unwrap();
    if let Some(token) = token {
        req.headers_mut().insert("Authorization", format!("Bearer {token}").parse().unwrap());
    }
    let stream = TcpStream::connect(addr).await.unwrap();
    match tokio_tungstenite::client_async(req, stream).await {
        Ok((socket, _)) => Ok(socket),
        Err(tokio_tungstenite::tungstenite::Error::Http(response)) => Err(response.status().as_u16()),
        Err(e) => panic!("{e}"),
    }
}

/// The next binary message, skipping keepalives.
async fn next_bytes(socket: &mut Socket) -> Vec<u8> {
    loop {
        match socket.next().await.unwrap().unwrap() {
            Message::Binary(bytes) => return bytes.to_vec(),
            Message::Ping(_) | Message::Pong(_) => continue,
            other => panic!("unexpected {other:?}"),
        }
    }
}

async fn next_notice(host: &mut Socket) -> HostNotice {
    loop {
        match host.next().await.unwrap().unwrap() {
            Message::Text(text) => return serde_json::from_str(&text).unwrap(),
            _ => continue,
        }
    }
}

#[tokio::test]
async fn accounts_and_sign_in() {
    let addr = server().await;
    let made: Session = post(addr, "/accounts", None, &credentials("dominic", "Desktop", 'a')).await.unwrap();
    assert_eq!(made.username, "dominic");

    let taken = post::<_, Session>(addr, "/accounts", None, &credentials("Dominic", "Other", 'b')).await.unwrap_err();
    assert_eq!((taken.0, taken.1.error.as_str()), (409, "conflict"));

    let mut weak = credentials("someone", "PC", 'c');
    weak.password = "short".into();
    assert_eq!(post::<_, Session>(addr, "/accounts", None, &weak).await.unwrap_err().0, 400);

    // A wrong password and an unknown user are answered the same way.
    let mut wrong = credentials("dominic", "Desktop", 'a');
    wrong.password = "not the password at all".into();
    let (status, error) = post::<_, Session>(addr, "/sessions", None, &wrong).await.unwrap_err();
    let unknown = post::<_, Session>(addr, "/sessions", None, &credentials("nobody", "PC", 'd')).await.unwrap_err();
    assert_eq!((status, &error), (401, &unknown.1));

    // Signing in on a second device lists both; the first token still works.
    let laptop: Session = post(addr, "/sessions", None, &credentials("dominic", "Laptop", 'b')).await.unwrap();
    let list = devices(addr, &laptop.token).await;
    assert_eq!(list.iter().map(|d| d.name.as_str()).collect::<Vec<_>>(), ["Desktop", "Laptop"]);
    assert!(list.iter().find(|d| d.name == "Laptop").unwrap().current);
    assert!(list.iter().all(|d| !d.online));
    assert_eq!(devices(addr, &made.token).await.len(), 2);

    // Signing out ends the token; removing a device ends its token too.
    assert_eq!(request(addr, "DELETE", "/sessions/current", Some(&laptop.token), None).await.0, 204);
    assert_eq!(request(addr, "GET", "/devices", Some(&laptop.token), None).await.0, 401);
    assert_eq!(request(addr, "GET", "/devices", None, None).await.0, 401);
    let laptop: Session = post(addr, "/sessions", None, &credentials("dominic", "Laptop", 'b')).await.unwrap();
    let path = format!("/devices/{}", made.device_id);
    assert_eq!(request(addr, "DELETE", &path, Some(&laptop.token), None).await.0, 204);
    assert_eq!(request(addr, "GET", "/devices", Some(&made.token), None).await.0, 401);
}

#[tokio::test]
async fn sign_in_attempts_are_limited() {
    let addr = server().await;
    let _: Session = post(addr, "/accounts", None, &credentials("target", "PC", 'a')).await.unwrap();
    let mut wrong = credentials("target", "PC", 'a');
    wrong.password = "guessing guessing".into();
    let mut statuses = Vec::new();
    for _ in 0..12 {
        statuses.push(post::<_, Session>(addr, "/sessions", None, &wrong).await.unwrap_err().0);
    }
    assert!(statuses[..5].iter().all(|s| *s == 401));
    assert_eq!(statuses.last(), Some(&429));
    // The right password is refused too while the limit holds.
    assert_eq!(post::<_, Session>(addr, "/sessions", None, &credentials("target", "PC", 'a')).await.unwrap_err().0, 429);
}

#[tokio::test]
async fn a_device_reaches_its_accounts_host_through_a_tunnel() {
    let addr = server().await;
    let host: Session = post(addr, "/accounts", None, &credentials("owner", "Server", 'a')).await.unwrap();
    let client: Session = post(addr, "/sessions", None, &credentials("owner", "Laptop", 'b')).await.unwrap();
    let stranger: Session = post(addr, "/accounts", None, &credentials("stranger", "PC", 'c')).await.unwrap();
    let call = format!("/tunnel?device={}", host.device_id);

    // Not hosting yet: offline.
    assert_eq!(websocket(addr, &call, Some(&client.token)).await.unwrap_err(), 503);

    let mut presence = websocket(addr, "/host", Some(&host.token)).await.unwrap();
    // Give the server a moment to register the presence.
    for _ in 0..50 {
        if devices(addr, &client.token).await.iter().any(|d| d.online) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    assert!(devices(addr, &client.token).await.iter().find(|d| d.name == "Server").unwrap().online);

    // Another account can't reach it, and can't tell it exists.
    assert_eq!(websocket(addr, &call, Some(&stranger.token)).await.unwrap_err(), 404);
    assert_eq!(websocket(addr, &call, None).await.unwrap_err(), 401);

    let mut caller = websocket(addr, &call, Some(&client.token)).await.unwrap();
    let HostNotice::Connect { tunnel, from } = next_notice(&mut presence).await;
    assert_eq!(from, Caller::Device { id: client.device_id.clone(), name: "Laptop".into(), fingerprint: "b".repeat(64) });

    // Only the called host can pick the tunnel up.
    let accept = format!("/tunnel/{tunnel}/accept");
    assert_eq!(websocket(addr, &accept, Some(&client.token)).await.unwrap_err(), 404);
    let mut answered = websocket(addr, &accept, Some(&host.token)).await.unwrap();

    caller.send(Message::Binary(b"hello host".to_vec().into())).await.unwrap();
    answered.send(Message::Binary(b"hello caller".to_vec().into())).await.unwrap();
    assert_eq!(next_bytes(&mut answered).await, b"hello host");
    assert_eq!(next_bytes(&mut caller).await, b"hello caller");

    // Closing one end closes the other.
    caller.close(None).await.unwrap();
    let ended = tokio::time::timeout(std::time::Duration::from_secs(5), async {
        loop {
            match answered.next().await {
                None | Some(Err(_)) | Some(Ok(Message::Close(_))) => break,
                Some(Ok(_)) => continue,
            }
        }
    });
    ended.await.expect("the host's end closes when the caller leaves");
    // A used tunnel can't be picked up again.
    assert_eq!(websocket(addr, &accept, Some(&host.token)).await.unwrap_err(), 404);
}

#[tokio::test]
async fn a_guest_reaches_a_host_by_invite_without_an_account() {
    let addr = server().await;
    let host: Session = post(addr, "/accounts", None, &credentials("inviter", "Gaming PC", 'a')).await.unwrap();
    let mut presence = websocket(addr, "/host", Some(&host.token)).await.unwrap();
    let invite: Invite = post(addr, "/invites", Some(&host.token), &NewInvite { valid_for_secs: 600 }).await.unwrap();
    assert!(post::<_, Invite>(addr, "/invites", Some(&host.token), &NewInvite { valid_for_secs: 0 }).await.is_err());

    assert_eq!(websocket(addr, "/tunnel?invite=does-not-exist", None).await.unwrap_err(), 404);
    assert_eq!(websocket(addr, "/tunnel", None).await.unwrap_err(), 400);

    let call = format!("/tunnel?invite={}", invite.id);
    let mut guest = websocket(addr, &call, None).await.unwrap();
    let HostNotice::Connect { tunnel, from } = next_notice(&mut presence).await;
    assert_eq!(from, Caller::Invite { id: invite.id.clone() });
    let mut answered = websocket(addr, &format!("/tunnel/{tunnel}/accept"), Some(&host.token)).await.unwrap();
    guest.send(Message::Binary(vec![1, 2, 3].into())).await.unwrap();
    assert_eq!(next_bytes(&mut answered).await, [1, 2, 3]);

    // Once withdrawn, the invite leads nowhere.
    assert_eq!(request(addr, "DELETE", &format!("/invites/{}", invite.id), Some(&host.token), None).await.0, 204);
    assert_eq!(websocket(addr, &call, None).await.unwrap_err(), 404);
}
