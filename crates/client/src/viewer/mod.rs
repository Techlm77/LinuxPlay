//! The stream window: connects to a host, shows its screen and sends it
//! what the user does.
//!
//! Hotkeys (Ctrl+Alt+Shift+…) stay local: Q disconnects, F toggles
//! fullscreen, M toggles the game mouse, S the statistics. Everything else
//! goes to the host.

mod hud;
mod render;
mod stream;
mod wake;
mod window;

use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{Context as _, Result};
use linuxplay_net::rendezvous::Account;
use linuxplay_net::tls::{self, Identity};
use linuxplay_proto::input::InputEvent;
use linuxplay_proto::signal::MonitorInfo;
use sdl3::EventPump;
use sdl3::event::{Event, WindowEvent};
use sdl3::keyboard::{Mod, Scancode};
use tokio::sync::oneshot;
use tracing::{debug, info, warn};

use self::hud::Line;
use self::render::{RenderThread, Scene};
use self::stream::Stream;
use self::wake::{Wake, Waker};
use self::window::GlWindow;
use crate::connect::{self, ConnectEvent, Offer, Request, UiSink};
use crate::decode;
use crate::gl::GlBridge;
use crate::input::gamepad::Gamepads;
use crate::input::{InputMapper, MouseMode};
use crate::known_hosts::KnownHosts;
use crate::layout;

const INITIAL_SIZE: (u32, u32) = (1280, 720);
/// How often the round trip is probed and the statistics refreshed.
const TICK: Duration = Duration::from_secs(1);
/// How long the hotkey hint stays up when a stream starts.
const HINT_TIME: Duration = Duration::from_secs(6);
const HINT: &str = "Ctrl+Alt+Shift +   Q disconnect · F fullscreen · M game mouse · S statistics";
/// How long a warning stays up when a stream starts.
const WARNING_TIME: Duration = Duration::from_secs(20);
/// Longest PIN or password that can be typed.
const MAX_PIN_CHARS: usize = 128;
/// Notices wrap at about this many characters.
const WRAP_COLUMNS: usize = 64;

/// What to connect to and how the window starts out.
#[derive(Debug, Clone)]
pub struct ViewerOptions {
    /// What to connect to: an invite link, the name of one of the account's
    /// devices, or a host as `address`, `address:port` or `[v6]:port`.
    pub address: String,
    /// Which of the host's displays to open; `None` asks when there are several.
    pub display: Option<u32>,
    /// A PIN or password known in advance.
    pub pin: Option<String>,
    /// The name this device shows on the host.
    pub client_name: String,
    pub fullscreen: bool,
    /// Show the statistics panel from the start.
    pub stats: bool,
    /// Start with the mouse locked to the window, for games.
    pub game_mouse: bool,
}

/// Where the connection stands, which is also what the window shows.
enum Stage {
    Connecting { status: String },
    Pin { host: String, password: bool, typed: String, reply: oneshot::Sender<Option<String>> },
    Displays { host: String, monitors: Vec<MonitorInfo>, reply: oneshot::Sender<Option<u32>> },
    Streaming { stream: Box<Stream>, host: String },
    Ended { failure: Option<String> },
}

/// Open the stream window and run it until it is closed. Must be called on
/// the main thread.
pub fn run(options: ViewerOptions) -> Result<()> {
    let (mut viewer, mut events) = Viewer::open(options)?;
    viewer.run(&mut events)
}

struct Viewer {
    // Fields drop in this order: everything that uses the OpenGL context
    // goes before the window that owns it.
    stage: Stage,
    render: RenderThread,
    bridge: GlBridge,
    gamepads: Gamepads,
    gl: GlWindow,

    waker: Arc<Waker>,
    connection: async_channel::Receiver<ConnectEvent>,
    /// Hangs up the signalling connection when dropped.
    _hang_up: async_channel::Sender<()>,
    runtime: tokio::runtime::Runtime,
    known_hosts: Option<KnownHosts>,

