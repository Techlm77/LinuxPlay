//! Hosts this client has paired with, and the certificate each one must show.
//!
//! This file is what stops an impostor, so it fails closed: a missing file is
//! a first run, but a file that can't be read or parsed is an error the user
//! has to resolve, never silently an empty list (which would make every
//! paired host look new and invite pairing with whoever answers).

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use linuxplay_net::{fsutil, tls};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone)]
pub struct KnownHost {
    /// What the user typed, e.g. `192.168.1.30` or `server:47800`.
    pub address: String,
    pub name: String,
    /// Hex SHA-256 of the host's certificate.
    pub fingerprint: String,
    /// Unix seconds.
    pub last_used: u64,
}

impl KnownHost {
    /// The pinned fingerprint, or an error if the stored value is malformed.
    pub fn pin(&self) -> Result<[u8; 32]> {
        let bytes = hex::decode(&self.fingerprint).ok().filter(|b| b.len() == 32);
        let bytes = bytes.with_context(|| format!("the saved fingerprint for {} is damaged", self.address))?;
        Ok(bytes.try_into().expect("checked length"))
    }
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
    /// Where the client keeps the store.
    pub fn default_path() -> PathBuf {
        tls::config_dir("client").join("known_hosts.toml")
    }

    /// Load the store; only a missing file counts as empty.
    pub fn load(path: PathBuf) -> Result<Self> {
        let hosts = match fsutil::read_private_text(&path)? {
            Some(text) => {
                let file: File = toml::from_str(&text).with_context(|| format!("{} is damaged", path.display()))?;
                for h in &file.hosts {
                    h.pin()?;
                }
                file.hosts
            }
            None => Vec::new(),
        };
        Ok(Self { path, hosts })
    }

    /// The user chose to start over: keep the damaged file aside (so nothing
    /// is lost) and begin with no paired hosts.
    pub fn reset(path: &Path) -> Result<(Self, PathBuf)> {
        let aside = path.with_extension(format!("toml.damaged-{}", now()));
        if path.exists() {
            std::fs::rename(path, &aside).with_context(|| format!("moving {} aside", path.display()))?;
        }
        Ok((Self { path: path.to_path_buf(), hosts: Vec::new() }, aside))
    }

    /// Every pinned certificate, by the address its host is remembered under.
    pub fn pins(&self) -> Result<HashMap<String, [u8; 32]>> {
        self.hosts.iter().map(|h| Ok((h.address.clone(), h.pin()?))).collect()
    }

    pub fn get(&self, address: &str) -> Option<&KnownHost> {
        self.hosts.iter().find(|h| h.address == address)
    }

    /// Most recently used first.
    pub fn recent(&self) -> Vec<KnownHost> {
        let mut v = self.hosts.clone();
        v.sort_by_key(|h| std::cmp::Reverse(h.last_used));
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

    /// Forget the host remembered under `target`, an address or a host's
    /// name, and return it. A name shared by several hosts is refused, since
    /// the wrong one could be dropped; the address picks one.
    pub fn forget_matching(&mut self, target: &str) -> Result<KnownHost> {
        let found: Vec<KnownHost> = match self.get(target) {
            Some(host) => vec![host.clone()],
            None => self.hosts.iter().filter(|h| h.name.eq_ignore_ascii_case(target)).cloned().collect(),
        };
        match found.as_slice() {
            [] => bail!("no paired host is called or reached at {target:?}; `linuxplay hosts` lists them"),
            [host] => {
                self.forget(&host.address)?;
                Ok(host.clone())
            }
            several => bail!(
                "{} paired hosts are called {target:?}; forget one by its address: {}",
                several.len(),
                several.iter().map(|h| h.address.as_str()).collect::<Vec<_>>().join(", ")
            ),
        }
    }

    fn save(&self) -> Result<()> {
        let text = toml::to_string_pretty(&File { hosts: self.hosts.clone() })?;
        fsutil::write_private(&self.path, text.as_bytes())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("linuxplay-known-{}-{name}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        dir.join("known_hosts.toml")
    }

    #[test]
    fn first_run_is_empty() {
        let path = scratch("first");
        assert!(KnownHosts::load(path).unwrap().recent().is_empty());
    }

    #[test]
    fn a_damaged_store_is_an_error_not_an_empty_list() {
        let path = scratch("damaged");
        let mut k = KnownHosts::load(path.clone()).unwrap();
        k.remember("server", "Server", &[7; 32]).unwrap();
        std::fs::write(&path, "[[host]]\naddress = \"server\"\nname = ").unwrap();
        assert!(KnownHosts::load(path.clone()).is_err());

        // A truncated fingerprint is damage too.
        std::fs::write(&path, "[[host]]\naddress = \"s\"\nname = \"S\"\nfingerprint = \"abcd\"\nlast_used = 1\n").unwrap();
        assert!(KnownHosts::load(path.clone()).is_err());

        // Starting over keeps the old file.
        let (fresh, aside) = KnownHosts::reset(&path).unwrap();
        assert!(fresh.recent().is_empty());
        assert!(aside.exists() && !path.exists());
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn forgetting_by_address_or_name() {
        let path = scratch("forget");
        let mut k = KnownHosts::load(path.clone()).unwrap();
        k.remember("192.168.1.30", "Server", &[1; 32]).unwrap();
        k.remember("192.168.1.31", "Desk", &[2; 32]).unwrap();
        k.remember("192.168.1.32", "Desk", &[3; 32]).unwrap();

        assert_eq!(k.forget_matching("server").unwrap().address, "192.168.1.30");
        assert!(k.forget_matching("Desk").is_err(), "an ambiguous name forgets nothing");
        assert!(k.forget_matching("nowhere").is_err());
        assert_eq!(k.forget_matching("192.168.1.32").unwrap().name, "Desk");

        let k = KnownHosts::load(path.clone()).unwrap();
        let left: Vec<String> = k.recent().into_iter().map(|h| h.address).collect();
        assert_eq!(left, ["192.168.1.31"]);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn pins_round_trip() {
        let path = scratch("pins");
        let mut k = KnownHosts::load(path.clone()).unwrap();
        k.remember("server", "Server", &[7; 32]).unwrap();
        let k = KnownHosts::load(path.clone()).unwrap();
        assert_eq!(k.get("server").unwrap().pin().unwrap(), [7; 32]);
        std::fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
