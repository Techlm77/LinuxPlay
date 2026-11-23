//! The clipboard of an X11 desktop.
//!
//! A hidden window watches the CLIPBOARD selection (XFixes) and reads it as
//! UTF-8 when someone else copies; to paste a client's text it takes the
//! selection and answers other programs' requests for it.

use std::time::Duration;

use anyhow::{Context, Result};
use tracing::{debug, warn};
use x11rb::connection::Connection;
use x11rb::protocol::Event;
use x11rb::protocol::xfixes::{ConnectionExt as _, SelectionEventMask};
use x11rb::protocol::xproto::{
    Atom, AtomEnum, ConnectionExt as _, CreateWindowAux, EventMask, PropMode, SelectionNotifyEvent,
    SelectionRequestEvent, WindowClass, SELECTION_NOTIFY_EVENT,
};
use x11rb::rust_connection::RustConnection;
use x11rb::wrapper::ConnectionExt as _;
use x11rb::{CURRENT_TIME, NONE};

use super::Port;

/// How often the thread checks for text to paste while X is quiet.
const POLL: Duration = Duration::from_millis(20);
/// Largest selection read, in 32-bit units for GetProperty.
const MAX_READ_WORDS: u32 = (super::MAX_TEXT as u32).div_ceil(4);

struct Atoms {
    clipboard: Atom,
    utf8: Atom,
    targets: Atom,
    text: Atom,
    mime: Atom,
    landing: Atom,
}

/// Run the clipboard for `display` on its own thread until the host ends.
pub fn spawn(display: Option<String>, port: Port) {
    let spawned = std::thread::Builder::new().name("linuxplay-clipboard".into()).spawn(move || {
        if let Err(e) = run(display.as_deref(), port) {
            warn!("the clipboard isn't shared: {e:#}");
        }
    });
    if let Err(e) = spawned {
        warn!("the clipboard isn't shared: {e}");
    }
}

fn run(display: Option<&str>, mut port: Port) -> Result<()> {
    let (conn, screen) = x11rb::connect(display).context("connecting to the X display")?;
    let root = conn.setup().roots[screen].root;
    let window = conn.generate_id()?;
    conn.create_window(0, window, root, 0, 0, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new().event_mask(EventMask::PROPERTY_CHANGE))?;
    let atom = |name: &str| -> Result<Atom> { Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom) };
    let atoms = Atoms {
        clipboard: atom("CLIPBOARD")?,
        utf8: atom("UTF8_STRING")?,
        targets: atom("TARGETS")?,
        text: atom("TEXT")?,
        mime: atom("text/plain;charset=utf-8")?,
        landing: atom("LINUXPLAY_CLIPBOARD")?,
    };
    conn.xfixes_query_version(5, 0)?.reply().context("the X server has no XFixes")?;
    conn.xfixes_select_selection_input(window, atoms.clipboard, SelectionEventMask::SET_SELECTION_OWNER)?;
    conn.flush()?;

    // The text we are offering, while we own the selection.
    let mut offering: Option<String> = None;
    loop {
        // The host is gone once nobody can send text any more.
        let Ok(pending) = port.wanted.has_changed() else { return Ok(()) };
        if pending
            && let Some(text) = port.wanted.borrow_and_update().clone() {
                offering = Some(text.to_string());
                conn.set_selection_owner(window, atoms.clipboard, CURRENT_TIME)?;
                conn.flush()?;
            }
        let Some(event) = conn.poll_for_event()? else {
            std::thread::sleep(POLL);
            continue;
        };
        match event {
            Event::XfixesSelectionNotify(e) if e.selection == atoms.clipboard && e.owner != window && e.owner != NONE => {
                // Someone else copied: ask for it as UTF-8.
                conn.convert_selection(window, atoms.clipboard, atoms.utf8, atoms.landing, e.selection_timestamp)?;
                conn.flush()?;
            }
            Event::SelectionNotify(e) if e.requestor == window && e.property == atoms.landing => {
                let reply = conn.get_property(true, window, atoms.landing, AtomEnum::ANY, 0, MAX_READ_WORDS)?.reply()?;
                // Large selections (INCR) are not followed; they don't fit a message anyway.
                if reply.type_ == atoms.utf8 || reply.type_ == u32::from(AtomEnum::STRING) {
                    port.copied(String::from_utf8_lossy(&reply.value).into_owned());
                }
            }
            Event::SelectionRequest(request) => answer(&conn, &atoms, &request, offering.as_deref())?,
            Event::SelectionClear(e) if e.selection == atoms.clipboard => offering = None,
            _ => debug!("clipboard: ignored {event:?}"),
        }
    }
}

