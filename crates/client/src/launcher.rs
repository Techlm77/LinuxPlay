//! The main window: type an address or pick a recent host.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use linuxplay_net::tls::{self, Identity};
use linuxplay_proto::signal::MonitorInfo;
use tracing::warn;

use crate::connect::{self, ToUi};
use crate::known_hosts::KnownHosts;
use crate::stream::{self, Stream};
use crate::{media, runtime};

/// One connection to a host (one display in one window).
struct Conn {
    address: String,
    cancel: async_channel::Sender<()>,
    stream: Option<Rc<Stream>>,
}

pub struct Launcher {
    app: adw::Application,
    window: adw::ApplicationWindow,
    entry: adw::EntryRow,
    connect_btn: gtk::Button,
    status: gtk::Label,
    recent: adw::PreferencesGroup,
    recent_rows: RefCell<Vec<adw::ActionRow>>,
    known: RefCell<KnownHosts>,
    identity: Arc<Identity>,
    /// Every live connection, keyed by a counter.
    conns: RefCell<HashMap<u64, Conn>>,
    /// The connection whose progress the launcher button reflects.
    connecting: Cell<Option<u64>>,
    next_id: Cell<u64>,
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "LinuxPlay client".into())
}

impl Launcher {
    pub fn new(app: &adw::Application, identity: Identity) -> Rc<Self> {
        let entry = adw::EntryRow::builder().title("Host address").build();
        entry.set_input_purpose(gtk::InputPurpose::Url);
        let connect_btn = gtk::Button::builder()
            .label("Connect")
            .valign(gtk::Align::Center)
            .css_classes(["suggested-action"])
            .build();
        entry.add_suffix(&connect_btn);

        let connect_group = adw::PreferencesGroup::builder()
            .title("Connect")
            .description("The name or IP address of a computer running linuxplay-host")
            .build();
        connect_group.add(&entry);

        let status = gtk::Label::builder()
            .css_classes(["dim-label"])
            .wrap(true)
            .visible(false)
            .build();

        let recent = adw::PreferencesGroup::builder().title("Paired hosts").build();

        let fingerprint = tls::fingerprint_display(&identity.fingerprint);
        let short: String = fingerprint.chars().take(19).collect();
        let me = gtk::Label::builder()
            .label(format!("This device: {} · {short}…", hostname()))
            .tooltip_text(format!("Certificate fingerprint\n{fingerprint}"))
            .css_classes(["dim-label", "caption"])
            .build();

        let content = gtk::Box::builder()
            .orientation(gtk::Orientation::Vertical)
            .spacing(24)
            .margin_top(24)
            .margin_bottom(24)
            .margin_start(12)
            .margin_end(12)
            .build();
        content.append(&connect_group);
        content.append(&status);
        content.append(&recent);
        content.append(&me);

        let clamp = adw::Clamp::builder().maximum_size(560).child(&content).build();
        let scroller = gtk::ScrolledWindow::builder()
            .hscrollbar_policy(gtk::PolicyType::Never)
            .child(&clamp)
            .vexpand(true)
            .build();
        let toolbar = adw::ToolbarView::new();
        toolbar.add_top_bar(&adw::HeaderBar::new());
        toolbar.set_content(Some(&scroller));

        let window = adw::ApplicationWindow::builder()
            .application(app)
            .title("LinuxPlay")
            .default_width(520)
            .default_height(560)
            .content(&toolbar)
            .build();

        let known = KnownHosts::load(tls::config_dir("client").join("known_hosts.toml"));
        let this = Rc::new(Self {
            app: app.clone(),
            window,
            entry,
            connect_btn,
            status,
            recent,
            recent_rows: RefCell::default(),
            known: RefCell::new(known),
            identity: Arc::new(identity),
            conns: RefCell::default(),
            connecting: Cell::new(None),
            next_id: Cell::new(1),
        });

        let weak = Rc::downgrade(&this);
        this.entry.connect_entry_activated(move |e| {
            if let Some(l) = weak.upgrade() {
                l.connect(&e.text());
            }
        });
        let weak = Rc::downgrade(&this);
        this.connect_btn.connect_clicked(move |_| {
            if let Some(l) = weak.upgrade() {
                if l.connecting.get().is_some() {
                    l.cancel();
                } else {
                    l.connect(&l.entry.text());
                }
            }
        });
        this.refresh_recent();
        this
    }

    pub fn present(&self) {
        self.window.present();
    }

    /// Test hook: fill the address and click Connect exactly as a user would,
    /// so the button's own handler runs.
    pub fn test_click_connect(&self, addr: &str) {
        self.entry.set_text(addr);
        self.connect_btn.emit_clicked();
    }

