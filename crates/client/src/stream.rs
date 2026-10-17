//! The window showing a remote desktop, and the input it captures.
//!
//! Hotkeys (Ctrl+Alt+Shift+…) stay local: F toggles fullscreen, S the stats
//! overlay, Q disconnects. Everything else goes to the host.

use std::cell::{Cell, RefCell};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use adw::prelude::*;
use anyhow::Result;
use gstreamer as gst;
use gtk::{gdk, glib};
use linuxplay_net::{Channel, SessionEvent, SessionHandle, rtc};
use linuxplay_proto::control::ControlMsg;
use linuxplay_proto::input::{InputEvent, MouseButton, POINTER_MAX};
use linuxplay_proto::signal::VideoCodec;
use linuxplay_proto::{MAX_CONTROL_MESSAGE, decode, encode};
use tracing::{debug, warn};

use crate::connect::Offer;
use crate::media::{AudioOut, VideoOut};

/// X11/XKB keycodes (which GDK reports on both X11 and Wayland) are evdev + 8.
const EVDEV_OFFSET: u32 = 8;
/// Session events waiting for the GTK thread.
const EVENT_QUEUE: usize = 64;

#[derive(Default)]
struct Counters {
    frames: AtomicU64,
    bytes: AtomicU64,
}

struct HostStats {
    fps: u32,
    kbps: u32,
    encoder: String,
}

pub struct Stream {
    window: adw::ApplicationWindow,
    toolbar: adw::ToolbarView,
    picture: gtk::Picture,
    stats_label: gtk::Label,
    waiting: adw::StatusPage,
    video: VideoOut,
    _audio: Option<AudioOut>,
    _bus_watch: Option<gst::bus::BusWatchGuard>,
    session: SessionHandle,
    codec: VideoCodec,
    counters: Arc<Counters>,
    started: Instant,
    last_tick: Cell<Instant>,
    host_stats: RefCell<Option<HostStats>>,
    rtt_ms: Cell<Option<f64>>,
    remote_size: Cell<(u32, u32)>,
    /// The host lets this session use keyboard and mouse.
    control: bool,
    input_ready: Cell<bool>,
    connected: Cell<bool>,
    closed: Cell<bool>,
    cancel: async_channel::Sender<()>,
}

/// Build the stream window for `offer` and answer it. `cancel` hangs up the
/// signalling connection when the window closes.
pub fn open(
    app: &adw::Application,
    offer: Offer,
    cancel: async_channel::Sender<()>,
    on_open_display: Rc<dyn Fn(u32)>,
) -> Result<(Rc<Stream>, String)> {
    let video = VideoOut::new(offer.codec)?;
    let audio = AudioOut::new().inspect_err(|e| warn!("no audio playback: {e:#}")).ok();
    let counters = Arc::new(Counters::default());

    // Bounded: a host flooding the control channel can't grow this. The one
    // event that must arrive, Closed, displaces the oldest if it's full.
    let (tx, rx) = async_channel::bounded::<SessionEvent>(EVENT_QUEUE);
    let handler = {
        let feed = video.feed();
        let audio_feed = audio.as_ref().map(|a| a.feed());
        let counters = counters.clone();
        // Media goes straight into the pipelines from the network thread;
        // everything else is handled on the GTK thread.
        Box::new(move |ev: SessionEvent| match ev {
            SessionEvent::Video { data, .. } => {
                counters.frames.fetch_add(1, Ordering::Relaxed);
                counters.bytes.fetch_add(data.len() as u64, Ordering::Relaxed);
                feed.push(&data);
            }
            SessionEvent::Audio { data, .. } => {
                if let Some(a) = &audio_feed {
                    a.push(&data);
                }
            }
            closed @ SessionEvent::Closed(_) => drop(tx.force_send(closed)),
            SessionEvent::Data(_, bytes) if bytes.len() > MAX_CONTROL_MESSAGE => {}
            other => drop(tx.try_send(other)),
        })
    };
    let (session, answer) = rtc::accept_offer(offer.local_ip, &offer.sdp, handler)?;

    let stream = build(app, &offer, video, audio, session, counters, cancel, on_open_display);

    let weak = Rc::downgrade(&stream);
    glib::spawn_future_local(async move {
        while let Ok(ev) = rx.recv().await {
            let Some(s) = weak.upgrade() else { break };
            s.on_event(ev);
        }
    });
    let weak = Rc::downgrade(&stream);
    glib::timeout_add_local(Duration::from_secs(1), move || match weak.upgrade() {
        Some(s) if !s.closed.get() => {
            s.tick();
            glib::ControlFlow::Continue
        }
        _ => glib::ControlFlow::Break,
    });
    Ok((stream, answer))
}