    mapper: InputMapper,
    show_stats: bool,
    hint_until: Option<Instant>,
    /// Something the user should act on, shown at the bottom of the window.
    warning: Option<String>,
    /// The clipboard text last exchanged with the host, either way.
    clipboard: Option<String>,
    next_tick: Instant,
    /// What the render thread was last told to draw.
    scene: Scene,
    quit: bool,
}

impl Viewer {
    fn open(options: ViewerOptions) -> Result<(Self, EventPump)> {
        let mut gl = GlWindow::open("LinuxPlay", INITIAL_SIZE)?;
        let events = gl.sdl.event_pump().map_err(|e| anyhow::anyhow!("starting the event loop: {e}"))?;
        let waker = Arc::new(
            Waker::new(&gl.sdl.event().map_err(|e| anyhow::anyhow!("{e}"))?).map_err(|e| anyhow::anyhow!("{e}"))?,
        );
        let (render, bridge) = RenderThread::start(gl.render_target()?, Scene::default())?;
        let gamepads = Gamepads::new(gl.sdl.gamepad().map_err(|e| anyhow::anyhow!("starting gamepad support: {e}"))?);

        let identity = Identity::load_or_create(&tls::config_dir("client"), "LinuxPlay client")
            .context("creating this device's identity")?;
        let known_path = tls::config_dir("client").join("known_hosts.toml");
        let known_hosts = KnownHosts::load(known_path).context(
            "the list of paired hosts can't be read; fix or remove known_hosts.toml in LinuxPlay's config folder",
        )?;
        let pins = known_hosts.pins()?;
        // A damaged sign-in file shouldn't stop a direct connection.
        let account = Account::load(&Account::default_path()).inspect_err(|e| warn!("not signed in: {e:#}")).ok().flatten();

        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("linuxplay-net")
            .enable_all()
            .build()
            .context("starting the network runtime")?;
        let (ui, connection) = UiSink::new({
            let waker = waker.clone();
            move || waker.wake()
        });
        let (hang_up, cancelled) = async_channel::bounded(1);
        let request = Request {
            target: options.address.clone(),
            account,
            pins,
            client_name: options.client_name.clone(),
            codecs: decode::decodable(),
            hw_codecs: decode::hw_decodable(),
            max_fps: gl.refresh_rate().unwrap_or(60),
            monitor: options.display,
            pin: options.pin.clone(),
        };
        runtime.spawn(connect::run(request, Arc::new(identity), ui, cancelled));

        if options.fullscreen {
            let _ = gl.window.set_fullscreen(true);
        }
        let mut mapper = InputMapper::new();
        if options.game_mouse {
            mapper.mode = MouseMode::Relative;
        }
        let viewer = Self {
            stage: Stage::Connecting { status: format!("Connecting to {}…", options.address) },
            render,
            bridge,
            gamepads,
            gl,
            waker,
            connection,
            _hang_up: hang_up,
            runtime,
            known_hosts: Some(known_hosts),
            mapper,
            show_stats: options.stats,
            hint_until: None,
            warning: None,
            clipboard: None,
            next_tick: Instant::now() + TICK,
            scene: Scene::default(),
            quit: false,
        };
        Ok((viewer, events))
    }

    fn run(&mut self, events: &mut EventPump) -> Result<()> {
        while !self.quit {
            self.publish();
            let wait = self.next_tick.saturating_duration_since(Instant::now());
            if let Some(event) = events.wait_event_timeout(wait) {
                self.on_event(event);
            }
            while let Some(event) = events.poll_event() {
                self.on_event(event);
            }
            self.drain_connection();
            self.drain_stream();
            self.tick();
        }
        Ok(())
    }

    // ---- window events -------------------------------------------------

