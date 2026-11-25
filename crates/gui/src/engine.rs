//! The connection to the engine (`linuxplay service`), which does the real
//! work: this window only shows its state and asks it to do things, over the
//! local API in [`linuxplay_proto::api`].
//!
//! A reader thread turns the engine's lines into messages for the main loop;
//! requests are written from the main loop and matched to their answers by id.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::fs::File;
use std::io::{BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};
use std::rc::Rc;
use std::time::{Duration, Instant};

use gtk::{gio, glib};
use linuxplay_proto::api::{self, Notification, Request, Response, method};
use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;

/// How long a newly started engine has to open its socket.
const START_TIMEOUT: Duration = Duration::from_secs(15);
/// How long a stopping engine gets to end its sessions cleanly.
const STOP_TIMEOUT: Duration = Duration::from_secs(5);

/// Where the engine listens: `$XDG_RUNTIME_DIR/linuxplay/api.sock`.
fn socket_path() -> PathBuf {
    let runtime = std::env::var_os("XDG_RUNTIME_DIR").map(PathBuf::from).unwrap_or_else(std::env::temp_dir);
    runtime.join("linuxplay").join(api::SOCKET_NAME)
}

/// The log of an engine this window started.
pub fn log_path() -> PathBuf {
    glib::user_cache_dir().join("linuxplay").join("engine.log")
}

enum Incoming {
    Response(Response),
    State(Box<api::State>),
    Closed,
}

/// A connected engine. Cheap to clone; every clone is the same connection.
#[derive(Clone)]
pub struct Engine(Rc<Inner>);

struct Inner {
    writer: RefCell<UnixStream>,
    next_id: Cell<u64>,
    pending: RefCell<HashMap<u64, async_channel::Sender<Result<Value, String>>>>,
    /// The engine, when this window started it (and so stops it).
    child: RefCell<Option<Child>>,
}

/// A socket to the engine, and the engine itself if it had to be started.
pub struct Link {
    stream: UnixStream,
    child: Option<Child>,
}

/// Reach the running engine, or start one and wait for it. Runs off the main
/// thread, since starting one takes a moment.
pub async fn link() -> Result<Link, String> {
    gio::spawn_blocking(link_blocking).await.unwrap_or_else(|_| Err("Starting the LinuxPlay engine failed.".into()))
}

fn link_blocking() -> Result<Link, String> {
    let path = socket_path();
    if let Ok(stream) = UnixStream::connect(&path) {
        return Ok(Link { stream, child: None });
    }
    let mut child = start_engine()?;
    let deadline = Instant::now() + START_TIMEOUT;
    loop {
        std::thread::sleep(Duration::from_millis(100));
        if let Ok(stream) = UnixStream::connect(&path) {
            return Ok(Link { stream, child: Some(child) });
        }
        let stopped = child.try_wait().ok().flatten();
        if stopped.is_some() || Instant::now() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err(format!("The LinuxPlay engine didn't start. What it said is in {}.", log_path().display()));
        }
    }
}

/// `linuxplay service`, from next to this program (the Flatpak and the
/// AppImage keep both in one folder), else from the PATH.
fn start_engine() -> Result<Child, String> {
    let beside = std::env::current_exe().ok().and_then(|p| p.parent().map(|d| d.join("linuxplay"))).filter(|p| p.exists());
    let program = beside.unwrap_or_else(|| "linuxplay".into());
    let log = log_path();
    if let Some(dir) = log.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let out = File::create(&log).map_err(|e| format!("Can't write {}: {e}", log.display()))?;
    let err = out.try_clone().map_err(|e| e.to_string())?;
    Command::new(&program)
        .arg("service")
        .stdin(Stdio::null())
        .stdout(out)
        .stderr(err)
        .spawn()
        .map_err(|e| format!("Can't start {}: {e}", program.display()))
}