fn initial_size(width: u32, height: u32) -> (i32, i32) {
    let (mw, mh) = gdk::Display::default()
        .and_then(|d| d.monitors().item(0))
        .and_downcast::<gdk::Monitor>()
        .map(|m| (m.geometry().width() as f64, m.geometry().height() as f64))
        .unwrap_or((1920.0, 1080.0));
    let (w, h) = (width.max(1) as f64, height.max(1) as f64);
    let scale = (0.85 * mw / w).min(0.85 * mh / h).min(1.0);
    ((w * scale) as i32, (h * scale) as i32)
}

fn build(
    app: &adw::Application,
    offer: &Offer,
    video: VideoOut,
    audio: Option<AudioOut>,
    session: SessionHandle,
    counters: Arc<Counters>,
    cancel: async_channel::Sender<()>,
    on_open_display: Rc<dyn Fn(u32)>,
) -> Rc<Stream> {
    let picture = gtk::Picture::for_paintable(&video.paintable);
    picture.set_content_fit(gtk::ContentFit::Contain);
    picture.set_can_shrink(true);
    picture.set_focusable(true);
    picture.set_hexpand(true);
    picture.set_vexpand(true);
    // The host draws its cursor into the picture.
    picture.set_cursor_from_name(Some("none"));

    // Lets the compositor scan out the decoded frames directly.
    let offload = gtk::GraphicsOffload::new(Some(&picture));
    offload.set_enabled(gtk::GraphicsOffloadEnabled::Enabled);

    let stats_label = gtk::Label::builder()
        .halign(gtk::Align::End)
        .valign(gtk::Align::Start)
        .margin_top(12)
        .margin_end(12)
        .can_target(false)
        .visible(false)
        .css_classes(["linuxplay-stats"])
        .build();
    let spinner = gtk::Spinner::builder().spinning(true).width_request(32).height_request(32).build();
    let waiting = adw::StatusPage::builder()
        .title("Connecting…")
        .description(format!("Waiting for {}", offer.host_name))
        .child(&spinner)
        .css_classes(["linuxplay-waiting"])
        .build();

    let overlay = gtk::Overlay::builder().child(&offload).css_classes(["linuxplay-video"]).build();
    overlay.add_overlay(&stats_label);
    overlay.add_overlay(&waiting);

    let title = adw::WindowTitle::new(&offer.host_name, "");
    let header = adw::HeaderBar::builder().title_widget(&title).build();
    let fullscreen_btn = gtk::Button::builder()
        .icon_name("view-fullscreen-symbolic")
        .tooltip_text("Fullscreen (Ctrl+Alt+Shift+F)")
        .build();
    let stats_btn = gtk::ToggleButton::builder()
        .icon_name("utilities-system-monitor-symbolic")
        .tooltip_text("Statistics (Ctrl+Alt+Shift+S)")
        .build();
    header.pack_end(&fullscreen_btn);
    header.pack_end(&stats_btn);

    // A menu to open the host's other displays, each in its own window.
    let others: Vec<_> = offer.monitors.iter().filter(|m| m.index != offer.monitor.index).cloned().collect();
    if !others.is_empty() {
        let popover = gtk::Popover::new();
        let list = gtk::Box::new(gtk::Orientation::Vertical, 4);
        for m in others {
            let button = gtk::Button::builder()
                .label(format!("{} · {}×{}", m.name, m.width, m.height))
                .css_classes(["flat"])
                .build();
            let open = on_open_display.clone();
            let popover = popover.clone();
            button.connect_clicked(move |_| {
                open(m.index);
                popover.popdown();
            });
            list.append(&button);
        }
        popover.set_child(Some(&list));
        let displays_btn = gtk::MenuButton::builder()
            .icon_name("video-display-symbolic")
            .tooltip_text("Open another display")
            .popover(&popover)
            .build();
        header.pack_end(&displays_btn);
    }

    let toolbar = adw::ToolbarView::new();
    toolbar.add_top_bar(&header);
    toolbar.set_content(Some(&overlay));

    let (w, h) = initial_size(offer.width, offer.height);
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title(format!("{} · {} — LinuxPlay", offer.monitor.name, offer.host_name))
        .default_width(w)
        .default_height(h)
        .content(&toolbar)
        .build();

    let bus_watch = {
        let host = offer.host_name.clone();
        video
            .bus()
            .add_watch_local(move |_, msg| {
                if let gst::MessageView::Error(e) = msg.view() {
                    warn!("video from {host}: {} ({:?})", e.error(), e.debug());
                }
                glib::ControlFlow::Continue
            })
            .ok()
    };
    let mode = if offer.control { "" } else { " · view only" };
    title.set_subtitle(&format!(
        "{} · {}×{} · {} fps · {} · {}{mode}",
        offer.monitor.name,
        offer.width,
        offer.height,
        offer.fps,
        offer.codec.name(),
        video.decoder
    ));

    let stream = Rc::new(Stream {
        window,
        toolbar,
        picture,
        stats_label,
        waiting,
        video,
        _audio: audio,
        _bus_watch: bus_watch,
        session,
        codec: offer.codec,
        counters,
        started: Instant::now(),
        last_tick: Cell::new(Instant::now()),
        host_stats: RefCell::new(None),
        rtt_ms: Cell::new(None),
        remote_size: Cell::new((offer.width, offer.height)),
        control: offer.control,
        input_ready: Cell::new(false),
        connected: Cell::new(false),
        closed: Cell::new(false),
        cancel,
    });

    {
        let weak = Rc::downgrade(&stream);
        fullscreen_btn.connect_clicked(move |_| {
            if let Some(s) = weak.upgrade() {
                s.toggle_fullscreen();
            }
        });
        let weak = Rc::downgrade(&stream);
        stats_btn.connect_toggled(move |b| {
            if let Some(s) = weak.upgrade() {
                s.stats_label.set_visible(b.is_active());
            }
        });
        let weak = Rc::downgrade(&stream);
        stream.window.connect_fullscreened_notify(move |w| {
            if let Some(s) = weak.upgrade() {
                s.on_fullscreen_changed(w.is_fullscreen());
            }
        });
        let weak = Rc::downgrade(&stream);
        stream.window.connect_is_active_notify(move |w| {
            if let (Some(s), false) = (weak.upgrade(), w.is_active()) {
                s.send_input(InputEvent::ReleaseAll);
            }
        });
        let weak = Rc::downgrade(&stream);
        stream.window.connect_close_request(move |_| {
            if let Some(s) = weak.upgrade() {
                s.hang_up();
            }
            glib::Propagation::Proceed
        });
    }
    attach_input(&stream, &stats_btn);
    stream.window.present();
    stream.picture.grab_focus();
    stream
}