    fn on_event(&mut self, event: Event) {
        match &event {
            Event::Quit { .. } | Event::Window { win_event: WindowEvent::CloseRequested, .. } => self.quit = true,
            Event::Window { win_event, .. } => self.on_window_event(win_event),
            Event::ClipboardUpdate { .. } => {
                self.share_clipboard();
                return;
            }
            Event::User { .. } => {
                // Taking the payload out frees it.
                if event.as_user_event_type::<Wake>().is_some() {
                    self.waker.acknowledge();
                }
                return;
            }
            _ => {}
        }
        if self.hotkey(&event) {
            return;
        }
        // Gamepads are tracked from the moment they are plugged in, so they
        // can be announced when a stream starts.
        let pad_event = self.gamepads.handle(&event);
        match &mut self.stage {
            Stage::Streaming { stream, .. } => {
                if let Some(input) = pad_event.or_else(|| self.mapper.translate(&event)) {
                    stream.send_input(input);
                }
            }
            Stage::Pin { .. } => self.pin_key(&event),
            Stage::Displays { .. } => self.display_key(&event),
            Stage::Connecting { .. } | Stage::Ended { .. } => {
                if matches!(event, Event::KeyDown { scancode: Some(Scancode::Escape | Scancode::Return), .. }) {
                    self.quit = true;
                }
            }
        }
    }

    fn on_window_event(&mut self, event: &WindowEvent) {
        if event == &WindowEvent::FocusLost
            && let Stage::Streaming { stream, .. } = &self.stage {
                // Whatever was held stays held on the host otherwise.
                stream.send_input(InputEvent::ReleaseAll);
            }
        self.layout();
    }

    /// Text was copied on this computer: hand it to the host.
    fn share_clipboard(&mut self) {
        let Stage::Streaming { stream, .. } = &self.stage else { return };
        let Ok(text) = self.gl.video.clipboard().clipboard_text() else { return };
        if text.is_empty() || self.clipboard.as_deref() == Some(text.as_str()) {
            return;
        }
        debug!("clipboard: {} characters copied here, sent to the host", text.chars().count());
        stream.send_clipboard(&text);
        self.clipboard = Some(text);
    }

    /// Handle Ctrl+Alt+Shift+key. Returns whether the event was a hotkey.
    fn hotkey(&mut self, event: &Event) -> bool {
        let Event::KeyDown { scancode: Some(key), keymod, repeat: false, .. } = *event else { return false };
        let held = |either: Mod| keymod.intersects(either);
        if !(held(Mod::LCTRLMOD | Mod::RCTRLMOD) && held(Mod::LALTMOD | Mod::RALTMOD) && held(Mod::LSHIFTMOD | Mod::RSHIFTMOD))
        {
            return false;
        }
        match key {
            Scancode::Q => self.quit = true,
            Scancode::F => {
                let fullscreen = self.gl.window.fullscreen_state() == sdl3::video::FullscreenType::Off;
                if let Err(e) = self.gl.window.set_fullscreen(fullscreen) {
                    warn!("fullscreen: {e}");
                }
                self.apply_grabs();
            }
            Scancode::M => {
                self.mapper.mode = match self.mapper.mode {
                    MouseMode::Absolute => MouseMode::Relative,
                    MouseMode::Relative => MouseMode::Absolute,
                };
                self.apply_grabs();
            }
            Scancode::S => self.show_stats = !self.show_stats,
            _ => return false,
        }
        true
    }

    /// Typing a PIN or password.
    fn pin_key(&mut self, event: &Event) {
        let Stage::Pin { typed, .. } = &mut self.stage else { return };
        match event {
            Event::TextInput { text, .. } => {
                typed.extend(text.chars().filter(|c| !c.is_control()));
                if let Some((cut, _)) = typed.char_indices().nth(MAX_PIN_CHARS) {
                    typed.truncate(cut);
                }
            }
            Event::KeyDown { scancode: Some(Scancode::Backspace), .. } => drop(typed.pop()),
            Event::KeyDown { scancode: Some(Scancode::Return | Scancode::KpEnter), .. } if !typed.is_empty() => {
                self.answer_pin(true);
            }
            Event::KeyDown { scancode: Some(Scancode::Escape), .. } => self.answer_pin(false),
            _ => {}
        }
    }

