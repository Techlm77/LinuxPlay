use std::collections::BTreeSet;
use std::net::IpAddr;
use std::path::PathBuf;

use anyhow::{Result, bail};
use clap::{Parser, Subcommand};
use gstreamer as gst;
use linuxplay_net::rendezvous::{Account, Server};
use linuxplay_net::tls::{self, Identity};
use linuxplay_proto::rendezvous::DeviceInfo;
use linuxplay_proto::DEFAULT_PORT;

/// `DEFAULT_MEDIA_PORTS` as the command line spells it.
const MEDIA_PORTS: &str = "47801-47816";

use linuxplay_host::control::{self, Command as Ctl};
use linuxplay_host::netif::{self, ListenMode};
use linuxplay_host::{CaptureKind, Decision, Host, HostConfig, TestPattern, TestPicture, TrustStore, password, portal, video};

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
    /// On Wayland, also offer a new virtual screen in the share dialog: a
    /// display that exists only for the stream (for headless computers).
    #[arg(long)]
    virtual_display: bool,
    /// Size of the picture `--capture test` shows, e.g. 1920x1080.
    #[arg(long, hide = true, value_name = "WIDTHxHEIGHT", value_parser = parse_size, default_value = "1280x720")]
    test_size: (u32, u32),
    /// What `--capture test` shows.
    #[arg(long, hide = true, value_enum, default_value_t = TestPattern::Ball)]
    test_pattern: TestPattern,
}

/// Parse `WIDTHxHEIGHT`, within what an encoder can take.
fn parse_size(text: &str) -> Result<(u32, u32), String> {
    let parsed = text.split_once(['x', 'X']).and_then(|(w, h)| Some((w.parse().ok()?, h.parse().ok()?)));
    match parsed {
        Some((w, h)) if (16..=8192).contains(&w) && (16..=8192).contains(&h) => Ok((w, h)),
        _ => Err(format!("expected a size like 1920x1080, got {text:?}")),
    }
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
    /// Sign this computer in to an account, so your other devices can find it.
    Account {
        #[command(subcommand)]
        action: AccountAction,
    },
    /// Make a link that lets a guest connect to the running host.
    Invite {
        /// How long the link works, in minutes.
        #[arg(long, default_value_t = 60)]
        minutes: u32,
        /// The guest may watch but not control.
        #[arg(long)]
        view_only: bool,
    },
    /// Withdraw an invite (its id is listed by `sessions`).
    Uninvite { id: String },
}

#[derive(Subcommand)]
enum AccountAction {
    /// Create an account on a LinuxPlay server and sign in.
    Create(SignIn),
    /// Sign in to an existing account.
    Login(SignIn),
    /// Sign out.
    Logout,
    /// Show the account and its devices.
    Status,
}

#[derive(clap::Args)]
struct SignIn {
    /// The LinuxPlay server, e.g. https://play.example.org.
    #[arg(long)]
    server: String,
    #[arg(long)]
    username: String,
    /// Read the password from this environment variable instead of asking.
    #[arg(long, env = "LINUXPLAY_ACCOUNT_PASSWORD", hide_env_values = true, hide = true)]
    password: Option<String>,
}

/// Carry out an `account` command.
async fn account(action: AccountAction, name: String, identity: &Identity) -> Result<()> {
    let path = Account::default_path();
    let sign_in = |form: SignIn, create: bool| async move {
        let server = Server::new(&form.server)?;
        let password = match form.password {
            Some(p) => p,
            None if create => password::prompt_new_account()?,
            None => password::prompt("Password: ")?,
        };
        let device = DeviceInfo { name, fingerprint: hex::encode(identity.fingerprint) };
        let account = if create {
            server.create_account(&form.username, &password, device).await?
        } else {
            server.sign_in(&form.username, &password, device).await?
        };
        account.save(&Account::default_path())?;
        println!("signed in as {} on {}", account.username(), account.server().address());
        println!("restart the host for it to become reachable through the account");
        Ok::<_, anyhow::Error>(())
    };
    match action {
        AccountAction::Create(form) => sign_in(form, true).await,
        AccountAction::Login(form) => sign_in(form, false).await,
        AccountAction::Logout => {
            if let Some(account) = Account::load(&path)? {
                // Still sign out here if the server can't be told.
                if let Err(e) = account.sign_out().await {
                    eprintln!("the server wasn't told ({e:#}); signing out on this computer only");
                }
            }
            linuxplay_net::fsutil::remove(&path)?;
            println!("signed out");
            Ok(())
        }
        AccountAction::Status => {
            let Some(account) = Account::load(&path)? else {
                println!("not signed in");
                return Ok(());
            };
            println!("signed in as {} on {}", account.username(), account.server().address());
            for d in account.devices().await? {
                let state = if d.online { "online" } else { "offline" };
                println!("  {:<24} {state:<8} {}{}", d.name, &d.fingerprint[..16], if d.current { "  (this device)" } else { "" });
            }
            Ok(())
        }
    }
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
            tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,zbus=error".into()),
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
        Some(Command::Account { action }) => {
            linuxplay_net::init_crypto();
            let identity = Identity::load_or_create(&dir, "LinuxPlay host")?;
            return account(action, cli.name.unwrap_or_else(hostname), &identity).await;
        }
        Some(Command::Invite { minutes, view_only }) => {
            return tell_host(Ctl::Invite { minutes, control: !view_only }).await;
        }
        Some(Command::Uninvite { id }) => return tell_host(Ctl::Uninvite(id)).await,
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
        test_picture: TestPicture { width: cli.test_size.0, height: cli.test_size.1, pattern: cli.test_pattern },
        virtual_display: cli.virtual_display,
        account: Account::load(&Account::default_path())?,
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
        assert_eq!(parse_size("1920x1080"), Ok((1920, 1080)));
        assert!(parse_size("1920").is_err() && parse_size("0x0").is_err() && parse_size("99999x9").is_err());
        assert!(Cli::try_parse_from(["linuxplay-host", "--password", "x"]).is_err(), "no secrets on the command line");
        assert!(Cli::try_parse_from(["linuxplay-host", "revoke"]).is_err(), "revoke needs a fingerprint or --all");
        assert!(Cli::try_parse_from(["linuxplay-host", "revoke", "--all"]).is_ok());
    }
}
