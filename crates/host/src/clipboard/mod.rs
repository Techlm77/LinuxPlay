//! The host desktop's clipboard, shared with the clients that may control
//! this desktop.
//!
//! One [`Clipboard`] per host. A desktop backend ([`x11`] or [`portal`])
//! publishes text copied on this computer and puts text from clients on the
//! desktop's clipboard. Text a client sent is not echoed back as a copy.

pub mod portal;
pub mod x11;

use std::sync::Arc;

use linuxplay_proto::MAX_CONTROL_MESSAGE;
use tokio::sync::watch;

/// Largest text passed either way: what fits in one control message.
pub const MAX_TEXT: usize = MAX_CONTROL_MESSAGE - 64;

/// Text, shared without copying.
pub type Text = Option<Arc<str>>;

#[derive(Clone)]
pub struct Clipboard {
    /// Copied on this computer, newest.
    copied: watch::Sender<Text>,
    /// Sent by a client, to put on this computer's clipboard.
    wanted: watch::Sender<Text>,
}

/// A backend's end: where it reports copies and learns what to paste.
pub struct Port {
    copied: watch::Sender<Text>,
    pub wanted: watch::Receiver<Text>,
}

impl Port {
    /// Report text copied on this desktop. Text already known, or text a
    /// client just pasted coming back, is not news: passing it on would
    /// bounce it between host and client for ever.
    pub fn copied(&self, text: String) {
        let text = fit(text);
        let from_a_client = self.wanted.borrow().as_deref() == Some(text.as_str());
        self.copied.send_if_modified(|current| {
            if from_a_client || current.as_deref() == Some(text.as_str()) {
                return false;
            }
            *current = Some(text.into());
            true
        });
    }
}

impl Default for Clipboard {
    fn default() -> Self {
        Self { copied: watch::channel(None).0, wanted: watch::channel(None).0 }
    }
}

impl Clipboard {
    /// What a desktop backend works with.
    pub fn port(&self) -> Port {
        Port { copied: self.copied.clone(), wanted: self.wanted.subscribe() }
    }

    /// Text copied on this computer, as it changes.
    pub fn copies(&self) -> watch::Receiver<Text> {
        let mut copies = self.copied.subscribe();
        // A session that starts now isn't sent whatever was copied before.
        copies.mark_unchanged();
        copies
    }

    /// Put a client's text on this computer's clipboard.
    pub fn paste(&self, text: &str) {
        if text.len() <= MAX_TEXT {
            self.wanted.send_replace(Some(text.into()));
        }
    }
}

/// Clamp text from the desktop to what a control message can carry, at a
/// character boundary.
pub fn fit(mut text: String) -> String {
    if text.len() > MAX_TEXT {
        let mut end = MAX_TEXT;
        while !text.is_char_boundary(end) {
            end -= 1;
        }
        text.truncate(end);
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn copies_and_pastes_meet_at_the_backend() {
        let clipboard = Clipboard::default();
        let mut port = clipboard.port();
        let mut copies = clipboard.copies();

        clipboard.paste("from the client");
        port.wanted.changed().await.unwrap();
        assert_eq!(port.wanted.borrow_and_update().as_deref(), Some("from the client"));

        port.copied("from the desktop".into());
        copies.changed().await.unwrap();
        assert_eq!(copies.borrow_and_update().as_deref(), Some("from the desktop"));

        // The same text again, or the client's own text coming back, is not news.
        port.copied("from the desktop".into());
        port.copied("from the client".into());
        assert!(!copies.has_changed().unwrap());

        // Too much text for one message is not passed on.
        clipboard.paste(&"x".repeat(MAX_TEXT + 1));
        assert!(!port.wanted.has_changed().unwrap());
    }

    #[test]
    fn long_text_is_cut_at_a_character() {
        let text = "é".repeat(MAX_TEXT);
        let cut = fit(text);
        assert!(cut.len() <= MAX_TEXT && cut.chars().all(|c| c == 'é'));
    }
}