    fn answer_pin(&mut self, confirmed: bool) {
        let waiting = Stage::Connecting { status: "Pairing…".into() };
        if let Stage::Pin { typed, reply, .. } = std::mem::replace(&mut self.stage, waiting) {
            let _ = reply.send(confirmed.then_some(typed));
        }
        self.gl.video.text_input().stop(&self.gl.window);
    }

    /// Choosing one of the host's displays with a number key.
    fn display_key(&mut self, event: &Event) {
        let Event::KeyDown { scancode: Some(key), .. } = *event else { return };
        let Stage::Displays { monitors, .. } = &self.stage else { return };
        let choice = match key {
            Scancode::Escape => None,
            key => match digit(key).and_then(|d| monitors.get(d.checked_sub(1)?)) {
                Some(monitor) => Some(monitor.index),
                None => return,
            },
        };
        let waiting = Stage::Connecting { status: "Starting the stream…".into() };
        if let Stage::Displays { reply, .. } = std::mem::replace(&mut self.stage, waiting) {
            let _ = reply.send(choice);
        }
    }

    // ---- the connection ------------------------------------------------

    fn drain_connection(&mut self) {
        while let Ok(event) = self.connection.try_recv() {
            self.on_connect_event(event);
        }
    }

    fn on_connect_event(&mut self, event: ConnectEvent) {
        match event {
            ConnectEvent::Status(status) => self.stage = Stage::Connecting { status },
            ConnectEvent::NeedPin { host_name, password, reply } => {
                self.stage = Stage::Pin { host: host_name, password, typed: String::new(), reply };
                self.gl.video.text_input().start(&self.gl.window);
            }
            ConnectEvent::NeedMonitor { host_name, monitors, reply } => {
                self.stage = Stage::Displays { host: host_name, monitors, reply };
            }
            ConnectEvent::Verified { address, host_name, fingerprint } => {
                if let Some(known) = &mut self.known_hosts
                    && let Err(e) = known.remember(&address, &host_name, &fingerprint) {
                        warn!("couldn't remember {host_name}: {e:#}");
                    }
            }
            ConnectEvent::Offer(offer, reply) => {
                let answer = self.start_stream(offer);
                if let Err(e) = &answer {
                    self.stage = Stage::Ended { failure: Some(format!("{e:#}")) };
                }
                let _ = reply.send(answer);
            }
            ConnectEvent::Warning(warning) => {
                warn!("{warning}");
                self.warning = Some(warning);
            }
            ConnectEvent::Ended(failure) => self.end(failure),
        }
    }

    fn start_stream(&mut self, offer: Offer) -> Result<String> {
        let wake: Arc<dyn Fn() + Send + Sync> = {
            let waker = self.waker.clone();
            Arc::new(move || waker.wake())
        };
        let (stream, answer) = Stream::open(&offer, self.runtime.handle(), &self.bridge, wake, self.render.frame_notifier())?;
        self.render.set_video(Some(stream.video()));
        info!("streaming {} from {}", offer.monitor.name, offer.host_name);
        let title = format!("{} · {} — LinuxPlay", offer.monitor.name, offer.host_name);
        let _ = self.gl.window.set_title(&title);
        if self.gl.window.fullscreen_state() == sdl3::video::FullscreenType::Off {
            self.gl.fit_to((offer.width, offer.height));
        }
        self.stage = Stage::Streaming { stream: Box::new(stream), host: offer.host_name };
        // A warning stays up longer than the key hint: it needs reading.
        let shown_for = if self.warning.is_some() { WARNING_TIME } else { HINT_TIME };
        self.hint_until = Some(Instant::now() + shown_for);
        self.next_tick = Instant::now() + TICK;
        self.layout();
        self.apply_grabs();
        Ok(answer)
    }

