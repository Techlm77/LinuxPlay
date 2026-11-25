//! The main window: three pages (Connect, Share, Account) under a header
//! bar, a banner for anything urgent, and the engine connection behind them.

use std::cell::RefCell;
use std::rc::Rc;

use adw::prelude::*;
use gtk::{gio, glib};
use linuxplay_proto::api::{self, method};

use crate::account_page::AccountPage;
use crate::connect_page::ConnectPage;
use crate::engine::{self, Engine};
use crate::prompts::Prompts;
use crate::settings::Settings;
use crate::share_page::SharePage;
use crate::ui::Ui;
use crate::{APP_ID, VERSION};

/// Below this width the page switcher moves to the bottom.
const NARROW_SP: f64 = 560.0;
const STILL_RUNNING: &str = "still-running";

/// Everything that redraws when the state changes.
struct Pages {
    connect: Rc<ConnectPage>,
    share: Rc<SharePage>,
    account: Rc<AccountPage>,
    prompts: Rc<Prompts>,
    banner: adw::Banner,
}

impl Pages {
    fn update(&self, state: &api::State) {
        let warning = state.hosting.as_ref().and_then(|h| h.warning.clone());
        self.banner.set_title(&glib::markup_escape_text(warning.as_deref().unwrap_or("")));
        self.banner.set_revealed(warning.is_some());
        self.connect.update(state);
        self.share.update(state);
        self.account.update(state);
        self.prompts.update(state);
    }
}

pub fn activate(app: &adw::Application) {
    if let Some(window) = app.active_window().or_else(|| app.windows().into_iter().next()) {
        window.present();
        return;
    }
    build(app);
}

fn build(app: &adw::Application) {
    let window = adw::ApplicationWindow::builder()
        .application(app)
        .title("LinuxPlay")
        .icon_name(APP_ID)
        .default_width(780)
        .default_height(760)
        .width_request(360)
        .height_request(400)
        .build();
    let toasts = adw::ToastOverlay::new();
    let ui = Rc::new(Ui {
        app: app.clone(),
        window: window.clone(),
        toasts: toasts.clone(),
        engine: RefCell::new(None),
        settings: RefCell::new(Settings::load()),
        state: RefCell::new(api::State::default()),
    });

    let stack = adw::ViewStack::new();
    let connect = ConnectPage::new(&ui);
    let share = SharePage::new(&ui);
    let account = AccountPage::new(&ui);
    stack.add_titled_with_icon(&connect.widget, Some("connect"), "Connect", "video-display-symbolic");
    stack.add_titled_with_icon(&share.widget, Some("share"), "Share", "network-transmit-receive-symbolic");
    stack.add_titled_with_icon(&account.widget, Some("account"), "Account", "avatar-default-symbolic");

    let switcher = adw::ViewSwitcher::builder().stack(&stack).policy(adw::ViewSwitcherPolicy::Wide).build();
    let header = adw::HeaderBar::builder().title_widget(&switcher).build();
    let menu = gio::Menu::new();
    menu.append(Some("About LinuxPlay"), Some("app.about"));
    menu.append(Some("Quit"), Some("app.quit"));
    header.pack_end(&gtk::MenuButton::builder().icon_name("open-menu-symbolic").menu_model(&menu).tooltip_text("Main menu").build());
    let bottom = adw::ViewSwitcherBar::builder().stack(&stack).build();

    let banner = adw::Banner::new("");
    let content = gtk::Box::new(gtk::Orientation::Vertical, 0);
    content.append(&banner);
    content.append(&stack);
    stack.set_vexpand(true);

    // Shown until the engine answers, or when it can't be reached.
    let status = adw::StatusPage::builder().icon_name(APP_ID).title("Starting LinuxPlay…").build();
    let retry = gtk::Button::builder().label("Try again").halign(gtk::Align::Center).visible(false).build();
    retry.add_css_class("pill");
    retry.add_css_class("suggested-action");
    status.set_child(Some(&retry));
    let body = gtk::Stack::new();
    body.add_named(&status, Some("status"));
    body.add_named(&content, Some("main"));
    toasts.set_child(Some(&body));

    let view = adw::ToolbarView::new();
    view.add_top_bar(&header);
    view.set_content(Some(&toasts));
    view.add_bottom_bar(&bottom);
    window.set_content(Some(&view));

    let narrow = adw::Breakpoint::new(adw::BreakpointCondition::new_length(
        adw::BreakpointConditionLengthType::MaxWidth,
        NARROW_SP,
        adw::LengthUnit::Sp,
    ));
    narrow.add_setter(&bottom, "reveal", Some(&true.to_value()));
    narrow.add_setter(&switcher, "visible", Some(&false.to_value()));
    window.add_breakpoint(narrow);

    {
        let connect = connect.clone();
        stack.connect_visible_child_name_notify(move |stack| {
            if stack.visible_child_name().as_deref() == Some("connect") {
                connect.refresh();
            }
        });
    }
    {
        let connect = connect.clone();
        window.connect_is_active_notify(move |w| {
            if w.is_active() {
                connect.refresh();
            }
        });
    }
    let pages = Rc::new(Pages { connect, share, account, prompts: Prompts::new(&ui), banner });
    add_actions(&ui);
    close_to_background(&ui);

    let start = {
        let (ui, pages, body, status, retry) = (ui.clone(), pages.clone(), body.clone(), status.clone(), retry.clone());
        Rc::new(move || start_engine(&ui, &pages, &body, &status, &retry))
    };
    let s = start.clone();
    retry.connect_clicked(move |_| s());
    start();
    window.present();
}

