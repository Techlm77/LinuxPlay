//! The main window: type an address or pick a recent host.

use std::cell::RefCell;
use std::rc::Rc;
use std::sync::Arc;

use adw::prelude::*;
use gtk::glib;
use linuxplay_net::tls::{self, Identity};
use tracing::warn;

use crate::connect::{self, ToUi};
use crate::known_hosts::KnownHosts;
use crate::stream::{self, Stream};
use crate::{media, runtime};

struct Active {
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
    active: RefCell<Option<Active>>,
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
            active: RefCell::default(),
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
                if l.active.borrow().is_some() {
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
        if let Some(a) = self.active.borrow().as_ref() {
            let _ = a.cancel.try_send(());
        }
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

    pub fn connect(self: &Rc<Self>, address: &str) {
        let address = address.trim().to_string();
        if address.is_empty() || self.active.borrow().is_some() {
            return;
        }
        let codecs = media::decodable();
        if codecs.is_empty() {
            self.error("Can't decode video", "No H.264, H.265 or AV1 decoder is installed.");
            return;
        }
        let pinned = self.known.borrow().get(&address).map(|h| h.fingerprint.clone());
        let req = connect::Request {
            address,
            pinned,
            client_name: hostname(),
            hw_codecs: media::hw_decodable(),
            max_fps: self.refresh_rate(),
            codecs,
        };

        let (ui_tx, ui_rx) = async_channel::unbounded();
        let (cancel_tx, cancel_rx) = async_channel::bounded(1);
        *self.active.borrow_mut() = Some(Active { cancel: cancel_tx, stream: None });
        self.set_busy(true, "Connecting…");
        runtime().spawn(connect::run(req, self.identity.clone(), ui_tx, cancel_rx));

        let this = self.clone();
        glib::spawn_future_local(async move {
            while let Ok(msg) = ui_rx.recv().await {
                this.on_message(msg);
            }
        });
    }

    fn on_message(self: &Rc<Self>, msg: ToUi) {
        match msg {
            ToUi::Status(s) => self.set_busy(true, &s),
            ToUi::NeedPin { host_name, password, reply } => self.ask_pin(&host_name, password, reply),
            ToUi::Verified { address, host_name, fingerprint } => {
                if let Err(e) = self.known.borrow_mut().remember(&address, &host_name, &fingerprint) {
                    warn!("saving {address}: {e:#}");
                }
                self.refresh_recent();
            }
            ToUi::Offer(offer, reply) => {
                let Some(cancel) = self.active.borrow().as_ref().map(|a| a.cancel.clone()) else {
                    let _ = reply.send(Err(anyhow::anyhow!("cancelled")));
                    return;
                };
                match stream::open(&self.app, offer, cancel) {
                    Ok((stream, answer)) => {
                        if let Some(a) = self.active.borrow_mut().as_mut() {
                            a.stream = Some(stream);
                        }
                        let _ = reply.send(Ok(answer));
                        self.window.set_visible(false);
                    }
                    Err(e) => {
                        let _ = reply.send(Err(e));
                    }
                }
            }
            ToUi::Ended(err) => {
                if let Some(stream) = self.active.take().and_then(|a| a.stream) {
                    stream.close();
                }
                self.set_busy(false, "");
                self.window.present();
                if let Some(e) = err {
                    self.error("Connection ended", &e);
                }
            }
        }
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
