//! What the window remembers between runs: stream and sharing options, and
//! the account server last used. A small JSON file in the user's config.

use std::path::PathBuf;

use gtk::glib;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(default)]
pub struct Settings {
    pub fullscreen: bool,
    pub game_mouse: bool,
    pub stats: bool,
    pub unattended: bool,
    pub audio: bool,
    pub virtual_display: bool,
    pub account_server: bool,
    pub server: String,
    pub username: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            fullscreen: false,
            game_mouse: false,
            stats: false,
            unattended: false,
            audio: true,
            virtual_display: false,
            account_server: false,
            server: String::new(),
            username: String::new(),
        }
    }
}

fn path() -> PathBuf {
    glib::user_config_dir().join("linuxplay").join("gui.json")
}

impl Settings {
    /// The saved settings; anything unreadable means the defaults.
    pub fn load() -> Self {
        std::fs::read_to_string(path()).ok().and_then(|text| serde_json::from_str(&text).ok()).unwrap_or_default()
    }

    pub fn save(&self) {
        let path = path();
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        if let Ok(text) = serde_json::to_string_pretty(self) {
            let _ = std::fs::write(path, text);
        }
    }
}
