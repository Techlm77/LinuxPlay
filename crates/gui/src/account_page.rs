//! "Account": sign in to (or create) an account on your own account server,
//! see this computer's identity, and manage the account's devices.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::glib;
use linuxplay_proto::api::{self, method};
use linuxplay_proto::rendezvous::Device;

use crate::ui::Ui;
use crate::util::{RowList, ago, fingerprint, icon_button, row_button};

pub struct AccountPage {
    pub widget: adw::PreferencesPage,
    ui: Rc<Ui>,
    signed_out: adw::PreferencesGroup,
    signed_in: adw::PreferencesGroup,
    who: adw::ActionRow,
    devices: RowList,
    this_name: adw::ActionRow,
    this_fingerprint: adw::ActionRow,
    shown: RefCell<Option<(api::ThisDevice, Option<api::SignedIn>)>>,
}

impl AccountPage {
    pub fn new(ui: &Rc<Ui>) -> Rc<Self> {
        let widget = adw::PreferencesPage::new();
        let s = ui.settings.borrow().clone();

        let signed_out = adw::PreferencesGroup::builder()
            .title("Sign in")
            .description(
                "An account lets your devices find each other by name, from anywhere. The account server runs on one \
                 of your own computers, over Tailscale: turn on “Be the account server” under Share on that one, then \
                 sign in to it from the others.",
            )
            .build();
        let server = adw::EntryRow::builder().title("Account server, e.g. http://office-pc:47900").text(&s.server).build();
        let username = adw::EntryRow::builder().title("Username").text(&s.username).build();
        let password = adw::PasswordEntryRow::builder().title("Password").build();
        signed_out.add(&server);
        signed_out.add(&username);
        signed_out.add(&password);
        let buttons = gtk::Box::builder().spacing(12).halign(gtk::Align::End).margin_top(12).build();
        let create = gtk::Button::builder().label("Create account").build();
        let sign_in = gtk::Button::builder().label("Sign in").build();
        sign_in.add_css_class("suggested-action");
        buttons.append(&create);
        buttons.append(&sign_in);
        signed_out.add(&buttons);
        widget.add(&signed_out);

        let signed_in = adw::PreferencesGroup::builder().title("Account").visible(false).build();
        let who = adw::ActionRow::new();
        who.add_prefix(&gtk::Image::from_icon_name("avatar-default-symbolic"));
        let sign_out = row_button("Sign out", None);
        who.add_suffix(&sign_out);
        signed_in.add(&who);
        widget.add(&signed_in);

        let devices = RowList::new("Devices on this account", None, None);
        let refresh = icon_button("view-refresh-symbolic", "Look again");
        devices.group.set_header_suffix(Some(&refresh));
        widget.add(&devices.group);

        let this = adw::PreferencesGroup::builder()
            .title("This computer")
            .description("Other devices check this fingerprint, so nobody can pretend to be this computer.")
            .build();
        let this_name = adw::ActionRow::builder().title("Name").subtitle_selectable(true).build();
        let this_fingerprint = adw::ActionRow::builder().title("Fingerprint").subtitle_selectable(true).build();
        this_name.add_css_class("property");
        this_fingerprint.add_css_class("property");
        this.add(&this_name);
        this.add(&this_fingerprint);
        widget.add(&this);

        let page = Rc::new(Self {
            widget,
            ui: ui.clone(),
            signed_out,
            signed_in,
            who,
            devices,
            this_name,
            this_fingerprint,
            shown: RefCell::new(None),
        });

        let submit = {
            let page = page.clone();
            let (server, username, password) = (server.clone(), username.clone(), password.clone());
            let (b1, b2) = (sign_in.clone(), create.clone());
            move |create: bool| {
                let form = api::SignIn {
                    server: server.text().trim().to_string(),
                    username: username.text().trim().to_string(),
                    password: password.text().to_string(),
                };
                if form.server.is_empty() || form.username.is_empty() || form.password.is_empty() {
                    page.ui.toast("Fill in the account server, username and password.");
                    return;
                }
                page.ui.update_settings(|s| {
                    s.server = form.server.clone();
                    s.username = form.username.clone();
                });
                let (page, password, b1, b2) = (page.clone(), password.clone(), b1.clone(), b2.clone());
                b1.set_sensitive(false);
                b2.set_sensitive(false);
                glib::spawn_future_local(async move {
                    let name = if create { method::ACCOUNT_CREATE } else { method::ACCOUNT_LOGIN };
                    let result = page.ui.call::<api::State>(name, form).await;
                    b1.set_sensitive(true);
                    b2.set_sensitive(true);
                    match result {
                        Ok(_) => {
                            password.set_text("");
                            page.ui.toast(if create { "Account created; this computer is signed in" } else { "Signed in" });
                        }
                        Err(e) => page.ui.toast(&e),
                    }
                });
            }
        };
        let s1 = submit.clone();
        sign_in.connect_clicked(move |_| s1(false));
        let s2 = submit.clone();
        create.connect_clicked(move |_| s2(true));
        password.connect_entry_activated(move |_| submit(false));
        {
            let page = page.clone();
            sign_out.connect_clicked(move |_| {
                let page = page.clone();
                page.ui.clone().confirm(
                    "Sign out?",
                    "Your other devices won't find this computer by name until it signs in again. Paired devices can still connect on your network.",
                    "Sign out",
                    move || {
                        let ui = page.ui.clone();
                        page.ui.spawn(async move { ui.call::<api::State>(method::ACCOUNT_LOGOUT, ()).await.map(drop) });
                    },
                );
            });
        }
        {
            let page = page.clone();
            refresh.connect_clicked(move |_| page.load_devices());
        }
        page
    }

