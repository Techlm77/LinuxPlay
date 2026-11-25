//! "Share this computer": start and stop sharing, see who is connected, make
//! invite links and manage the devices that may connect.

use std::cell::{Cell, RefCell};
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use linuxplay_proto::api::{self, method};

use crate::ui::Ui;
use crate::util::{RowList, fingerprint, icon_button, minutes_left, row_button};

/// Invite lengths offered, in minutes; the first is the default.
const INVITE_MINUTES: (f64, f64, f64) = (30.0, 5.0, 1440.0);

pub struct SharePage {
    pub widget: adw::PreferencesPage,
    ui: Rc<Ui>,
    sharing: adw::SwitchRow,
    /// Set while the page itself moves the switch, so that isn't taken as the user's.
    quiet: Cell<bool>,
    busy: Cell<bool>,
    spinner: gtk::Spinner,
    account_server: adw::ActionRow,
    pins: RowList,
    waiting: RowList,
    sessions: RowList,
    disconnect_all: gtk::Button,
    invites: RowList,
    invite_group: adw::PreferencesGroup,
    new_invite: gtk::Button,
    trusted: RowList,
    shown: RefCell<Option<Option<api::Hosting>>>,
}

impl SharePage {
    pub fn new(ui: &Rc<Ui>) -> Rc<Self> {
        let widget = adw::PreferencesPage::new();
        let s = ui.settings.borrow().clone();

        let main = adw::PreferencesGroup::new();
        let sharing = adw::SwitchRow::builder()
            .title("Share this computer")
            .subtitle("Paired devices and guests with an invite can connect to see and use it")
            .build();
        let spinner = gtk::Spinner::builder().visible(false).build();
        sharing.add_suffix(&spinner);
        main.add(&sharing);
        let account_server = adw::ActionRow::builder().title("Your devices sign in at").subtitle_selectable(true).visible(false).build();
        let copy_server = icon_button("edit-copy-symbolic", "Copy");
        account_server.add_suffix(&copy_server);
        main.add(&account_server);
        widget.add(&main);

        let options = adw::PreferencesGroup::builder()
            .title("Options")
            .description("Changing one while sharing restarts sharing, which ends the open sessions.")
            .build();
        let unattended = option_row(
            "Let paired devices in without asking",
            "For a computer nobody sits at. Each connection still shows a notification, and new devices still need a PIN.",
            s.unattended,
        );
        let audio = option_row("Share sound", "Send this computer's sound to whoever is connected", s.audio);
        let virtual_display = option_row(
            "Offer a virtual screen",
            "Wayland: a screen that exists only for the stream, so a computer without a monitor can be used",
            s.virtual_display,
        );
        let account = option_row(
            "Be the account server for my devices",
            "Over Tailscale: your other devices sign in to this computer and find each other by name",
            s.account_server,
        );
        for row in [&unattended, &audio, &virtual_display, &account] {
            options.add(row);
        }
        widget.add(&options);

        let pins = RowList::new("Pairing", Some("Type this PIN on the device that is pairing."), None);
        let waiting = RowList::new("Waiting to connect", None, None);
        let sessions = RowList::new("Connected", None, None);
        let disconnect_all = gtk::Button::builder().label("Disconnect all").valign(gtk::Align::Center).build();
        disconnect_all.add_css_class("flat");
        sessions.group.set_header_suffix(Some(&disconnect_all));

        let invite_group = adw::PreferencesGroup::builder()
            .title("Invite a guest")
            .description("A link that lets someone connect once, without an account, until it expires.")
            .visible(false)
            .build();
        let new_invite = gtk::Button::builder().label("New link…").valign(gtk::Align::Center).build();
        invite_group.set_header_suffix(Some(&new_invite));
        let invites = RowList::new("Invite links", None, None);
        let trusted = RowList::new("Trusted devices", Some("Devices that have paired with this computer."), Some("None yet"));
        trusted.group.set_visible(false);
        for g in [&pins.group, &waiting.group, &sessions.group, &invite_group, &invites.group, &trusted.group] {
            widget.add(g);
        }

        let page = Rc::new(Self {
            widget,
            ui: ui.clone(),
            sharing,
            quiet: Cell::new(false),
            busy: Cell::new(false),
            spinner,
            account_server,
            pins,
            waiting,
            sessions,
            disconnect_all,
            invites,
            invite_group,
            new_invite: new_invite.clone(),
            trusted,
            shown: RefCell::new(None),
        });

        {
            let page = page.clone();
            page.sharing.clone().connect_active_notify(move |row| {
                if !page.quiet.get() {
                    page.set_sharing(row.is_active());
                }
            });
        }
        let on_option = |row: &adw::SwitchRow, set: fn(&mut crate::settings::Settings, bool)| {
            let page = page.clone();
            row.connect_active_notify(move |row| {
                page.ui.update_settings(|s| set(s, row.is_active()));
                if page.ui.state.borrow().hosting.is_some() {
                    page.set_sharing(true);
                }
            });
        };
        on_option(&unattended, |s, v| s.unattended = v);
        on_option(&audio, |s, v| s.audio = v);
        on_option(&virtual_display, |s, v| s.virtual_display = v);
        on_option(&account, |s, v| s.account_server = v);
        {
            let page = page.clone();
            copy_server.connect_clicked(move |_| {
                page.ui.window.clipboard().set_text(&page.account_server.subtitle().unwrap_or_default());
                page.ui.toast("Copied");
            });
        }
        {
            let page = page.clone();
            page.disconnect_all.clone().connect_clicked(move |_| {
                let page = page.clone();
                page.ui.clone().confirm("Disconnect everyone?", "Every open session ends now.", "Disconnect", move || {
                    let ui = page.ui.clone();
                    page.ui.spawn(async move { ui.call::<u64>(method::HOST_DISCONNECT, api::Disconnect { id: None }).await.map(drop) });
                });
            });
        }
        {
            let page = page.clone();
            new_invite.connect_clicked(move |_| page.ask_for_invite());
        }
        page
    }