fn attach_input(stream: &Rc<Stream>, stats_btn: &gtk::ToggleButton) {
    let motion = gtk::EventControllerMotion::new();
    let weak = Rc::downgrade(stream);
    motion.connect_motion(move |_, x, y| {
        let Some(s) = weak.upgrade() else { return };
        if s.window.is_fullscreen() {
            // Peek the header bar by touching the top edge.
            s.toolbar.set_reveal_top_bars(y < 2.0);
        }
        if let Some((nx, ny)) = s.to_remote(x, y) {
            s.send_input(InputEvent::PointerAbs { x: nx, y: ny });
        }
    });
    stream.picture.add_controller(motion);

    // Raw button events: GestureClick only tracks one button at a time.
    let buttons = gtk::EventControllerLegacy::new();
    let weak = Rc::downgrade(stream);
    buttons.connect_event(move |_, event| {
        let Some(s) = weak.upgrade() else { return glib::Propagation::Proceed };
        let pressed = match event.event_type() {
            gdk::EventType::ButtonPress => true,
            gdk::EventType::ButtonRelease => false,
            _ => return glib::Propagation::Proceed,
        };
        let Some(button) = event
            .downcast_ref::<gdk::ButtonEvent>()
            .and_then(|b| MouseButton::from_x11(b.button()))
        else {
            return glib::Propagation::Proceed;
        };
        if pressed {
            s.picture.grab_focus();
        }
        s.send_input(InputEvent::Button { button, pressed });
        glib::Propagation::Stop
    });
    stream.picture.add_controller(buttons);

    let scroll = gtk::EventControllerScroll::new(gtk::EventControllerScrollFlags::BOTH_AXES);
    let weak = Rc::downgrade(stream);
    scroll.connect_scroll(move |ctl, dx, dy| {
        let Some(s) = weak.upgrade() else { return glib::Propagation::Proceed };
        // Wheels report notches (fractions on hi-res wheels); touchpads
        // report pixels, roughly ten to a notch.
        let per_unit = match ctl.unit() {
            gdk::ScrollUnit::Wheel => 120.0,
            _ => 12.0,
        };
        let (sx, sy) = ((dx * per_unit).round() as i32, (dy * per_unit).round() as i32);
        if sx != 0 || sy != 0 {
            s.send_input(InputEvent::Scroll { dx: sx, dy: sy });
        }
        glib::Propagation::Stop
    });
    stream.picture.add_controller(scroll);

    // Keys are caught for the whole window before any widget sees them.
    let keys = gtk::EventControllerKey::new();
    keys.set_propagation_phase(gtk::PropagationPhase::Capture);
    let weak = Rc::downgrade(stream);
    let stats_btn = stats_btn.clone();
    keys.connect_key_pressed(move |_, keyval, keycode, state| {
        let Some(s) = weak.upgrade() else { return glib::Propagation::Proceed };
        let hotkey = gdk::ModifierType::CONTROL_MASK | gdk::ModifierType::ALT_MASK | gdk::ModifierType::SHIFT_MASK;
        if state.contains(hotkey) {
            match keyval.to_lower() {
                gdk::Key::f => s.toggle_fullscreen(),
                gdk::Key::s => stats_btn.set_active(!stats_btn.is_active()),
                gdk::Key::q => s.window.close(),
                _ => return s.key(keycode, true),
            }
            return glib::Propagation::Stop;
        }
        s.key(keycode, true)
    });
    let weak = Rc::downgrade(stream);
    keys.connect_key_released(move |_, _, keycode, _| {
        if let Some(s) = weak.upgrade() {
            s.key(keycode, false);
        }
    });
    stream.window.add_controller(keys);
}

