//! `linuxplay`: the stream window, the engine a front end talks to, and a
//! small command-line client for that engine.

mod ctl;
mod service;

use anyhow::Result;
use clap::{Args, Parser, Subcommand};
use linuxplay_client::ViewerOptions;

#[derive(Parser)]
#[command(name = "linuxplay", version, about = "Low-latency remote desktop and game streaming", arg_required_else_help = true)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Open a stream window. `linuxplay TARGET` is short for this.
    Connect(Connect),
    /// Run the engine: it shares this computer, keeps it signed in, and
    /// opens stream windows when a front end asks over its local API.
    Service,
    /// Call a method of the running engine's API and print the answer.
    Api {
        /// For example `state`, `host.start` or `account.login`.
        method: String,
        /// The method's parameters as JSON, e.g. '{"unattended": true}'.
        params: Option<String>,
    },
}

#[derive(Args)]
struct Connect {
    /// What to connect to: an invite link, the name of one of your account's
    /// computers, or a network address (optionally with `:port`).
    target: String,
    /// Which of the host's displays to open (asks when there are several).
    #[arg(long)]
    display: Option<u32>,
    /// The pairing PIN or password, instead of typing it in the window.
    #[arg(long, env = "LINUXPLAY_PIN", hide_env_values = true)]
    pin: Option<String>,
    /// The name this device shows on the host (default: the hostname).
    #[arg(long)]
    name: Option<String>,
    /// Start in fullscreen.
    #[arg(long)]
    fullscreen: bool,
    /// Show the statistics panel from the start.
    #[arg(long)]
    stats: bool,
    /// Start with the mouse locked to the window, for games.
    #[arg(long)]
    game_mouse: bool,
}

/// This computer's name, as other devices will see it.
fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "LinuxPlay".into())
}

/// The command line with `connect` filled in when it starts with a target
/// instead of a command: `linuxplay 192.168.1.20` means `linuxplay connect …`.
fn arguments() -> Vec<String> {
    const WORDS: [&str; 8] = ["connect", "service", "api", "help", "-h", "--help", "-V", "--version"];
    let mut args: Vec<String> = std::env::args().collect();
    if args.get(1).is_some_and(|first| !WORDS.contains(&first.as_str())) {
        args.insert(1, "connect".into());
    }
    args
}

fn connect(options: Connect) -> Result<()> {
    linuxplay_client::init()?;
    linuxplay_client::run_viewer(ViewerOptions {
        address: options.target,
        display: options.display,
        pin: options.pin,
        client_name: options.name.unwrap_or_else(hostname),
        fullscreen: options.fullscreen,
        stats: options.stats,
        game_mouse: options.game_mouse,
    })
}

fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,zbus=error".into()))
        .with_writer(std::io::stderr)
        .init();
    match Cli::parse_from(arguments()).command {
        // The stream window needs the main thread to itself.
        Command::Connect(options) => connect(options),
        Command::Service => runtime()?.block_on(service::run(hostname())),
        Command::Api { method, params } => runtime()?.block_on(ctl::call(&method, params.as_deref())),
    }
}

fn runtime() -> Result<tokio::runtime::Runtime> {
    Ok(tokio::runtime::Builder::new_multi_thread().enable_all().build()?)
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn the_command_line_is_consistent() {
        Cli::command().debug_assert();
        assert!(Cli::try_parse_from(["linuxplay", "connect", "192.168.1.20", "--stats"]).is_ok());
        assert!(Cli::try_parse_from(["linuxplay", "api", "host.start", r#"{"unattended":true}"#]).is_ok());
        assert!(Cli::try_parse_from(["linuxplay", "service"]).is_ok());
    }
}