    /// The connection is over; keep the window up to say why.
    fn end(&mut self, failure: Option<String>) {
        // The first reason given is the real one; later ones are its echo.
        if !matches!(self.stage, Stage::Ended { .. }) {
            match &failure {
                Some(reason) => warn!("connection ended: {reason}"),
                None => info!("the host ended the session"),
            }
            self.stage = Stage::Ended { failure };
        }
        self.render.set_video(None);
        self.apply_grabs();
    }

    fn drain_stream(&mut self) {
        let Stage::Streaming { stream, .. } = &mut self.stage else { return };
        let changes = stream.poll();
        if changes.input_opened {
            for event in self.gamepads.announce() {
                stream.send_input(event);
            }
        }
        for effect in changes.rumble {
            self.gamepads.rumble(effect);
        }
        if let Some(text) = changes.clipboard.filter(|text| self.clipboard.as_ref() != Some(text)) {
            debug!("clipboard: {} characters from the host", text.chars().count());
            // Remembered first, so the clipboard update this causes isn't
            // sent straight back.
            self.clipboard = Some(text.clone());
            if let Err(e) = self.gl.video.clipboard().set_clipboard_text(&text) {
                warn!("couldn't set the clipboard: {e}");
            }
        }
        if let Some(failure) = changes.ended {
            self.end(failure);
        }
        // The picture may have appeared or changed size.
        self.layout();
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if self.hint_until.is_some_and(|t| now >= t) {
            self.hint_until = None;
        }
        if now < self.next_tick {
            return;
        }
        self.next_tick = now + TICK;
        if let Stage::Streaming { stream, .. } = &mut self.stage {
            stream.tick();
        }
    }

    // ---- window state ----------------------------------------------------

    /// Work out where the picture sits, for mapping the pointer.
    fn layout(&mut self) {
        self.mapper.picture = match &self.stage {
            Stage::Streaming { stream, .. } => layout::letterbox(self.gl.window.size(), stream.video_size()),
            _ => None,
        };
    }

    /// Match pointer lock, keyboard grab and cursor visibility to the stage
    /// and mode.
    fn apply_grabs(&mut self) {
        let controlling = matches!(&self.stage, Stage::Streaming { stream, .. } if stream.can_control());
        let relative = controlling && self.mapper.mode == MouseMode::Relative;
        let fullscreen = self.gl.window.fullscreen_state() != sdl3::video::FullscreenType::Off;
        let mouse = self.gl.sdl.mouse();
        mouse.set_relative_mouse_mode(&self.gl.window, relative);
        // In fullscreen or game mode, Alt+Tab, Super and friends go to the host.
        self.gl.window.set_keyboard_grab(controlling && (relative || fullscreen));
        // The host draws its own pointer into the picture.
        mouse.show_cursor(!controlling);
    }

    // ---- drawing -------------------------------------------------------

    /// Tell the render thread what to draw, if that changed.
    fn publish(&mut self) {
        let streaming = match &self.stage {
            Stage::Streaming { stream, .. } => Some(stream),
            _ => None,
        };
        let has_picture = streaming.is_some_and(|s| s.has_picture());
        let scene = Scene {
            frame_size: self.gl.window.size_in_pixels(),
            scale: self.gl.pixel_scale().1,
            notice: self.notice(has_picture),
            stats: match streaming {
                Some(stream) if self.show_stats => stream.stats().to_vec(),
                _ => Vec::new(),
            },
            hint: match (&self.warning, has_picture && self.hint_until.is_some()) {
                (_, false) => Vec::new(),
                (Some(warning), true) => wrap(warning, WRAP_COLUMNS).into_iter().map(Line::normal).collect(),
                (None, true) => vec![Line::dim(HINT)],
            },
        };
        if scene != self.scene {
            self.scene = scene.clone();
            self.render.set_scene(scene);
        }
    }