    /// Start (or restart, with the current options) or stop sharing.
    fn set_sharing(self: &Rc<Self>, on: bool) {
        if self.busy.replace(true) {
            return;
        }
        self.spinner.set_visible(true);
        self.spinner.start();
        self.sharing.set_sensitive(false);
        let page = self.clone();
        glib::spawn_future_local(async move {
            let result = if on {
                let s = page.ui.settings.borrow().clone();
                let options = api::StartHosting {
                    unattended: s.unattended,
                    audio: s.audio,
                    account_server: s.account_server,
                    virtual_display: s.virtual_display,
                };
                page.ui.call::<api::State>(method::HOST_START, options).await
            } else {
                page.ui.call::<api::State>(method::HOST_STOP, ()).await
            };
            page.busy.set(false);
            page.spinner.stop();
            page.spinner.set_visible(false);
            page.sharing.set_sensitive(true);
            match result {
                Ok(state) => page.update(&state),
                Err(e) => {
                    page.ui.toast(&e);
                    let state = page.ui.state.borrow().clone();
                    page.update(&state);
                }
            }
        });
    }

    pub fn update(self: &Rc<Self>, state: &api::State) {
        self.quiet.set(true);
        self.sharing.set_active(state.hosting.is_some());
        self.quiet.set(false);
        // Invite links go through the account server.
        let signed_in = state.account.is_some();
        self.new_invite.set_sensitive(signed_in);
        self.invite_group.set_description(Some(if signed_in {
            "A link that lets someone connect once, without an account, until it expires."
        } else {
            "Sign in to an account (under Account) to make invite links; they go through your account server."
        }));
        if self.shown.borrow().as_ref() == Some(&state.hosting) {
            return;
        }
        let was_sharing = self.shown.borrow().as_ref().is_some_and(Option::is_some);
        let counts = |h: &api::Hosting| (h.sessions.len(), h.pins.len());
        let before = self.shown.borrow().as_ref().and_then(|h| h.as_ref().map(counts));
        *self.shown.borrow_mut() = Some(state.hosting.clone());

        let Some(h) = &state.hosting else {
            self.account_server.set_visible(false);
            for list in [&self.pins, &self.waiting, &self.sessions, &self.invites] {
                list.set(Vec::new());
            }
            self.invite_group.set_visible(false);
            self.trusted.group.set_visible(false);
            return;
        };
        self.account_server.set_visible(h.account_server.is_some());
        self.account_server.set_subtitle(h.account_server.as_deref().unwrap_or(""));
        self.invite_group.set_visible(true);
        self.show_pins(&h.pins);
        self.show_waiting(&h.waiting);
        self.show_sessions(&h.sessions);
        self.show_invites(&h.invites);
        // Who may connect changes when sharing starts or someone new pairs.
        if !was_sharing || before != Some(counts(h)) {
            self.load_trusted();
        }
    }

