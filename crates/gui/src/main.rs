//! LinuxPlay's window: connect to another computer, share this one, sign in.
//!
//! It is a front end for the engine (`linuxplay service`), which it starts
//! when none is running and drives through the engine's local API; everything
//! it shows comes from the engine's state.

mod account_page;
mod connect_page;
mod engine;
mod prompts;
mod settings;
mod share_page;
mod ui;
mod util;
mod window;

use adw::prelude::*;

pub const APP_ID: &str = "io.github.techlm77.LinuxPlay";
pub const VERSION: &str = env!("CARGO_PKG_VERSION");

fn main() -> gtk::glib::ExitCode {
    let app = adw::Application::builder().application_id(APP_ID).build();
    app.connect_activate(window::activate);
    // The window takes no command-line arguments; `linuxplay --help` has them.
    app.run_with_args::<&str>(&[])
}