    fn refresh_recent(self: &Rc<Self>) {
        for row in self.recent_rows.borrow_mut().drain(..) {
            self.recent.remove(&row);
        }
        let hosts = self.known.borrow().recent();
        self.recent.set_visible(!hosts.is_empty());
        for host in hosts {
            let row = adw::ActionRow::builder()
                .title(glib::markup_escape_text(&host.name))
                .subtitle(glib::markup_escape_text(&host.address))
                .activatable(true)
                .build();
            row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
            let forget = gtk::Button::builder()
                .icon_name("user-trash-symbolic")
                .tooltip_text("Forget this host")
                .valign(gtk::Align::Center)
                .css_classes(["flat"])
                .build();
            row.add_suffix(&forget);

            let weak = Rc::downgrade(self);
            let address = host.address.clone();
            row.connect_activated(move |_| {
                if let Some(l) = weak.upgrade() {
                    l.entry.set_text(&address);
                    l.connect(&address);
                }
            });
            let weak = Rc::downgrade(self);
            let address = host.address.clone();
            forget.connect_clicked(move |_| {
                if let Some(l) = weak.upgrade() {
                    if let Err(e) = l.known.borrow_mut().forget(&address) {
                        warn!("forgetting {address}: {e:#}");
                    }
                    l.refresh_recent();
                }
            });
            self.recent.add(&row);
            self.recent_rows.borrow_mut().push(row);
        }
    }

    fn set_busy(&self, busy: bool, status: &str) {
        self.entry.set_sensitive(!busy);
        self.connect_btn.set_label(if busy { "Cancel" } else { "Connect" });
        if busy {
            self.connect_btn.remove_css_class("suggested-action");
        } else {
            self.connect_btn.add_css_class("suggested-action");
        }
        self.status.set_label(status);
        self.status.set_visible(!status.is_empty());
    }

    fn cancel(&self) {
        if let Some(id) = self.connecting.get() {
            if let Some(c) = self.conns.borrow().get(&id) {
                let _ = c.cancel.try_send(());
            }
        }
    }

    fn is_foreground(&self, id: u64) -> bool {
        self.connecting.get() == Some(id)
    }

    fn has_active_stream(&self) -> bool {
        self.conns.borrow().values().any(|c| c.stream.is_some())
    }

    /// The main monitor's refresh rate in Hz (default 60), so the host won't
    /// send more frames than this screen can show.
    fn refresh_rate(&self) -> u32 {
        gtk::gdk::Display::default()
            .and_then(|d| d.monitors().item(0))
            .and_downcast::<gtk::gdk::Monitor>()
            .map(|m| m.refresh_rate())
            .filter(|r| *r > 0)
            .map(|milli_hz| (milli_hz as f64 / 1000.0).round() as u32)
            .unwrap_or(60)
            .clamp(24, 240)
    }

    /// Connect to `address` and let the user pick a display.
    pub fn connect(self: &Rc<Self>, address: &str) {
        if self.connecting.get().is_some() {
            return;
        }
        if let Some(id) = self.start(address, None) {
            self.connecting.set(Some(id));
            self.set_busy(true, "Connecting…");
        }
    }