/// Reach the engine (starting it if needed) and follow its state.
fn start_engine(ui: &Rc<Ui>, pages: &Rc<Pages>, body: &gtk::Stack, status: &adw::StatusPage, retry: &gtk::Button) {
    status.set_title("Starting LinuxPlay…");
    status.set_description(None);
    retry.set_visible(false);
    body.set_visible_child_name("status");
    let (ui, pages, body, status, retry) = (ui.clone(), pages.clone(), body.clone(), status.clone(), retry.clone());
    glib::spawn_future_local(async move {
        let failed = |status: &adw::StatusPage, retry: &gtk::Button, body: &gtk::Stack, title: &str, why: &str| {
            status.set_title(title);
            status.set_description(Some(&glib::markup_escape_text(why)));
            retry.set_visible(true);
            body.set_visible_child_name("status");
        };
        let link = match engine::link().await {
            Ok(link) => link,
            Err(e) => return failed(&status, &retry, &body, "LinuxPlay didn't start", &e),
        };
        let on_state = {
            let (ui, pages) = (ui.clone(), pages.clone());
            move |state: api::State| {
                *ui.state.borrow_mut() = state.clone();
                pages.update(&state);
            }
        };
        let on_closed = {
            let (ui, status, retry, body) = (ui.clone(), status.clone(), retry.clone(), body.clone());
            move || {
                ui.engine.borrow_mut().take();
                let why = format!("What it said last is in {}.", engine::log_path().display());
                failed(&status, &retry, &body, "LinuxPlay stopped", &why);
            }
        };
        let engine = match Engine::new(link, on_state.clone(), on_closed) {
            Ok(engine) => engine,
            Err(e) => return failed(&status, &retry, &body, "LinuxPlay didn't start", &e),
        };
        *ui.engine.borrow_mut() = Some(engine.clone());
        match engine.call::<api::State>(method::SUBSCRIBE, ()).await {
            Ok(state) => {
                on_state(state);
                body.set_visible_child_name("main");
            }
            Err(e) => failed(&status, &retry, &body, "LinuxPlay didn't answer", &e),
        }
    });
}

fn add_actions(ui: &Rc<Ui>) {
    let app = &ui.app;
    let about = gio::SimpleAction::new("about", None);
    {
        let ui = ui.clone();
        about.connect_activate(move |_, _| {
            adw::AboutDialog::builder()
                .application_name("LinuxPlay")
                .application_icon(APP_ID)
                .developer_name("Techlm77")
                .version(VERSION)
                .comments("Low-latency remote desktop and game streaming for Linux")
                .website("https://github.com/Techlm77/LinuxPlay")
                .issue_url("https://github.com/Techlm77/LinuxPlay/issues")
                .license_type(gtk::License::Gpl30)
                .build()
                .present(Some(&ui.window));
        });
    }
    let quit = gio::SimpleAction::new("quit", None);
    {
        let ui = ui.clone();
        quit.connect_activate(move |_, _| quit_now(&ui));
    }
    let show = gio::SimpleAction::new("show", None);
    {
        let ui = ui.clone();
        show.connect_activate(move |_, _| ui.window.present());
    }
    app.add_action(&about);
    app.add_action(&quit);
    app.add_action(&show);
    app.set_accels_for_action("app.quit", &["<Control>q"]);
    app.set_accels_for_action("window.close", &["<Control>w"]);
}

/// Closing the window while this computer is shared, or while a stream is
/// open, keeps LinuxPlay running in the background: the sharing (or stream)
/// would otherwise end with it. Opening LinuxPlay again brings the window back.
fn close_to_background(ui: &Rc<Ui>) {
    let ui = ui.clone();
    ui.window.clone().connect_close_request(move |window| {
        let busy = {
            let state = ui.state.borrow();
            state.hosting.is_some() || !state.connections.is_empty()
        };
        let owned = ui.engine.borrow().as_ref().is_some_and(Engine::owned);
        if busy && owned {
            window.set_visible(false);
            let sharing = ui.state.borrow().hosting.is_some();
            let n = gio::Notification::new("LinuxPlay is still running");
            n.set_body(Some(if sharing {
                "This computer is still shared. Open LinuxPlay to stop sharing."
            } else {
                "Your stream is still open. Open LinuxPlay to see it."
            }));
            n.set_default_action("app.show");
            ui.app.send_notification(Some(STILL_RUNNING), &n);
            return glib::Propagation::Stop;
        }
        stop_engine(&ui);
        glib::Propagation::Proceed
    });
}

fn quit_now(ui: &Rc<Ui>) {
    stop_engine(ui);
    ui.app.quit();
}

fn stop_engine(ui: &Ui) {
    ui.app.withdraw_notification(STILL_RUNNING);
    if let Some(engine) = ui.engine.borrow_mut().take() {
        engine.shut_down();
    }
}
