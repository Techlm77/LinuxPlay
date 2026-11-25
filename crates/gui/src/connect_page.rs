//! "Connect": open a stream to another computer, by address, by the name of
//! one of the account's devices, or from an invite link.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use linuxplay_proto::api::{self, method};
use linuxplay_proto::rendezvous::Device;

use crate::ui::Ui;
use crate::util::{RowList, ago, icon_button, row_button};

/// How often the account's device list is read again while the window shows.
const DEVICES_REFRESH_SECONDS: u32 = 15;

pub struct ConnectPage {
    pub widget: adw::PreferencesPage,
    ui: Rc<Ui>,
    devices: RowList,
    recent: RowList,
    streams: RowList,
    /// What was last drawn, so lists are only rebuilt when they change.
    shown_account: RefCell<Option<Option<api::SignedIn>>>,
    shown_streams: RefCell<Option<Vec<api::Connection>>>,
}

impl ConnectPage {
    pub fn new(ui: &Rc<Ui>) -> Rc<Self> {
        let widget = adw::PreferencesPage::new();

        let group = adw::PreferencesGroup::builder()
            .title("Connect to a computer")
            .description("Its address on your network or tailnet, the name of one of your devices, or an invite link.")
            .build();
        let target = adw::EntryRow::builder().title("Address, device name or invite link").build();
        let go = row_button("Connect", Some("suggested-action"));
        target.add_suffix(&go);
        group.add(&target);

        let options = adw::ExpanderRow::builder().title("Stream options").build();
        let settings = ui.settings.borrow().clone();
        let fullscreen = switch("Start in fullscreen", None, settings.fullscreen);
        let game_mouse = switch("Lock the mouse for games", Some("Ctrl+Alt+Shift+M switches it in the stream window"), settings.game_mouse);
        let stats = switch("Show statistics", Some("Latency, frame rate and bitrate over the picture"), settings.stats);
        for row in [&fullscreen, &game_mouse, &stats] {
            options.add_row(row);
        }
        group.add(&options);
        widget.add(&group);
        {
            let ui = ui.clone();
            fullscreen.connect_active_notify(move |r| ui.update_settings(|s| s.fullscreen = r.is_active()));
        }
        {
            let ui = ui.clone();
            game_mouse.connect_active_notify(move |r| ui.update_settings(|s| s.game_mouse = r.is_active()));
        }
        {
            let ui = ui.clone();
            stats.connect_active_notify(move |r| ui.update_settings(|s| s.stats = r.is_active()));
        }

        let streams = RowList::new("Open streams", None, None);
        let devices = RowList::new("Your devices", Some("Signed in to the same account."), None);
        let recent = RowList::new("Recent", Some("Computers this one has paired with."), Some("None yet"));
        let refresh = icon_button("view-refresh-symbolic", "Look again");
        devices.group.set_header_suffix(Some(&refresh));
        widget.add(&streams.group);
        widget.add(&devices.group);
        widget.add(&recent.group);

        let page = Rc::new(Self {
            widget,
            ui: ui.clone(),
            devices,
            recent,
            streams,
            shown_account: RefCell::new(None),
            shown_streams: RefCell::new(None),
        });

        let connect_typed = {
            let page = page.clone();
            let target = target.clone();
            move || {
                let text = target.text().trim().to_string();
                if !text.is_empty() {
                    page.connect(&text);
                }
            }
        };
        let c = connect_typed.clone();
        go.connect_clicked(move |_| c());
        target.connect_entry_activated(move |_| connect_typed());
        {
            let page = page.clone();
            refresh.connect_clicked(move |_| page.load_devices());
        }
        {
            let page = Rc::downgrade(&page);
            glib::timeout_add_seconds_local(DEVICES_REFRESH_SECONDS, move || {
                let Some(page) = page.upgrade() else { return glib::ControlFlow::Break };
                if page.ui.window.is_visible() {
                    page.refresh();
                }
                glib::ControlFlow::Continue
            });
        }
        page
    }

    /// Read the device list and the recent hosts again (they change outside
    /// this window: devices come online, a stream from the command line pairs).
    pub fn refresh(self: &Rc<Self>) {
        if self.ui.state.borrow().account.is_some() {
            self.load_devices();
        }
        self.load_recent();
    }

    /// Open a stream window to `target`.
    pub fn connect(self: &Rc<Self>, target: &str) {
        let s = self.ui.settings.borrow().clone();
        let request = api::Connect {
            target: target.to_string(),
            display: None,
            pin: None,
            fullscreen: s.fullscreen,
            stats: s.stats,
            game_mouse: s.game_mouse,
        };
        let ui = self.ui.clone();
        self.ui.spawn(async move {
            let _: api::Connection = ui.call(method::CONNECT, request).await?;
            ui.toast("Opening the stream window…");
            Ok(())
        });
    }