    /// Start one connection for a specific display (or `None` to be asked).
    /// Returns its id, or `None` if it couldn't start.
    fn start(self: &Rc<Self>, address: &str, monitor: Option<u32>) -> Option<u64> {
        let address = address.trim().to_string();
        if address.is_empty() {
            return None;
        }
        let codecs = media::decodable();
        if codecs.is_empty() {
            self.error("Can't decode video", "No H.264, H.265 or AV1 decoder is installed.");
            return None;
        }
        let pinned = self.known.borrow().get(&address).map(|h| h.fingerprint.clone());
        let req = connect::Request {
            address: address.clone(),
            pinned,
            client_name: hostname(),
            hw_codecs: media::hw_decodable(),
            max_fps: self.refresh_rate(),
            codecs,
            monitor,
        };

        let (ui_tx, ui_rx) = async_channel::unbounded();
        let (cancel_tx, cancel_rx) = async_channel::bounded(1);
        let id = self.next_id.get();
        self.next_id.set(id + 1);
        self.conns.borrow_mut().insert(id, Conn { address, cancel: cancel_tx, stream: None });
        runtime().spawn(connect::run(req, self.identity.clone(), ui_tx, cancel_rx));

        let this = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(msg) = ui_rx.recv().await {
                this.on_message(id, msg);
            }
        });
        Some(id)
    }

    fn on_message(self: &Rc<Self>, id: u64, msg: ToUi) {
        match msg {
            ToUi::Status(s) => {
                if self.is_foreground(id) {
                    self.set_busy(true, &s);
                }
            }
            ToUi::NeedPin { host_name, password, reply } => self.ask_pin(&host_name, password, reply),
            ToUi::NeedMonitor { host_name, monitors, reply } => self.ask_monitor(&host_name, monitors, reply),
            ToUi::Verified { address, host_name, fingerprint } => {
                if let Err(e) = self.known.borrow_mut().remember(&address, &host_name, &fingerprint) {
                    warn!("saving {address}: {e:#}");
                }
                self.refresh_recent();
            }
            ToUi::Offer(offer, reply) => {
                let found = {
                    let conns = self.conns.borrow();
                    conns.get(&id).map(|c| (c.cancel.clone(), c.address.clone()))
                };
                let Some((cancel, address)) = found else {
                    let _ = reply.send(Err(anyhow::anyhow!("cancelled")));
                    return;
                };
                let this = self.clone();
                let on_open_display = Rc::new(move |index: u32| {
                    let _ = this.start(&address, Some(index));
                });
                match stream::open(&self.app, offer, cancel, on_open_display) {
                    Ok((stream, answer)) => {
                        if let Some(c) = self.conns.borrow_mut().get_mut(&id) {
                            c.stream = Some(stream);
                        }
                        let _ = reply.send(Ok(answer));
                        if self.is_foreground(id) {
                            self.connecting.set(None);
                            self.set_busy(false, "");
                        }
                        // The stream window is now the focus; get the launcher
                        // out of the way so it's clear something happened.
                        self.window.set_visible(false);
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            ToUi::Ended(err) => {
                if let Some(stream) = self.conns.borrow_mut().remove(&id).and_then(|c| c.stream) {
                    stream.close();
                }
                if self.is_foreground(id) {
                    self.connecting.set(None);
                    self.set_busy(false, "");
                }
                // Bring the launcher back when the last window closes, or to
                // show an error.
                if err.is_some() || !self.has_active_stream() {
                    self.window.present();
                }
                if let Some(e) = err {
                    self.error("Connection ended", &e);
                }
            }
        }
    }

    /// Ask which display to open, one button per monitor.
    fn ask_monitor(&self, host_name: &str, monitors: Vec<MonitorInfo>, reply: tokio::sync::oneshot::Sender<Option<u32>>) {
        if let Ok(idx) = std::env::var("LINUXPLAY_MONITOR").map(|s| s.trim().parse::<u32>()) {
            let _ = reply.send(idx.ok());
            return;
        }
        let dialog = adw::AlertDialog::new(
            Some("Choose a display"),
            Some(&format!("{host_name} is sharing {} displays.", monitors.len())),
        );
        for m in &monitors {
            let tag = if m.primary { " (primary)" } else { "" };
            dialog.add_response(&m.index.to_string(), &format!("{} · {}×{}{tag}", m.name, m.width, m.height));
        }
        dialog.add_response("cancel", "Cancel");
        dialog.set_close_response("cancel");
        if let Some(primary) = monitors.iter().find(|m| m.primary).or_else(|| monitors.first()) {
            dialog.set_default_response(Some(&primary.index.to_string()));
        }
        let reply = RefCell::new(Some(reply));
        dialog.connect_response(None, move |_, response| {
            if let Some(r) = reply.take() {
                let _ = r.send(response.parse::<u32>().ok());
            }
        });
        dialog.present(Some(&self.window));
    }

    fn ask_pin(&self, host_name: &str, password: bool, reply: tokio::sync::oneshot::Sender<Option<String>>) {
        // For scripted tests against a host started with --password.
        if let Ok(pin) = std::env::var("LINUXPLAY_PIN") {
            let _ = reply.send(Some(pin));
            return;
        }
        let body = if password {
            "Enter the password set on the host."
        } else {
            "Enter the PIN shown on the host. It is also in the host's log."
        };
        let dialog = adw::AlertDialog::new(Some(&format!("Pair with {host_name}")), Some(body));
        let entry = gtk::PasswordEntry::builder().activates_default(true).show_peek_icon(true).build();
        dialog.set_extra_child(Some(&entry));
        dialog.add_responses(&[("cancel", "Cancel"), ("pair", "Pair")]);
        dialog.set_response_appearance("pair", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("pair"));
        dialog.set_close_response("cancel");
        let reply = RefCell::new(Some(reply));
        dialog.connect_response(None, move |_, response| {
            if let Some(r) = reply.take() {
                let _ = r.send((response == "pair").then(|| entry.text().to_string()));
            }
        });
        dialog.present(Some(&self.window));
    }

    fn error(&self, heading: &str, body: &str) {
        let dialog = adw::AlertDialog::new(Some(heading), Some(body));
        dialog.add_response("close", "Close");
        dialog.present(Some(&self.window));
    }
}
