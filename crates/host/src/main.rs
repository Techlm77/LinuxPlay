mod approval;
mod audio;
mod capture;
mod control;
mod input;
mod limits;
mod monitor;
mod netif;
mod notify;
mod password;
mod portal;
mod quality;
mod server;
mod sessions;
mod tray;
mod trust;
mod video;

#[cfg(test)]
mod tests;

use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use gstreamer as gst;
use linuxplay_net::tls::{self, Identity};
use linuxplay_proto::DEFAULT_PORT;

/// `DEFAULT_MEDIA_PORTS` as the command line spells it.
const MEDIA_PORTS: &str = "47801-47816";

use crate::approval::Decision;
use crate::capture::CaptureKind;
use crate::control::Command as Ctl;
use crate::netif::ListenMode;
use crate::server::{Host, HostConfig};
use crate::trust::TrustStore;

#[derive(Parser)]
#[command(name = "linuxplay-host", version, about = "Share this desktop with LinuxPlay clients")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Which network to accept connections from.
    #[arg(long, value_enum, default_value_t = ListenMode::Lan)]
    listen: ListenMode,
    /// Listen only on these addresses instead (repeatable); overrides --listen.
    #[arg(long, value_name = "ADDRESS")]
    bind: Vec<IpAddr>,
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
    /// UDP ports for media, one per display streamed, e.g. 47801-47816.
    #[arg(long, value_name = "FIRST-LAST", value_parser = netif::parse_port_range, default_value = MEDIA_PORTS)]
    media_ports: (u16, u16),
    /// Let paired devices connect without asking, and remember the Wayland
    /// screen-share grant. Only for a computer nobody sits at.
    #[arg(long)]
    unattended: bool,
    /// Pair with this password instead of a one-time PIN (unattended only).
    /// Default: the file `linuxplay-host set-password` writes, if any.
    #[arg(long, value_name = "FILE")]
    password_file: Option<PathBuf>,
    /// Only offer these displays (comma-separated indices, e.g. 0,1).
    #[arg(long, value_delimiter = ',')]
    displays: Vec<u32>,
    /// Name shown to clients (default: the hostname).
    #[arg(long)]
    name: Option<String>,
    /// Capture backend (default: detected from the session).
    #[arg(long, value_enum)]
    capture: Option<CaptureKind>,
    /// Use this GStreamer encoder element instead of the best available.
    #[arg(long)]
    encoder: Option<String>,
    /// Framerate cap; the client's refresh rate may lower it.
    #[arg(long, default_value_t = 60)]
    fps: u32,
    /// Starting video bitrate, kbit/s (0 = pick automatically).
    #[arg(long, default_value_t = 0)]
    start_bitrate: u32,
    /// Highest video bitrate, kbit/s (0 = pick automatically from resolution and link).
    #[arg(long, default_value_t = 0)]
    max_bitrate: u32,
    /// Don't stream desktop audio.
    #[arg(long)]
    no_audio: bool,
}

#[derive(Subcommand)]
enum Command {
    /// List the video encoders available on this machine.
    Encoders,
    /// Show this host's certificate fingerprint and paired clients.
    Info,
    /// Forget a paired client (by fingerprint prefix) and end its sessions now.
    Revoke {
        #[arg(required_unless_present = "all")]
        fingerprint: Option<String>,
        /// Forget every paired client.
        #[arg(long, conflicts_with = "fingerprint")]
        all: bool,
    },
    /// Show pairing PINs, connections waiting for approval, and who is connected.
    Sessions,
    /// Allow a waiting connection (see `sessions`).
    Approve {
        id: u64,
        /// Video and audio only; no keyboard or mouse.
        #[arg(long)]
        view_only: bool,
    },
    /// Refuse a waiting connection.
    Deny { id: u64 },
    /// End a session now, or `all` to stop sharing.
    Disconnect { id: String },
    /// Set the pairing password used with --unattended (read from the terminal).
    SetPassword {
        /// Remove it instead.
        #[arg(long)]
        clear: bool,
    },
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "linuxplay-host".into())
}

