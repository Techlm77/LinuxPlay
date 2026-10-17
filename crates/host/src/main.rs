mod audio;
mod capture;
mod input;
mod monitor;
mod portal;
mod quality;
mod server;
mod tray;
mod trust;
mod video;

use std::net::IpAddr;

use anyhow::Result;
use clap::{Parser, Subcommand};
use gstreamer as gst;
use linuxplay_net::tls::{self, Identity};
use linuxplay_proto::DEFAULT_PORT;

use crate::capture::CaptureKind;
use crate::server::{Host, HostConfig};
use crate::trust::TrustStore;

#[derive(Parser)]
#[command(name = "linuxplay-host", version, about = "Share this desktop with LinuxPlay clients")]
struct Cli {
    #[command(subcommand)]
    command: Option<Command>,
    /// Address to listen on.
    #[arg(long, default_value = "0.0.0.0")]
    bind: IpAddr,
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
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
    /// Permanent pairing password instead of a one-time PIN.
    #[arg(long, env = "LINUXPLAY_PASSWORD", hide_env_values = true)]
    password: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// List the video encoders available on this machine.
    Encoders,
    /// Show this host's certificate fingerprint and paired clients.
    Info,
    /// Forget a paired client, by fingerprint prefix.
    Revoke { fingerprint: String },
}

fn hostname() -> String {
    std::fs::read_to_string("/proc/sys/kernel/hostname")
        .map(|s| s.trim().to_string())
        .unwrap_or_else(|_| "linuxplay-host".into())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()),
        )
        .init();
    let cli = Cli::parse();
    gst::init()?;
    linuxplay_net::init_crypto();

    let dir = tls::config_dir("host");
    let mut trust = TrustStore::load(dir.join("trusted.toml"))?;
    match cli.command {
        Some(Command::Encoders) => {
            for e in video::available() {
                println!("{:<14} {:<6} {:?}", e.element, e.codec.name(), e.backend);
            }
            return Ok(());
        }
        Some(Command::Info) => {
            let identity = Identity::load_or_create(&dir, "LinuxPlay host")?;
            println!("fingerprint: {}", tls::fingerprint_display(&identity.fingerprint));
            println!("paired clients:");
            for c in trust.clients() {
                println!("  {:<24} {}", c.name, &c.fingerprint[..16]);
            }
            return Ok(());
        }
        Some(Command::Revoke { fingerprint }) => {
            let n = trust.revoke(&fingerprint)?;
            println!("removed {n} client(s)");
            return Ok(());
        }
        None => {}
    }

    let identity = Identity::load_or_create(&dir, "LinuxPlay host")?;
    tracing::info!("host fingerprint {}", tls::fingerprint_display(&identity.fingerprint));
    let cfg = HostConfig {
        name: cli.name.unwrap_or_else(hostname),
        bind: cli.bind,
        port: cli.port,
        capture: cli.capture,
        encoder: cli.encoder,
        fps: cli.fps.clamp(1, 240),
        start_kbps: cli.start_bitrate,
        max_kbps: cli.max_bitrate,
        audio: !cli.no_audio,
        password: cli.password.filter(|p| !p.is_empty()),
    };
    Host::new(cfg, identity, trust)?.run().await
}
