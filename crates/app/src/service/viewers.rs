//! Stream windows the engine has opened. Each is its own process: the window
//! needs a main thread to itself, and one crashing can't take the engine or
//! the other windows with it.

use std::sync::Mutex;

use anyhow::{Context, Result};
use linuxplay_proto::api;
use tokio::process::{Child, Command};
use tracing::info;

/// Stream windows open at once, at most.
const MAX_WINDOWS: usize = 16;

struct Window {
    id: u64,
    target: String,
    process: Child,
}

#[derive(Default)]
pub struct Viewers {
    windows: Mutex<Vec<Window>>,
    next_id: Mutex<u64>,
}

impl Viewers {
    /// Open a stream window for `request`.
    pub fn open(&self, request: &api::Connect) -> Result<api::Connection> {
        let mut windows = self.windows.lock().unwrap();
        reap(&mut windows);
        anyhow::ensure!(windows.len() < MAX_WINDOWS, "too many stream windows are open");

        let program = std::env::current_exe().context("finding this program")?;
        let mut command = Command::new(program);
        command.arg("connect").arg(&request.target).kill_on_drop(true);
        if let Some(display) = request.display {
            command.args(["--display", &display.to_string()]);
        }
        for (wanted, flag) in [(request.fullscreen, "--fullscreen"), (request.stats, "--stats"), (request.game_mouse, "--game-mouse")] {
            if wanted {
                command.arg(flag);
            }
        }
        // Secrets go through the environment, never the argument list, which
        // other users can read.
        match &request.pin {
            Some(pin) => command.env("LINUXPLAY_PIN", pin),
            None => command.env_remove("LINUXPLAY_PIN"),
        };
        let process = command.spawn().context("opening the stream window")?;

        let id = {
            let mut next = self.next_id.lock().unwrap();
            *next += 1;
            *next
        };
        info!("stream window {id} to {}", request.target);
        windows.push(Window { id, target: request.target.clone(), process });
        Ok(api::Connection { id, target: request.target.clone() })
    }

    /// Close a window. Returns whether it was open.
    pub fn close(&self, id: u64) -> bool {
        let mut windows = self.windows.lock().unwrap();
        let Some(index) = windows.iter().position(|w| w.id == id) else { return false };
        // Dropping the process ends it.
        windows.remove(index);
        true
    }

    /// The windows still open.
    pub fn list(&self) -> Vec<api::Connection> {
        let mut windows = self.windows.lock().unwrap();
        reap(&mut windows);
        windows.iter().map(|w| api::Connection { id: w.id, target: w.target.clone() }).collect()
    }
}

/// Forget windows whose process has ended.
fn reap(windows: &mut Vec<Window>) {
    windows.retain_mut(|w| matches!(w.process.try_wait(), Ok(None)));
}