/// Send a command to the running host and print its answer.
async fn tell_host(cmd: Ctl) -> Result<()> {
    match control::request(cmd).await? {
        Some(reply) => {
            print!("{reply}");
            if reply.starts_with("error") {
                std::process::exit(1);
            }
        }
        None => bail!("no LinuxPlay host is running"),
    }
    Ok(())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();

    let dir = tls::config_dir("host");
    match cli.command {
        Some(Command::Encoders) => {
            gst::init()?;
            for e in video::available() {
                println!("{:<14} {:<6} {:?}", e.element, e.codec.name(), e.backend);
            }
            return Ok(());
        }
        Some(Command::Info) => {
            let trust = TrustStore::load(dir.join("trusted.toml"))?;
            let identity = Identity::load_or_create(&dir, "LinuxPlay host")?;
            println!("fingerprint: {}", tls::fingerprint_display(&identity.fingerprint));
            println!("paired clients:");
            for c in trust.clients() {
                println!("  {:<24} {}", c.name, &c.fingerprint[..16]);
            }
            return Ok(());
        }
        Some(Command::Revoke { fingerprint, all }) => {
            let mut trust = TrustStore::load(dir.join("trusted.toml"))?;
            let removed = if all { trust.revoke_all()? } else { trust.revoke(fingerprint.as_deref().unwrap_or(""))? };
            if all {
                portal::remove_token()?;
            }
            for c in &removed {
                println!("forgot {} ({})", c.name, &c.fingerprint[..16]);
            }
            if removed.is_empty() {
                println!("no paired client matches");
            }
            // Make the running host drop them now, and say whether it did.
            match control::request(Ctl::Reload).await {
                Ok(Some(reply)) => print!("running host: {reply}"),
                Ok(None) => println!("no host is running; this applies when it next starts"),
                Err(e) => bail!("the running host didn't confirm: {e:#}"),
            }
            return Ok(());
        }
        Some(Command::Sessions) => return tell_host(Ctl::List).await,
        Some(Command::Approve { id, view_only }) => {
            let d = if view_only { Decision::ViewOnly } else { Decision::Control };
            return tell_host(Ctl::Decide(id, d)).await;
        }
        Some(Command::Deny { id }) => return tell_host(Ctl::Decide(id, Decision::Deny)).await,
        Some(Command::Disconnect { id }) => {
            let target = if id == "all" { None } else { Some(id.parse()?) };
            return tell_host(Ctl::Disconnect(target)).await;
        }
        Some(Command::SetPassword { clear }) => {
            let path = password::default_path();
            if clear {
                linuxplay_net::fsutil::remove(&path)?;
                println!("pairing password removed");
            } else {
                password::save(&path, &password::prompt_new()?)?;
                println!("saved; it is used when the host runs with --unattended");
            }
            return Ok(());
        }
        None => {}
    }

    gst::init()?;
    linuxplay_net::init_crypto();
    let trust = TrustStore::load(dir.join("trusted.toml"))?;
    let identity = Identity::load_or_create(&dir, "LinuxPlay host")?;
    tracing::info!("host fingerprint {}", tls::fingerprint_display(&identity.fingerprint));

    let password = if cli.unattended {
        password::load(cli.password_file.as_deref().unwrap_or(&password::default_path()))?
    } else {
        if cli.password_file.is_some() {
            bail!("--password-file only applies with --unattended");
        }
        None
    };
    let listen = netif::resolve(cli.listen, &cli.bind)?;
    let exposure = if cli.bind.is_empty() { cli.listen.describe().to_string() } else { "chosen addresses".into() };
    if listen.iter().any(|ip| ip.is_unspecified()) {
        tracing::warn!("listening on every interface, including any public ones; prefer --listen lan or tailscale");
    }
    let cfg = HostConfig {
        name: cli.name.unwrap_or_else(hostname),
        listen,
        exposure,
        port: cli.port,
        media_ports: Some(cli.media_ports),
        capture: cli.capture,
        encoder: cli.encoder,
        fps: cli.fps.clamp(1, 240),
        start_kbps: cli.start_bitrate,
        max_kbps: cli.max_bitrate,
        audio: !cli.no_audio,
        unattended: cli.unattended,
        password,
        displays: (!cli.displays.is_empty()).then(|| cli.displays.iter().copied().collect::<BTreeSet<_>>()),
    };
    Host::new(cfg, identity, trust).await?.run().await
}

#[cfg(test)]
mod cli_tests {
    use clap::{CommandFactory, Parser};
    use linuxplay_proto::DEFAULT_MEDIA_PORTS;

    use super::*;

    #[test]
    fn the_command_line_is_consistent() {
        Cli::command().debug_assert();
        assert_eq!(netif::parse_port_range(MEDIA_PORTS), Ok(DEFAULT_MEDIA_PORTS));
    }

    #[test]
    fn defaults_are_safe() {
        let cli = Cli::try_parse_from(["linuxplay-host"]).unwrap();
        assert_eq!(cli.listen, ListenMode::Lan);
        assert!(cli.bind.is_empty());
        assert!(!cli.unattended);
        assert_eq!(cli.media_ports, DEFAULT_MEDIA_PORTS);
    }

    #[test]
    fn options_parse() {
        let cli = Cli::try_parse_from([
            "linuxplay-host", "--listen", "tailscale", "--media-ports", "50000-50003", "--displays", "0,2", "--unattended",
        ])
        .unwrap();
        assert_eq!(cli.listen, ListenMode::Tailscale);
        assert_eq!(cli.media_ports, (50000, 50003));
        assert_eq!(cli.displays, vec![0, 2]);
        assert!(Cli::try_parse_from(["linuxplay-host", "--media-ports", "9-1"]).is_err());
        assert!(Cli::try_parse_from(["linuxplay-host", "--password", "x"]).is_err(), "no secrets on the command line");
        assert!(Cli::try_parse_from(["linuxplay-host", "revoke"]).is_err(), "revoke needs a fingerprint or --all");
        assert!(Cli::try_parse_from(["linuxplay-host", "revoke", "--all"]).is_ok());
    }
}