impl Engine {
    /// Start talking to the engine behind `link`. `on_state` gets every new
    /// state, `on_closed` is called once if the engine goes away.
    pub fn new(link: Link, on_state: impl Fn(api::State) + 'static, on_closed: impl FnOnce() + 'static) -> Result<Self, String> {
        let reader = link.stream.try_clone().map_err(|e| e.to_string())?;
        let engine = Self(Rc::new(Inner {
            writer: RefCell::new(link.stream),
            next_id: Cell::new(1),
            pending: RefCell::new(HashMap::new()),
            child: RefCell::new(link.child),
        }));
        let (tx, rx) = async_channel::unbounded();
        std::thread::spawn(move || read(reader, tx));
        let weak = Rc::downgrade(&engine.0);
        glib::spawn_future_local(async move {
            while let Ok(message) = rx.recv().await {
                match message {
                    Incoming::State(state) => on_state(*state),
                    Incoming::Response(response) => {
                        let Some(inner) = weak.upgrade() else { break };
                        let Some(id) = response.id.as_u64() else { continue };
                        if let Some(reply) = inner.pending.borrow_mut().remove(&id) {
                            let result = match response.error {
                                Some(error) => Err(error.message),
                                None => Ok(response.result.unwrap_or(Value::Null)),
                            };
                            let _ = reply.try_send(result);
                        }
                    }
                    Incoming::Closed => break,
                }
            }
            // Whoever is still waiting for an answer gets an error.
            if let Some(inner) = weak.upgrade() {
                inner.pending.borrow_mut().clear();
            }
            on_closed();
        });
        Ok(engine)
    }

    /// Call `name` with `params` (`()` for none) and read the result as `T`.
    pub async fn call<T: DeserializeOwned>(&self, name: &str, params: impl Serialize) -> Result<T, String> {
        let id = self.0.next_id.get();
        self.0.next_id.set(id + 1);
        let params = serde_json::to_value(params).map_err(|e| e.to_string())?;
        let mut line = serde_json::to_string(&Request::new(id, name, params)).map_err(|e| e.to_string())?;
        line.push('\n');
        let (tx, rx) = async_channel::bounded(1);
        self.0.pending.borrow_mut().insert(id, tx);
        if self.0.writer.borrow_mut().write_all(line.as_bytes()).is_err() {
            self.0.pending.borrow_mut().remove(&id);
            return Err("The LinuxPlay engine isn't running.".into());
        }
        let value = rx.recv().await.map_err(|_| "The LinuxPlay engine stopped.".to_string())??;
        serde_json::from_value(value).map_err(|e| format!("The engine's answer made no sense: {e}"))
    }

    /// Whether this window started the engine, and so stops it on quitting.
    pub fn owned(&self) -> bool {
        self.0.child.borrow().is_some()
    }

    /// Stop the engine if this window started it: it ends its sessions and
    /// stops sharing first, as on Ctrl+C.
    pub fn shut_down(&self) {
        let Some(mut child) = self.0.child.borrow_mut().take() else { return };
        // SAFETY: kill(2) on our own child's pid; no memory is involved.
        unsafe { libc::kill(child.id() as libc::pid_t, libc::SIGINT) };
        let deadline = Instant::now() + STOP_TIMEOUT;
        while Instant::now() < deadline {
            if child.try_wait().ok().flatten().is_some() {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        let _ = child.kill();
        let _ = child.wait();
    }
}

/// Read the engine's lines until it goes away.
fn read(stream: UnixStream, tx: async_channel::Sender<Incoming>) {
    for line in BufReader::new(stream).lines() {
        let Ok(line) = line else { break };
        let Ok(value) = serde_json::from_str::<Value>(&line) else { continue };
        let message = if value.get("method").is_some() {
            match serde_json::from_value::<Notification>(value) {
                Ok(n) if n.method == method::STATE_CHANGED => match serde_json::from_value(n.params) {
                    Ok(state) => Incoming::State(Box::new(state)),
                    Err(_) => continue,
                },
                _ => continue,
            }
        } else {
            match serde_json::from_value(value) {
                Ok(response) => Incoming::Response(response),
                Err(_) => continue,
            }
        };
        if tx.send_blocking(message).is_err() {
            return;
        }
    }
    let _ = tx.send_blocking(Incoming::Closed);
}