    /// The box in the middle of the window for the current stage; empty
    /// while the picture is showing.
    fn notice(&self, has_picture: bool) -> Vec<Line> {
        match &self.stage {
            Stage::Connecting { status } => {
                let mut lines = vec![Line::title("LinuxPlay"), Line::normal(status.clone())];
                if let Some(warning) = &self.warning {
                    lines.extend(wrap(warning, WRAP_COLUMNS).into_iter().map(Line::dim));
                }
                lines
            }
            Stage::Pin { host, password, typed, .. } => {
                let (what, shown) = if *password {
                    ("password", "•".repeat(typed.chars().count()))
                } else {
                    ("PIN", typed.clone())
                };
                vec![
                    Line::title(format!("Enter the {what}")),
                    Line::normal(if *password { format!("set on {host}") } else { format!("shown on {host}") }),
                    Line::title(if shown.is_empty() { "–".into() } else { shown }),
                    Line::dim("Enter to confirm · Esc to cancel"),
                ]
            }
            Stage::Displays { host, monitors, .. } => {
                let mut lines = vec![Line::title("Choose a display"), Line::normal(format!("on {host}"))];
                lines.extend(
                    monitors
                        .iter()
                        .take(9)
                        .enumerate()
                        .map(|(i, m)| Line::normal(format!("{}   {} · {}×{}", i + 1, m.name, m.width, m.height))),
                );
                lines.push(Line::dim("Press its number · Esc to cancel"));
                lines
            }
            Stage::Streaming { host, .. } if !has_picture => {
                vec![Line::title(host.clone()), Line::normal("Starting the stream…")]
            }
            Stage::Streaming { .. } => Vec::new(),
            Stage::Ended { failure } => {
                let mut lines = vec![Line::title(if failure.is_some() { "Connection ended" } else { "Disconnected" })];
                let reason = failure.as_deref().unwrap_or("The host ended the session.");
                lines.extend(wrap(reason, WRAP_COLUMNS).into_iter().map(Line::normal));
                lines.push(Line::dim("Press Esc to close"));
                lines
            }
        }
    }
}

/// The digit on a number key, from the top row or the keypad.
fn digit(key: Scancode) -> Option<usize> {
    Some(match key {
        Scancode::_1 | Scancode::Kp1 => 1,
        Scancode::_2 | Scancode::Kp2 => 2,
        Scancode::_3 | Scancode::Kp3 => 3,
        Scancode::_4 | Scancode::Kp4 => 4,
        Scancode::_5 | Scancode::Kp5 => 5,
        Scancode::_6 | Scancode::Kp6 => 6,
        Scancode::_7 | Scancode::Kp7 => 7,
        Scancode::_8 | Scancode::Kp8 => 8,
        Scancode::_9 | Scancode::Kp9 => 9,
        _ => return None,
    })
}

/// Break `text` into lines of at most `columns` characters at spaces. A
/// single word longer than that gets a line of its own.
fn wrap(text: &str, columns: usize) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    for word in text.split_whitespace() {
        match lines.last_mut() {
            Some(line) if line.chars().count() + 1 + word.chars().count() <= columns => {
                line.push(' ');
                line.push_str(word);
            }
            _ => lines.push(word.to_string()),
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn long_messages_wrap_at_spaces() {
        let lines = wrap("the host declined the connection because nobody was there to accept it", 30);
        assert!(lines.len() >= 3);
        assert!(lines.iter().all(|l| l.chars().count() <= 30));
        assert_eq!(lines.join(" "), "the host declined the connection because nobody was there to accept it");
        assert_eq!(wrap("", 10), Vec::<String>::new());
        assert_eq!(wrap("unbreakablewordthatislong", 5), vec!["unbreakablewordthatislong"]);
    }

    #[test]
    fn number_keys_choose_displays() {
        assert_eq!(digit(Scancode::_1), Some(1));
        assert_eq!(digit(Scancode::Kp9), Some(9));
        assert_eq!(digit(Scancode::_0), None);
        assert_eq!(digit(Scancode::A), None);
    }
}
