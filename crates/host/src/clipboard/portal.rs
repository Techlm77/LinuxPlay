//! The clipboard of a Wayland desktop, through the RemoteDesktop session's
//! clipboard portal. The compositor's share dialog covers it; no other
//! permission is asked for.

use std::io::{Read, Write};
use std::os::fd::{AsRawFd, OwnedFd};
use std::sync::Arc;

use ashpd::desktop::Session;
use ashpd::desktop::clipboard::{Clipboard as Portal, SetSelectionOptions};
use ashpd::desktop::remote_desktop::RemoteDesktop;
use futures_util::StreamExt;
use tracing::{debug, info};

use super::{Port, Text, fit};

/// The kinds of text offered and accepted, most preferred first.
const TEXT_TYPES: [&str; 4] = ["text/plain;charset=utf-8", "UTF8_STRING", "text/plain", "STRING"];

/// Ask for clipboard access on `session`. Must happen before the session
/// starts; `None` when the desktop's portal has no clipboard.
pub async fn request(session: &Session<RemoteDesktop>) -> Option<Portal> {
    let portal = Portal::new().await.inspect_err(|e| debug!("no clipboard portal: {e}")).ok()?;
    match portal.request(session, Default::default()).await {
        Ok(()) => Some(portal),
        Err(e) => {
            info!("this desktop doesn't share its clipboard through the portal: {e}");
            None
        }
    }
}

/// Carry text both ways until the session ends (the task is aborted).
pub async fn serve(portal: Portal, session: Arc<Session<RemoteDesktop>>, mut port: Port) {
    let (Ok(owners), Ok(transfers)) = (
        portal.receive_selection_owner_changed::<RemoteDesktop>().await,
        portal.receive_selection_transfer::<RemoteDesktop>().await,
    ) else {
        return debug!("clipboard portal signals unavailable");
    };
    tokio::pin!(owners);
    tokio::pin!(transfers);
    // The text we are offering, while we own the selection.
    let mut offering: Text = None;
    loop {
        tokio::select! {
            Some((_, change)) = owners.next() => {
                // Our own offer coming back is not a copy.
                if change.session_is_owner() == Some(true) {
                    continue;
                }
                offering = None;
                let Some(kind) = TEXT_TYPES.iter().find(|t| change.mime_types().iter().any(|m| m == *t)) else { continue };
                match portal.selection_read(&session, kind).await {
                    Ok(fd) => {
                        if let Some(text) = read_all(fd.into()).await {
                            port.copied(text);
                        }
                    }
                    Err(e) => debug!("reading the clipboard: {e}"),
                }
            }
            Some((_, _kind, serial)) = transfers.next() => {
                let sent = match (&offering, portal.selection_write(&session, serial).await) {
                    (Some(text), Ok(fd)) => write_all(fd.into(), text.clone()).await,
                    _ => false,
                };
                let _ = portal.selection_write_done(&session, serial, sent).await;
            }
            changed = port.wanted.changed() => {
                if changed.is_err() {
                    return;
                }
                offering = port.wanted.borrow_and_update().clone();
                if offering.is_some() {
                    let options = SetSelectionOptions::default().set_mime_types(&TEXT_TYPES);
                    if let Err(e) = portal.set_selection(&session, options).await {
                        debug!("offering the clipboard: {e}");
                    }
                }
            }
            else => return,
        }
    }
}

/// The portal's pipes may be non-blocking; the reads and writes here block
/// on a worker thread instead.
fn blocking(fd: OwnedFd) -> std::fs::File {
    // SAFETY: fcntl on a descriptor we own.
    unsafe {
        let raw = fd.as_raw_fd();
        libc::fcntl(raw, libc::F_SETFL, libc::fcntl(raw, libc::F_GETFL) & !libc::O_NONBLOCK);
    }
    std::fs::File::from(fd)
}

async fn read_all(fd: OwnedFd) -> Option<String> {
    tokio::task::spawn_blocking(move || {
        let mut bytes = Vec::new();
        blocking(fd).take(super::MAX_TEXT as u64 + 4).read_to_end(&mut bytes).ok()?;
        Some(fit(String::from_utf8_lossy(&bytes).into_owned()))
    })
    .await
    .ok()
    .flatten()
}

async fn write_all(fd: OwnedFd, text: Arc<str>) -> bool {
    tokio::task::spawn_blocking(move || blocking(fd).write_all(text.as_bytes()).is_ok()).await.unwrap_or(false)
}