    pub fn update(self: &Rc<Self>, state: &api::State) {
        if self.shown_account.borrow().as_ref() != Some(&state.account) {
            *self.shown_account.borrow_mut() = Some(state.account.clone());
            if state.account.is_some() {
                self.load_devices();
            } else {
                self.devices.set(Vec::new());
            }
        }
        if self.shown_streams.borrow().as_ref() != Some(&state.connections) {
            // A stream that just ended may have paired with a new host.
            *self.shown_streams.borrow_mut() = Some(state.connections.clone());
            self.show_streams(&state.connections);
            self.load_recent();
        }
    }

    fn show_streams(self: &Rc<Self>, connections: &[api::Connection]) {
        let rows = connections
            .iter()
            .map(|c| {
                let row = adw::ActionRow::builder().title(glib::markup_escape_text(&c.target)).build();
                row.add_prefix(&gtk::Image::from_icon_name("video-display-symbolic"));
                let close = row_button("Close", None);
                let (ui, id) = (self.ui.clone(), c.id);
                close.connect_clicked(move |_| {
                    let ui2 = ui.clone();
                    ui.spawn(async move { ui2.call::<serde_json::Value>(method::DISCONNECT, api::ById { id }).await.map(drop) });
                });
                row.add_suffix(&close);
                row.upcast()
            })
            .collect();
        self.streams.set(rows);
    }

    fn load_devices(self: &Rc<Self>) {
        let page = self.clone();
        glib::spawn_future_local(async move {
            match page.ui.call::<Vec<Device>>(method::ACCOUNT_DEVICES, ()).await {
                Ok(devices) => page.show_devices(&devices),
                Err(e) => page.devices.set(vec![message_row(&format!("Can't reach the account server: {e}"))]),
            }
        });
    }

    fn show_devices(self: &Rc<Self>, devices: &[Device]) {
        let mut sorted: Vec<&Device> = devices.iter().collect();
        sorted.sort_by_key(|d| (d.current, !d.online, d.name.to_lowercase()));
        let rows = sorted
            .into_iter()
            .map(|d| {
                let status = if d.current {
                    "This computer".to_string()
                } else if d.online {
                    "Online".to_string()
                } else {
                    format!("Offline · seen {}", ago(d.last_seen))
                };
                let row = adw::ActionRow::builder().title(glib::markup_escape_text(&d.name)).subtitle(status).build();
                let icon = gtk::Image::from_icon_name("computer-symbolic");
                if d.online && !d.current {
                    icon.add_css_class("success");
                }
                row.add_prefix(&icon);
                if !d.current {
                    let go = row_button("Connect", None);
                    go.set_sensitive(d.online);
                    let (page, name) = (self.clone(), d.name.clone());
                    go.connect_clicked(move |_| page.connect(&name));
                    row.add_suffix(&go);
                }
                row.upcast()
            })
            .collect();
        self.devices.set(rows);
    }

    fn load_recent(self: &Rc<Self>) {
        let page = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(hosts) = page.ui.call::<Vec<api::PairedHost>>(method::HOSTS_LIST, ()).await {
                page.show_recent(&hosts);
            }
        });
    }

    fn show_recent(self: &Rc<Self>, hosts: &[api::PairedHost]) {
        let rows = hosts
            .iter()
            .map(|h| {
                let title = if h.name.is_empty() { &h.address } else { &h.name };
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(title))
                    .subtitle(glib::markup_escape_text(&format!("{} · used {}", h.address, ago(h.last_used))))
                    .build();
                row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
                let forget = icon_button("user-trash-symbolic", "Forget this computer");
                let go = row_button("Connect", None);
                row.add_suffix(&forget);
                row.add_suffix(&go);
                {
                    let (page, address) = (self.clone(), h.address.clone());
                    go.connect_clicked(move |_| page.connect(&address));
                }
                {
                    let (page, address, name) = (self.clone(), h.address.clone(), title.clone());
                    forget.connect_clicked(move |_| {
                        let (page, address) = (page.clone(), address.clone());
                        page.ui.clone().confirm(
                            &format!("Forget {name}?"),
                            "Connecting to it again will pair from scratch, with a new PIN. Do this when it was reinstalled.",
                            "Forget",
                            move || {
                                let (page, address) = (page.clone(), address.clone());
                                page.ui.clone().spawn(async move {
                                    let _: api::PairedHost = page.ui.call(method::HOSTS_FORGET, api::ById { id: address }).await?;
                                    page.load_recent();
                                    Ok(())
                                });
                            },
                        );
                    });
                }
                row.upcast()
            })
            .collect();
        self.recent.set(rows);
    }
}

fn switch(title: &str, subtitle: Option<&str>, active: bool) -> adw::SwitchRow {
    let row = adw::SwitchRow::builder().title(title).active(active).build();
    if let Some(s) = subtitle {
        row.set_subtitle(s);
    }
    row
}

fn message_row(text: &str) -> gtk::Widget {
    let row = adw::ActionRow::builder().title(glib::markup_escape_text(text)).build();
    row.add_css_class("dim-label");
    row.upcast()
}