    fn show_pins(&self, pins: &[api::PairingPin]) {
        let rows = pins
            .iter()
            .map(|p| {
                let row = adw::ActionRow::builder().title(glib::markup_escape_text(&p.who)).build();
                let pin = gtk::Label::builder().label(&p.pin).selectable(true).build();
                pin.add_css_class("title-2");
                pin.add_css_class("numeric");
                row.add_suffix(&pin);
                row.upcast()
            })
            .collect();
        self.pins.set(rows);
    }

    fn show_waiting(self: &Rc<Self>, waiting: &[api::WaitingConnection]) {
        let rows = waiting
            .iter()
            .map(|w| {
                let row = adw::ActionRow::builder().title(glib::markup_escape_text(&w.who)).build();
                for (label, decision, style) in [
                    ("Deny", api::Decision::Deny, None),
                    ("View only", api::Decision::View, None),
                    ("Allow", api::Decision::Control, Some("suggested-action")),
                ] {
                    let b = row_button(label, style);
                    let (ui, id) = (self.ui.clone(), w.id);
                    b.connect_clicked(move |_| crate::prompts::decide(&ui, id, decision));
                    row.add_suffix(&b);
                }
                row.upcast()
            })
            .collect();
        self.waiting.set(rows);
    }

    fn show_sessions(self: &Rc<Self>, sessions: &[api::Session]) {
        self.disconnect_all.set_visible(sessions.len() > 1);
        let rows = sessions
            .iter()
            .map(|s| {
                let mut about = vec![if s.address == "0.0.0.0" { "through your account".to_string() } else { s.address.clone() }];
                if let Some(d) = s.display {
                    about.push(format!("display {}", d + 1));
                }
                about.push(if s.control { "can control".into() } else { "watching".into() });
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&s.name))
                    .subtitle(glib::markup_escape_text(&about.join(" · ")))
                    .build();
                row.add_prefix(&gtk::Image::from_icon_name(if s.control { "input-keyboard-symbolic" } else { "view-reveal-symbolic" }));
                let end = row_button("Disconnect", None);
                let (ui, id) = (self.ui.clone(), s.id);
                end.connect_clicked(move |_| {
                    let ui2 = ui.clone();
                    ui.spawn(async move { ui2.call::<u64>(method::HOST_DISCONNECT, api::Disconnect { id: Some(id) }).await.map(drop) });
                });
                row.add_suffix(&end);
                row.upcast()
            })
            .collect();
        self.sessions.set(rows);
    }

    fn show_invites(self: &Rc<Self>, invites: &[api::InviteInfo]) {
        let rows = invites
            .iter()
            .map(|i| {
                let what = if i.control { "Guest can control" } else { "Guest can watch" };
                let row = adw::ActionRow::builder().title(what).subtitle(minutes_left(i.minutes_left)).build();
                row.add_prefix(&gtk::Image::from_icon_name("send-to-symbolic"));
                let withdraw = row_button("Withdraw", None);
                let (ui, id) = (self.ui.clone(), i.id.clone());
                withdraw.connect_clicked(move |_| {
                    let (ui2, id) = (ui.clone(), id.clone());
                    ui.spawn(async move { ui2.call::<serde_json::Value>(method::HOST_UNINVITE, api::ById { id }).await.map(drop) });
                });
                row.add_suffix(&withdraw);
                row.upcast()
            })
            .collect();
        self.invites.set(rows);
    }

    fn load_trusted(self: &Rc<Self>) {
        let page = self.clone();
        glib::spawn_future_local(async move {
            match page.ui.call::<Vec<api::TrustedDevice>>(method::HOST_TRUSTED, ()).await {
                Ok(devices) => page.show_trusted(&devices),
                Err(_) => page.trusted.group.set_visible(false),
            }
        });
    }

    fn show_trusted(self: &Rc<Self>, devices: &[api::TrustedDevice]) {
        let rows = devices
            .iter()
            .map(|d| {
                let short: String = d.fingerprint.chars().take(16).collect();
                let row = adw::ActionRow::builder()
                    .title(glib::markup_escape_text(&d.name))
                    .subtitle(fingerprint(&short))
                    .build();
                row.add_css_class("property");
                let remove = icon_button("user-trash-symbolic", "Remove this device");
                let (page, fp, name) = (self.clone(), d.fingerprint.clone(), d.name.clone());
                remove.connect_clicked(move |_| {
                    let (page, fp) = (page.clone(), fp.clone());
                    page.ui.clone().confirm(
                        &format!("Remove {name}?"),
                        "It is disconnected now, and has to pair again with a PIN to connect.",
                        "Remove",
                        move || {
                            let (page, fp) = (page.clone(), fp.clone());
                            page.ui.clone().spawn(async move {
                                page.ui.call::<serde_json::Value>(method::HOST_REVOKE, api::Revoke { fingerprint: fp }).await?;
                                page.load_trusted();
                                Ok(())
                            });
                        },
                    );
                });
                row.add_suffix(&remove);
                row.upcast()
            })
            .collect();
        self.trusted.group.set_visible(true);
        self.trusted.set(rows);
    }

    /// Ask how long and with what rights, then make the link and show it.
    fn ask_for_invite(self: &Rc<Self>) {
        let dialog = adw::AlertDialog::new(Some("New invite link"), Some("Send the link to your guest. It works once."));
        let group = adw::PreferencesGroup::new();
        let (default, min, max) = INVITE_MINUTES;
        let minutes = adw::SpinRow::with_range(min, max, 5.0);
        minutes.set_title("Valid for (minutes)");
        minutes.set_value(default);
        let control = adw::SwitchRow::builder().title("Guest can control").subtitle("Otherwise they can only watch").build();
        group.add(&minutes);
        group.add(&control);
        dialog.set_extra_child(Some(&group));
        dialog.add_responses(&[("cancel", "Cancel"), ("make", "Make link")]);
        dialog.set_response_appearance("make", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("make"));
        dialog.set_close_response("cancel");
        let page = self.clone();
        dialog.connect_response(Some("make"), move |_, _| {
            let invite = api::NewInvite { minutes: minutes.value() as u32, control: control.is_active() };
            let page = page.clone();
            page.ui.clone().spawn(async move {
                let made: api::InviteMade = page.ui.call(method::HOST_INVITE, invite).await?;
                page.show_link(&made.link);
                Ok(())
            });
        });
        dialog.present(Some(&self.ui.window));
    }

    fn show_link(&self, link: &str) {
        let dialog = adw::AlertDialog::new(Some("Invite link"), None);
        let label = gtk::Label::builder().label(link).selectable(true).wrap(true).wrap_mode(gtk::pango::WrapMode::Char).build();
        label.add_css_class("monospace");
        dialog.set_extra_child(Some(&label));
        dialog.add_responses(&[("close", "Close"), ("copy", "Copy link")]);
        dialog.set_response_appearance("copy", adw::ResponseAppearance::Suggested);
        dialog.set_default_response(Some("copy"));
        dialog.set_close_response("close");
        let (ui, link) = (self.ui.clone(), link.to_string());
        dialog.connect_response(Some("copy"), move |_, _| {
            ui.window.clipboard().set_text(&link);
            ui.toast("Invite link copied");
        });
        dialog.present(Some(&self.ui.window));
    }
}

fn option_row(title: &str, subtitle: &str, active: bool) -> adw::SwitchRow {
    adw::SwitchRow::builder().title(title).subtitle(subtitle).active(active).build()
}