/// Give another program the text we offer, or say we can't.
fn answer(conn: &RustConnection, atoms: &Atoms, request: &SelectionRequestEvent, offering: Option<&str>) -> Result<()> {
    // Old clients leave the property out and mean the target.
    let property = if request.property == NONE { request.target } else { request.property };
    let delivered = match offering {
        Some(_) if request.target == atoms.targets => {
            let offered = [atoms.targets, atoms.utf8, atoms.mime, atoms.text, u32::from(AtomEnum::STRING)];
            conn.change_property32(PropMode::REPLACE, request.requestor, property, AtomEnum::ATOM, &offered)?;
            true
        }
        Some(text) if [atoms.utf8, atoms.mime, atoms.text, u32::from(AtomEnum::STRING)].contains(&request.target) => {
            let kind = if request.target == atoms.mime { atoms.mime } else { atoms.utf8 };
            conn.change_property8(PropMode::REPLACE, request.requestor, property, kind, text.as_bytes())?;
            true
        }
        _ => false,
    };
    let notify = SelectionNotifyEvent {
        response_type: SELECTION_NOTIFY_EVENT,
        sequence: 0,
        time: request.time,
        requestor: request.requestor,
        selection: request.selection,
        target: request.target,
        property: if delivered { property } else { NONE },
    };
    conn.send_event(false, request.requestor, EventMask::NO_EVENT, notify)?;
    conn.flush()?;
    Ok(())
}

/// The window that owns the clipboard while we offer text, for tests.
#[cfg(test)]
fn owner(conn: &RustConnection, clipboard: Atom) -> Result<x11rb::protocol::xproto::Window> {
    Ok(conn.get_selection_owner(clipboard)?.reply()?.owner)
}

#[cfg(test)]
mod tests {
    use std::process::{Child, Command};

    use super::*;
    use crate::clipboard::Clipboard;

    /// A private X server for the test; killed when dropped.
    struct Xvfb(Child, String);

    impl Drop for Xvfb {
        fn drop(&mut self) {
            let _ = self.0.kill();
        }
    }

    fn xvfb() -> Option<Xvfb> {
        let display = format!(":{}", 90 + std::process::id() % 100);
        let child = Command::new("Xvfb").args([&display, "-nolisten", "tcp"]).spawn().ok()?;
        for _ in 0..50 {
            if x11rb::connect(Some(&display)).is_ok() {
                return Some(Xvfb(child, display));
            }
            std::thread::sleep(Duration::from_millis(50));
        }
        None
    }

    /// Needs Xvfb: `cargo test -p linuxplay-host -- --ignored x11_clipboard`.
    #[test]
    #[ignore]
    fn x11_clipboard_both_ways() {
        let server = xvfb().expect("Xvfb");
        let clipboard = Clipboard::default();
        spawn(Some(server.1.clone()), clipboard.port());
        std::thread::sleep(Duration::from_millis(200));

        // Another program reads what a client pasted.
        clipboard.paste("from the client ✓");
        let (other, screen) = x11rb::connect(Some(&server.1)).unwrap();
        let root = other.setup().roots[screen].root;
        let win = other.generate_id().unwrap();
        other.create_window(0, win, root, 0, 0, 1, 1, 0, WindowClass::INPUT_ONLY, 0, &CreateWindowAux::new()).unwrap();
        let atom = |n: &str| other.intern_atom(false, n.as_bytes()).unwrap().reply().unwrap().atom;
        let (clip, utf8, prop) = (atom("CLIPBOARD"), atom("UTF8_STRING"), atom("TEST_PROP"));
        for _ in 0..50 {
            if owner(&other, clip).unwrap() != NONE {
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        other.convert_selection(win, clip, utf8, prop, CURRENT_TIME).unwrap();
        other.flush().unwrap();
        loop {
            if let Event::SelectionNotify(e) = other.wait_for_event().unwrap() {
                assert_eq!(e.property, prop, "the request was refused");
                break;
            }
        }
        let got = other.get_property(true, win, prop, AtomEnum::ANY, 0, 1024).unwrap().reply().unwrap();
        assert_eq!(String::from_utf8(got.value).unwrap(), "from the client ✓");

        // A program copies; the host sees it.
        let mut copies = clipboard.copies();
        other.set_selection_owner(win, clip, CURRENT_TIME).unwrap();
        other.flush().unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread().enable_time().build().unwrap();
        loop {
            let event = other.wait_for_event().unwrap();
            if let Event::SelectionRequest(r) = event {
                other.change_property8(PropMode::REPLACE, r.requestor, r.property, utf8, "copied on the host".as_bytes()).unwrap();
                let notify = SelectionNotifyEvent { response_type: SELECTION_NOTIFY_EVENT, sequence: 0, time: r.time, requestor: r.requestor, selection: r.selection, target: r.target, property: r.property };
                other.send_event(false, r.requestor, EventMask::NO_EVENT, notify).unwrap();
                other.flush().unwrap();
                break;
            }
        }
        runtime.block_on(async { tokio::time::timeout(Duration::from_secs(3), copies.changed()).await }).unwrap().unwrap();
        assert_eq!(copies.borrow().as_deref(), Some("copied on the host"));
    }
}