    pub fn update(self: &Rc<Self>, state: &api::State) {
        let now = (state.device.clone(), state.account.clone());
        if self.shown.borrow().as_ref() == Some(&now) {
            return;
        }
        *self.shown.borrow_mut() = Some(now);
        self.this_name.set_subtitle(&glib::markup_escape_text(&state.device.name));
        self.this_fingerprint.set_subtitle(&fingerprint(&state.device.fingerprint));
        match &state.account {
            Some(a) => {
                self.signed_out.set_visible(false);
                self.signed_in.set_visible(true);
                self.who.set_title(&glib::markup_escape_text(&a.username));
                self.who.set_subtitle(&glib::markup_escape_text(&a.server));
                self.load_devices();
            }
            None => {
                self.signed_out.set_visible(true);
                self.signed_in.set_visible(false);
                self.devices.set(Vec::new());
            }
        }
    }

    fn load_devices(self: &Rc<Self>) {
        let page = self.clone();
        glib::spawn_future_local(async move {
            if let Ok(devices) = page.ui.call::<Vec<Device>>(method::ACCOUNT_DEVICES, ()).await {
                page.show_devices(&devices);
            }
        });
    }

    fn show_devices(self: &Rc<Self>, devices: &[Device]) {
        let rows = devices
            .iter()
            .map(|d| {
                let status = if d.current { "This computer".into() } else { format!("Last seen {}", ago(d.last_seen)) };
                let row = adw::ActionRow::builder().title(glib::markup_escape_text(&d.name)).subtitle(status).build();
                row.add_prefix(&gtk::Image::from_icon_name("computer-symbolic"));
                if !d.current {
                    let remove = icon_button("user-trash-symbolic", "Remove from the account");
                    let (page, id, name) = (self.clone(), d.id.clone(), d.name.clone());
                    remove.connect_clicked(move |_| {
                        let (page, id) = (page.clone(), id.clone());
                        page.ui.clone().confirm(
                            &format!("Remove {name} from the account?"),
                            "It is signed out and has to sign in again to be found by name.",
                            "Remove",
                            move || {
                                let (page, id) = (page.clone(), id.clone());
                                page.ui.clone().spawn(async move {
                                    page.ui.call::<serde_json::Value>(method::ACCOUNT_REMOVE_DEVICE, api::ById { id }).await?;
                                    page.load_devices();
                                    Ok(())
                                });
                            },
                        );
                    });
                    row.add_suffix(&remove);
                }
                row.upcast()
            })
            .collect();
        self.devices.set(rows);
    }
}