impl Stream {
    fn key(&self, keycode: u32, pressed: bool) -> glib::Propagation {
        if let Some(code) = keycode.checked_sub(EVDEV_OFFSET).and_then(|c| u16::try_from(c).ok()) {
            self.send_input(InputEvent::Key { code, pressed });
        }
        glib::Propagation::Stop
    }

    fn send_input(&self, ev: InputEvent) {
        // The host ignores input from a view-only session anyway; don't send it.
        if self.control && self.input_ready.get() {
            self.session.send(Channel::Input, encode(&ev));
        }
    }

    /// Map a point on the picture to normalised host coordinates, or `None`
    /// when it falls in the letterbox around the video.
    fn to_remote(&self, x: f64, y: f64) -> Option<(u16, u16)> {
        let (pw, ph) = (self.picture.width() as f64, self.picture.height() as f64);
        let paintable = &self.video.paintable;
        let (vw, vh) = match (paintable.intrinsic_width(), paintable.intrinsic_height()) {
            (w, h) if w > 0 && h > 0 => (w as f64, h as f64),
            _ => {
                let (w, h) = self.remote_size.get();
                (w as f64, h as f64)
            }
        };
        if pw <= 0.0 || ph <= 0.0 || vw <= 0.0 || vh <= 0.0 {
            return None;
        }
        let scale = (pw / vw).min(ph / vh);
        let (dw, dh) = (vw * scale, vh * scale);
        let (ox, oy) = ((pw - dw) / 2.0, (ph - dh) / 2.0);
        let (nx, ny) = ((x - ox) / dw, (y - oy) / dh);
        if !(0.0..=1.0).contains(&nx) || !(0.0..=1.0).contains(&ny) {
            return None;
        }
        let max = POINTER_MAX as f64;
        Some(((nx * max).round() as u16, (ny * max).round() as u16))
    }

