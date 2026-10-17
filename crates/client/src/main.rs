mod connect;
mod known_hosts;
mod launcher;
mod media;
mod stream;

use std::sync::OnceLock;

use adw::prelude::*;
use gstreamer as gst;
use gtk::{gdk, glib};
use linuxplay_net::tls::{self, Identity};

const APP_ID: &str = "io.github.techlm77.LinuxPlay";

const CSS: &str = "
.linuxplay-video { background-color: black; }
.linuxplay-waiting { background-color: black; color: white; }
.linuxplay-stats {
    font-family: monospace;
    font-size: 0.9em;
    padding: 6px 10px;
    border-radius: 8px;
    background-color: rgba(0, 0, 0, 0.65);
    color: white;
}
";

/// Runtime for the signalling connections; GTK keeps the main thread.
pub fn runtime() -> &'static tokio::runtime::Runtime {
    static RT: OnceLock<tokio::runtime::Runtime> = OnceLock::new();
    RT.get_or_init(|| {
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .thread_name("linuxplay-net")
            .enable_all()
            .build()
            .expect("tokio runtime")
    })
}

fn main() -> glib::ExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    if let Err(e) = gst::init() {
        eprintln!("GStreamer failed to start: {e}");
        return glib::ExitCode::FAILURE;
    }
    if let Err(e) = gstgtk4::plugin_register_static() {
        eprintln!("registering gtk4paintablesink: {e}");
        return glib::ExitCode::FAILURE;
    }
    linuxplay_net::init_crypto();

    // `linuxplay ADDRESS` connects straight away.
    let address = std::env::args().nth(1);

    // NON_UNIQUE lets a second instance run (used by tests to drive two
    // displays as separate processes); normally the app is single-instance.
    let mut builder = adw::Application::builder().application_id(APP_ID);
    if std::env::var_os("LINUXPLAY_NONUNIQUE").is_some() {
        builder = builder.flags(gtk::gio::ApplicationFlags::NON_UNIQUE);
    }
    let app = builder.build();
    app.connect_startup(|_| {
        let provider = gtk::CssProvider::new();
        provider.load_from_string(CSS);
        if let Some(display) = gdk::Display::default() {
            gtk::style_context_add_provider_for_display(&display, &provider, gtk::STYLE_PROVIDER_PRIORITY_APPLICATION);
        }
    });
    app.connect_activate(move |app| {
        if let Some(win) = app.active_window() {
            win.present();
            return;
        }
        let identity = match Identity::load_or_create(&tls::config_dir("client"), "LinuxPlay client") {
            Ok(i) => i,
            Err(e) => {
                eprintln!("creating this device's identity: {e:#}");
                app.quit();
                return;
            }
        };
        let launcher = launcher::Launcher::new(app, identity);
        launcher.present();
        if let Some(addr) = &address {
            launcher.connect(addr);
        }
    });
    app.run_with_args::<&str>(&[])
}
