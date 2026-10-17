//! Hosts this client has paired with, and the certificate each one must show.

use std::fs;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct KnownHost {
    /// What the user typed, e.g. `192.168.8.184` or `server:47800`.
    pub address: String,
    pub name: String,
    /// Hex SHA-256 of the host's certificate.
    pub fingerprint: String,
    /// Unix seconds.
    pub last_used: u64,
}

#[derive(Serialize, Deserialize, Default)]
struct File {
    #[serde(default, rename = "host")]
    hosts: Vec<KnownHost>,
}

pub struct KnownHosts {
    path: PathBuf,
    hosts: Vec<KnownHost>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl KnownHosts {
    pub fn load(path: PathBuf) -> Self {
        let hosts = fs::read_to_string(&path)
            .ok()
            .and_then(|t| toml::from_str::<File>(&t).ok())
            .map(|f| f.hosts)
            .unwrap_or_default();
        Self { path, hosts }
    }

    pub fn get(&self, address: &str) -> Option<&KnownHost> {
        self.hosts.iter().find(|h| h.address == address)
    }

    /// Most recently used first.
    pub fn recent(&self) -> Vec<KnownHost> {
        let mut v = self.hosts.clone();
        v.sort_by(|a, b| b.last_used.cmp(&a.last_used));
        v
    }

    pub fn remember(&mut self, address: &str, name: &str, fingerprint: &[u8]) -> Result<()> {
        self.hosts.retain(|h| h.address != address);
        self.hosts.push(KnownHost {
            address: address.to_string(),
            name: name.to_string(),
            fingerprint: hex::encode(fingerprint),
            last_used: now(),
        });
        self.save()
    }

    pub fn forget(&mut self, address: &str) -> Result<()> {
        self.hosts.retain(|h| h.address != address);
        self.save()
    }

    fn save(&self) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            fs::create_dir_all(dir)?;
        }
        let text = toml::to_string_pretty(&File { hosts: self.hosts.clone() })?;
        let tmp = self.path.with_extension("toml.tmp");
        fs::write(&tmp, text)?;
        fs::rename(&tmp, &self.path).with_context(|| format!("writing {}", self.path.display()))
    }
}