    fn toggle_fullscreen(&self) {
        if self.window.is_fullscreen() {
            self.window.unfullscreen();
        } else {
            self.window.fullscreen();
        }
    }

    fn on_fullscreen_changed(&self, fullscreen: bool) {
        self.toolbar.set_extend_content_to_top_edge(fullscreen);
        self.toolbar.set_reveal_top_bars(!fullscreen);
        // In fullscreen, Alt+Tab, Super and friends go to the host.
        if let Some(toplevel) = self.window.surface().and_downcast::<gdk::Toplevel>() {
            if fullscreen {
                toplevel.inhibit_system_shortcuts(None::<&gdk::Event>);
            } else {
                toplevel.restore_system_shortcuts();
            }
        }
    }

    fn on_event(&self, ev: SessionEvent) {
        match ev {
            SessionEvent::Connected => {
                self.connected.set(true);
                self.waiting.set_visible(false);
            }
            SessionEvent::ChannelOpen(Channel::Input) => self.input_ready.set(true),
            SessionEvent::Data(Channel::Control, bytes) => match decode::<ControlMsg>(&bytes) {
                Ok(ControlMsg::VideoSize { width, height }) => self.remote_size.set((width, height)),
                Ok(ControlMsg::Pong { t }) => {
                    let now = self.started.elapsed().as_micros() as u64;
                    self.rtt_ms.set(Some(now.saturating_sub(t) as f64 / 1000.0));
                }
                Ok(ControlMsg::Stats { bitrate_kbps, fps, encoder }) => {
                    *self.host_stats.borrow_mut() = Some(HostStats { fps, kbps: bitrate_kbps, encoder });
                }
                Ok(m) => debug!("control: {m:?}"),
                Err(e) => debug!("control: {e}"),
            },
            SessionEvent::Closed(err) => {
                if let Some(e) = err {
                    warn!("session closed: {e}");
                }
                self.close();
            }
            _ => {}
        }
    }

    /// Once a second: probe the round trip and refresh the stats overlay.
    fn tick(&self) {
        if self.connected.get() {
            let t = self.started.elapsed().as_micros() as u64;
            self.session.send(Channel::Control, encode(&ControlMsg::Ping { t }));
        }
        let elapsed = self.last_tick.replace(Instant::now()).elapsed().as_secs_f64().max(0.001);
        let fps = self.counters.frames.swap(0, Ordering::Relaxed) as f64 / elapsed;
        let mbps = self.counters.bytes.swap(0, Ordering::Relaxed) as f64 * 8.0 / 1e6 / elapsed;
        let (w, h) = self.remote_size.get();
        let rtt = self.rtt_ms.get().map(|r| format!("{r:.1} ms")).unwrap_or_else(|| "–".into());
        let mut text = format!("{w}×{h}  {fps:.0} fps  {mbps:.1} Mbit/s  RTT {rtt}");
        if let Some(h) = &*self.host_stats.borrow() {
            text.push_str(&format!(
                "\n{} {} → {}  host {} fps, {:.1} Mbit/s",
                self.codec.name(),
                h.encoder,
                self.video.decoder,
                h.fps,
                h.kbps as f64 / 1000.0
            ));
        }
        self.stats_label.set_label(&text);
    }

    /// End the session from our side.
    fn hang_up(&self) {
        if !self.closed.replace(true) {
            self.session.close();
            let _ = self.cancel.try_send(());
        }
    }

    /// Close the window (and with it the session).
    pub fn close(&self) {
        self.hang_up();
        self.window.close();
    }
}
