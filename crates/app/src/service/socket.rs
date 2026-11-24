//! The API socket: JSON-RPC 2.0, one message per line, for this user only.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use linuxplay_net::fsutil;
use linuxplay_proto::api::{self, MAX_REQUEST, Notification, Request, Response, RpcError, code, method};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;
use tracing::{debug, warn};

use super::Engine;

/// Messages waiting to be written to one client. A client that doesn't read
/// loses its connection rather than growing this without bound.
const OUTBOX: usize = 256;

/// Where the engine listens: `$XDG_RUNTIME_DIR/linuxplay/api.sock`.
pub fn socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    runtime.join("linuxplay").join(api::SOCKET_NAME)
}

/// Removes the socket file when the engine exits.
pub struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Claim the socket. Fails if another engine is already serving it.
pub async fn bind(path: &Path) -> Result<(UnixListener, SocketGuard)> {
    let dir = path.parent().context("the socket path has no folder")?;
    fsutil::private_dir(dir)?;
    if UnixStream::connect(path).await.is_ok() {
        bail!("another LinuxPlay engine is already running");
    }
    // Nobody answered, so a file there is left over from an engine that died.
    let _ = std::fs::remove_file(path);
    let listener = UnixListener::bind(path).with_context(|| format!("creating {}", path.display()))?;
    Ok((listener, SocketGuard(path.to_path_buf())))
}

/// Accept clients until the task is dropped.
pub async fn serve(listener: UnixListener, engine: Arc<Engine>) {
    // SAFETY: getuid has no failure modes and touches no memory.
    let me = unsafe { libc_getuid() };
    loop {
        match listener.accept().await {
            Ok((stream, _)) => {
                // The folder is private already; this is the second lock.
                if stream.peer_cred().map(|c| c.uid()).ok() != Some(me) {
                    warn!("refused an API connection from another user");
                    continue;
                }
                tokio::spawn(client(stream, engine.clone()));
            }
            Err(e) => {
                warn!("API socket: {e}");
                tokio::time::sleep(std::time::Duration::from_millis(200)).await;
            }
        }
    }
}

unsafe extern "C" {
    #[link_name = "getuid"]
    fn libc_getuid() -> u32;
}

/// Serve one client until it disconnects.
async fn client(stream: UnixStream, engine: Arc<Engine>) {
    let (reader, mut writer) = stream.into_split();
    let (outbox, mut queued) = mpsc::channel::<String>(OUTBOX);
    let writing = tokio::spawn(async move {
        while let Some(line) = queued.recv().await {
            if writer.write_all(line.as_bytes()).await.is_err() || writer.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });

    let mut reader = BufReader::new(reader);
    let mut subscription = None;
    loop {
        let mut line = String::new();
        // One byte over the limit tells a long line from one that just fits.
        match (&mut reader).take(MAX_REQUEST as u64 + 1).read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) if line.len() > MAX_REQUEST => {
                let _ = outbox.try_send(reply(Value::Null, Err(RpcError { code: code::INVALID_REQUEST, message: "the request is too long".into() })));
                break;
            }
            Ok(_) => {}
        }
        if line.trim().is_empty() {
            continue;
        }
        let request: Request = match serde_json::from_str(&line) {
            Ok(request) => request,
            Err(e) => {
                let error = RpcError { code: code::PARSE_ERROR, message: format!("not a JSON-RPC request: {e}") };
                if outbox.try_send(reply(Value::Null, Err(error))).is_err() {
                    break;
                }
                continue;
            }
        };
        let subscribing = request.method == method::SUBSCRIBE;
        let result = engine.call(&request.method, request.params).await;
        // A request without an id is a notification: it gets no answer.
        if let Some(id) = request.id
            && outbox.try_send(reply(id, result)).is_err() {
                break;
            }
        if subscribing && subscription.is_none() {
            subscription = Some(tokio::spawn(notify(engine.clone(), outbox.clone())));
        }
    }
    debug!("an API client left");
    if let Some(task) = subscription {
        task.abort();
    }
    drop(outbox);
    let _ = writing.await;
}

/// Send the state to a subscriber every time it changes.
async fn notify(engine: Arc<Engine>, outbox: mpsc::Sender<String>) {
    let mut changes = engine.watch();
    while changes.changed().await.is_ok() {
        let state = serde_json::to_value(&*changes.borrow_and_update()).expect("the state always serialises");
        let notification = Notification { jsonrpc: "2.0".into(), method: method::STATE_CHANGED.into(), params: state };
        let line = serde_json::to_string(&notification).expect("a notification always serialises");
        // A subscriber that has stopped reading is cut off by its full outbox.
        if outbox.try_send(line).is_err() {
            break;
        }
    }
}

fn reply(id: Value, result: Result<Value, RpcError>) -> String {
    let (result, error) = match result {
        Ok(value) => (Some(value), None),
        Err(error) => (None, Some(error)),
    };
    serde_json::to_string(&Response { jsonrpc: "2.0".into(), id, result, error }).expect("a response always serialises")
}
