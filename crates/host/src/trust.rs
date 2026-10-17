//! Clients that have paired with this host, kept in `trusted.toml`.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use linuxplay_net::tls::Fingerprint;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct TrustedClient {
    pub name: String,
    /// Hex SHA-256 of the client's certificate.
    pub fingerprint: String,
    /// Unix seconds.
    pub paired_at: u64,
}

#[derive(Serialize, Deserialize, Debug, Default)]
struct File {
    #[serde(default, rename = "client")]
    clients: Vec<TrustedClient>,
}

pub struct TrustStore {
    path: PathBuf,
    clients: Vec<TrustedClient>,
}

impl TrustStore {
    pub fn load(path: PathBuf) -> Result<Self> {
        let clients = match fs::read_to_string(&path) {
            Ok(text) => {
                toml::from_str::<File>(&text)
                    .with_context(|| format!("reading {}", path.display()))?
                    .clients
            }
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Vec::new(),
            Err(e) => return Err(e).with_context(|| format!("reading {}", path.display())),
        };
        Ok(Self { path, clients })
    }

    pub fn get(&self, fp: &Fingerprint) -> Option<&TrustedClient> {
        let fp = hex::encode(fp);
        self.clients.iter().find(|c| c.fingerprint == fp)
    }

    pub fn clients(&self) -> &[TrustedClient] {
        &self.clients
    }

    pub fn add(&mut self, fp: &Fingerprint, name: &str) -> Result<()> {
        let fingerprint = hex::encode(fp);
        self.clients.retain(|c| c.fingerprint != fingerprint);
        let paired_at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        self.clients.push(TrustedClient { name: name.to_string(), fingerprint, paired_at });
        self.save()
    }

    /// Remove clients whose fingerprint starts with `prefix` (hex). Returns how many.
    pub fn revoke(&mut self, prefix: &str) -> Result<usize> {
        let prefix = prefix.to_ascii_lowercase().replace([' ', ':'], "");
        let before = self.clients.len();
        self.clients.retain(|c| !c.fingerprint.starts_with(&prefix));
        let removed = before - self.clients.len();
        if removed > 0 {
            self.save()?;
        }
        Ok(removed)
    }

    fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(&File { clients: self.clients.clone() })?;
        let tmp = self.path.with_extension("toml.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.path).with_context(|| format!("writing {}", self.path.display()))?;
        Ok(())
    }
}
