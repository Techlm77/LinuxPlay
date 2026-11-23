use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::PathBuf;

use anyhow::{Context, Result, bail};
use clap::Parser;
use linuxplay_net::netif::tailscale_addresses;
use linuxplay_proto::rendezvous::DEFAULT_PORT;
use linuxplay_rendezvous::{App, Config, Store};
use tracing::info;

/// LinuxPlay's account server, for your own devices.
///
/// Run it on one of your computers and reach it over Tailscale: by default
/// it listens only on this computer's Tailscale addresses. Don't open its
/// port on your router.
#[derive(Parser)]
#[command(name = "linuxplay-rendezvous", version)]
struct Cli {
    /// Where to listen: `tailscale` (this computer's Tailscale addresses),
    /// `loopback`, or an address.
    #[arg(long, default_value = "tailscale")]
    listen: String,
    #[arg(long, default_value_t = DEFAULT_PORT)]
    port: u16,
    /// Where accounts are stored.
    #[arg(long, default_value = "linuxplay-accounts.sqlite")]
    database: PathBuf,
    /// Take callers' addresses from the X-Forwarded-For header. Only for a
    /// reverse proxy that sets it and is the only way in.
    #[arg(long, hide = true)]
    trust_forwarded_for: bool,
    /// Also answer STUN on this UDP address. Not needed over Tailscale.
    #[arg(long, value_name = "ADDRESS", hide = true)]
    stun: Option<SocketAddr>,
}

fn addresses(listen: &str, port: u16) -> Result<Vec<SocketAddr>> {
    let ips: Vec<IpAddr> = match listen {
        "tailscale" => tailscale_addresses().context("listen on Tailscale (the default)")?,
        "loopback" => vec![IpAddr::V4(Ipv4Addr::LOCALHOST)],
        other => match other.parse::<SocketAddr>() {
            Ok(addr) => return Ok(vec![addr]),
            Err(_) => vec![other.parse().with_context(|| format!("{other} is not `tailscale`, `loopback` or an address"))?],
        },
    };
    if ips.is_empty() {
        bail!("nothing to listen on");
    }
    Ok(ips.into_iter().map(|ip| SocketAddr::new(ip, port)).collect())
}

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info".into()))
        .init();
    let cli = Cli::parse();
    let store = Store::open(&cli.database).with_context(|| format!("opening {}", cli.database.display()))?;
    let addresses = addresses(&cli.listen, cli.port)?;
    let mut config = Config { trust_forwarded_for: cli.trust_forwarded_for, stun_port: None, listening: addresses.clone() };
    if let Some(addr) = cli.stun {
        let socket = tokio::net::UdpSocket::bind(addr).await.with_context(|| format!("binding the STUN port {addr}"))?;
        config.stun_port = Some(socket.local_addr()?.port());
        tokio::spawn(linuxplay_rendezvous::stun::serve(socket));
    }
    let listening = linuxplay_rendezvous::listen(&addresses, App::new(store, config)).await?;
    for addr in &listening.addresses {
        info!("accounts on http://{addr}");
    }
    tokio::signal::ctrl_c().await?;
    Ok(())
}
