//! A private Unix socket through which `linuxplay-host revoke`, `sessions`,
//! `approve`, `deny` and `disconnect` reach the running host, so changes take
//! effect at once and the command can report what actually happened.
//!
//! The socket lives in `$XDG_RUNTIME_DIR/linuxplay/` (a `0700` directory) and
//! only accepts connections from the same user.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use linuxplay_net::{fsutil, tls};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tracing::{debug, warn};

use crate::approval::Decision;

const IO_TIMEOUT: Duration = Duration::from_secs(5);
const MAX_LINE: u64 = 512;

pub fn socket_path() -> PathBuf {
    match std::env::var_os("XDG_RUNTIME_DIR") {
        Some(dir) => PathBuf::from(dir).join("linuxplay").join("host.sock"),
        None => tls::config_dir("host").join("control.sock"),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Command {
    /// Re-read the trust store and end sessions that are no longer trusted.
    Reload,
    List,
    Decide(u64, Decision),
    Disconnect(Option<u64>),
}

impl Command {
    pub fn parse(line: &str) -> Option<Self> {
        let mut words = line.split_whitespace();
        let cmd = match (words.next()?, words.next(), words.next()) {
            ("reload", None, None) => Self::Reload,
            ("list", None, None) => Self::List,
            ("approve", Some(id), Some(scope)) => {
                let d = Decision::parse(scope).filter(|d| *d != Decision::Deny)?;
                Self::Decide(id.parse().ok()?, d)
            }
            ("deny", Some(id), None) => Self::Decide(id.parse().ok()?, Decision::Deny),
            ("disconnect", Some("all"), None) => Self::Disconnect(None),
            ("disconnect", Some(id), None) => Self::Disconnect(Some(id.parse().ok()?)),
            _ => return None,
        };
        words.next().is_none().then_some(cmd)
    }

    pub fn to_line(self) -> String {
        match self {
            Self::Reload => "reload".into(),
            Self::List => "list".into(),
            Self::Decide(id, Decision::Deny) => format!("deny {id}"),
            Self::Decide(id, Decision::ViewOnly) => format!("approve {id} view"),
            Self::Decide(id, Decision::Control) => format!("approve {id} control"),
            Self::Disconnect(None) => "disconnect all".into(),
            Self::Disconnect(Some(id)) => format!("disconnect {id}"),
        }
    }
}

/// What the running host does with a command.
pub trait Handler: Send + Sync + 'static {
    fn control(&self, cmd: Command) -> impl std::future::Future<Output = String> + Send;
}

/// Removes the socket file when the host exits.
pub struct SocketGuard(PathBuf);

impl Drop for SocketGuard {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

/// Claim the socket. Fails if another host is already running.
pub async fn bind() -> Result<(UnixListener, SocketGuard)> {
    let path = socket_path();
    if let Some(dir) = path.parent() {
        fsutil::private_dir(dir)?;
    }
    if path.exists() {
        if UnixStream::connect(&path).await.is_ok() {
            bail!("another linuxplay-host is already running (see `linuxplay-host sessions`)");
        }
        std::fs::remove_file(&path).with_context(|| format!("removing the stale {}", path.display()))?;
    }
    let listener = UnixListener::bind(&path).with_context(|| format!("creating {}", path.display()))?;
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))?;
    Ok((listener, SocketGuard(path)))
}

pub async fn serve<H: Handler>(listener: UnixListener, host: Arc<H>) {
    // SAFETY: geteuid has no preconditions.
    let me = unsafe { libc::geteuid() };
    loop {
        let stream = match listener.accept().await {
            Ok((s, _)) => s,
            Err(e) => {
                warn!("control socket: {e}");
                tokio::time::sleep(Duration::from_secs(1)).await;
                continue;
            }
        };
        if stream.peer_cred().map(|c| c.uid()).ok() != Some(me) {
            debug!("control socket: refused a connection from another user");
            continue;
        }
        let host = host.clone();
        tokio::spawn(async move {
            if let Err(e) = tokio::time::timeout(IO_TIMEOUT * 2, answer(stream, &*host)).await {
                debug!("control socket: {e}");
            }
        });
    }
}

async fn answer<H: Handler>(stream: UnixStream, host: &H) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let mut line = String::new();
    BufReader::new(read.take(MAX_LINE)).read_line(&mut line).await?;
    let reply = match Command::parse(line.trim()) {
        Some(cmd) => host.control(cmd).await,
        None => format!("error: unknown command {:?}", line.trim()),
    };
    write.write_all(reply.as_bytes()).await?;
    write.shutdown().await?;
    Ok(())
}

/// Send `cmd` to the running host. `Ok(None)` when no host is running.
pub async fn request(cmd: Command) -> Result<Option<String>> {
    let path = socket_path();
    let mut s = match UnixStream::connect(&path).await {
        Ok(s) => s,
        Err(e) if matches!(e.kind(), std::io::ErrorKind::NotFound | std::io::ErrorKind::ConnectionRefused) => {
            return Ok(None);
        }
        Err(e) => return Err(e).with_context(|| format!("connecting to {}", path.display())),
    };
    let fut = async {
        s.write_all(format!("{}\n", cmd.to_line()).as_bytes()).await?;
        let mut reply = String::new();
        s.read_to_string(&mut reply).await?;
        Ok::<_, anyhow::Error>(reply)
    };
    let reply = tokio::time::timeout(IO_TIMEOUT, fut).await.context("the running host didn't answer")??;
    Ok(Some(reply))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn commands_round_trip() {
        for c in [
            Command::Reload,
            Command::List,
            Command::Decide(3, Decision::Control),
            Command::Decide(3, Decision::ViewOnly),
            Command::Decide(4, Decision::Deny),
            Command::Disconnect(None),
            Command::Disconnect(Some(9)),
        ] {
            assert_eq!(Command::parse(&c.to_line()), Some(c));
        }
        assert_eq!(Command::parse("approve 3 deny"), None);
        assert_eq!(Command::parse("approve x control"), None);
        assert_eq!(Command::parse("list extra"), None);
        assert_eq!(Command::parse(""), None);
    }
}
