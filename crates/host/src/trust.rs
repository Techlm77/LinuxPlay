//! Clients that have paired with this host, kept in `trusted.toml`.
//!
//! The running host and `linuxplay-host revoke` both change the file, so every
//! change is a locked read-modify-write of what is on disk, and the running
//! host reloads whenever the file changes and ends sessions that lost trust.

use std::fs;
use std::os::fd::AsRawFd;
use std::path::PathBuf;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result, bail};
use linuxplay_net::fsutil;
use linuxplay_net::tls::Fingerprint;
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, Debug, Clone, PartialEq)]
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
    /// Which version of the file we last read.
    stamp: Option<Stamp>,
}

/// Identifies one version of the file: every save is a new inode (written
/// and renamed into place), and the mtime covers edits in place.
type Stamp = (u64, SystemTime);

fn stamp(path: &std::path::Path) -> Option<Stamp> {
    use std::os::unix::fs::MetadataExt;
    let m = fs::metadata(path).ok()?;
    Some((m.ino(), m.modified().ok()?))
}

/// An exclusive advisory lock on `trusted.toml.lock`, held while changing the file.
struct Lock(fs::File);

impl Lock {
    fn take(path: &std::path::Path) -> Result<Self> {
        if let Some(dir) = path.parent() {
            fsutil::private_dir(dir)?;
        }
        let lock_path = path.with_extension("toml.lock");
        let f = fs::OpenOptions::new()
            .create(true)
            .truncate(false)
            .write(true)
            .open(&lock_path)
            .with_context(|| format!("opening {}", lock_path.display()))?;
        // SAFETY: flock on a descriptor we own.
        if unsafe { libc::flock(f.as_raw_fd(), libc::LOCK_EX) } != 0 {
            return Err(std::io::Error::last_os_error()).context("locking the trust store");
        }
        Ok(Self(f))
    }
}

impl Drop for Lock {
    fn drop(&mut self) {
        // SAFETY: as above; closing the file would release it anyway.
        unsafe { libc::flock(self.0.as_raw_fd(), libc::LOCK_UN) };
    }
}

fn read(path: &std::path::Path) -> Result<(Vec<TrustedClient>, Option<Stamp>)> {
    let mtime = stamp(path);
    let clients = match fsutil::read_private_text(path)? {
        Some(text) => toml::from_str::<File>(&text).with_context(|| format!("reading {}", path.display()))?.clients,
        None => Vec::new(),
    };
    Ok((clients, mtime))
}

impl TrustStore {
    /// Load the store. A damaged file is an error, never an empty store.
    pub fn load(path: PathBuf) -> Result<Self> {
        let (clients, stamp) = read(&path)?;
        Ok(Self { path, clients, stamp })
    }

    pub fn contains(&self, fp: &Fingerprint) -> bool {
        let fp = hex::encode(fp);
        self.clients.iter().any(|c| c.fingerprint == fp)
    }

    pub fn clients(&self) -> &[TrustedClient] {
        &self.clients
    }

    /// Has the file changed since we read it?
    pub fn is_stale(&self) -> bool {
        stamp(&self.path) != self.stamp
    }

    /// Re-read the file. On error the store is emptied (nobody is trusted
    /// until it is fixed) and the error returned.
    pub fn reload(&mut self) -> Result<()> {
        match read(&self.path) {
            Ok((clients, stamp)) => {
                self.clients = clients;
                self.stamp = stamp;
                Ok(())
            }
            Err(e) => {
                self.clients.clear();
                self.stamp = stamp(&self.path);
                Err(e)
            }
        }
    }

    /// Apply `change` to what is on disk now, under the lock, and save.
    fn update<T>(&mut self, change: impl FnOnce(&mut Vec<TrustedClient>) -> T) -> Result<T> {
        let _lock = Lock::take(&self.path)?;
        let (mut clients, _) = read(&self.path)?;
        let out = change(&mut clients);
        let text = toml::to_string_pretty(&File { clients: clients.clone() })?;
        fsutil::write_private(&self.path, text.as_bytes())?;
        self.clients = clients;
        self.stamp = stamp(&self.path);
        Ok(out)
    }

    pub fn add(&mut self, fp: &Fingerprint, name: &str) -> Result<()> {
        let fingerprint = hex::encode(fp);
        let paired_at = SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        self.update(|clients| {
            clients.retain(|c| c.fingerprint != fingerprint);
            clients.push(TrustedClient { name: name.to_string(), fingerprint, paired_at });
        })
    }

    /// Remove the client whose fingerprint starts with `prefix` (hex, at
    /// least 8 digits, spaces and colons ignored). An ambiguous prefix
    /// removes nothing. Returns the removed clients.
    pub fn revoke(&mut self, prefix: &str) -> Result<Vec<TrustedClient>> {
        let prefix = prefix.to_ascii_lowercase().replace([' ', ':'], "");
        if prefix.len() < 8 || !prefix.bytes().all(|b| b.is_ascii_hexdigit()) {
            bail!("give at least 8 hex digits of the fingerprint (see `linuxplay-host info`)");
        }
        self.update(|clients| {
            let matching = clients.iter().filter(|c| c.fingerprint.starts_with(&prefix)).count();
            if matching != 1 {
                return Err(matching);
            }
            let i = clients.iter().position(|c| c.fingerprint.starts_with(&prefix)).unwrap();
            Ok(vec![clients.remove(i)])
        })?
        .or_else(|n| if n == 0 { Ok(Vec::new()) } else { bail!("{n} clients match {prefix}; give more digits") })
    }

    /// Remove every client.
    pub fn revoke_all(&mut self) -> Result<Vec<TrustedClient>> {
        self.update(std::mem::take)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("linuxplay-trust-{}-{name}", std::process::id()));
        let _ = fs::remove_dir_all(&dir);
        dir.join("trusted.toml")
    }

    #[test]
    fn a_second_process_revoking_is_not_undone_by_the_first() {
        let path = scratch("race");
        let mut daemon = TrustStore::load(path.clone()).unwrap();
        daemon.add(&[1; 32], "laptop").unwrap();
        daemon.add(&[2; 32], "phone").unwrap();

        // `linuxplay-host revoke` runs in its own process with its own copy.
        let mut cli = TrustStore::load(path.clone()).unwrap();
        let gone = cli.revoke(&hex::encode([1u8; 32])[..12]).unwrap();
        assert_eq!(gone.len(), 1);

        // The daemon notices, and a later pairing doesn't resurrect it.
        assert!(daemon.is_stale());
        daemon.reload().unwrap();
        assert!(!daemon.contains(&[1; 32]) && !daemon.is_stale());
        daemon.add(&[3; 32], "tablet").unwrap();
        assert!(!daemon.contains(&[1; 32]));
        let fresh = TrustStore::load(path.clone()).unwrap();
        assert!(!fresh.contains(&[1; 32]) && fresh.contains(&[2; 32]) && fresh.contains(&[3; 32]));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn revoke_needs_an_unambiguous_prefix() {
        let path = scratch("prefix");
        let mut s = TrustStore::load(path.clone()).unwrap();
        s.add(&[0xab; 32], "a").unwrap();
        let mut b = [0xab; 32];
        b[31] = 0;
        s.add(&b, "b").unwrap();
        assert!(s.revoke("").is_err());
        assert!(s.revoke("abab").is_err(), "too short");
        assert!(s.revoke("abababab").is_err(), "ambiguous");
        assert_eq!(s.revoke("00000000").unwrap().len(), 0);
        assert_eq!(s.clients().len(), 2);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn a_damaged_file_trusts_nobody() {
        let path = scratch("damaged");
        let mut s = TrustStore::load(path.clone()).unwrap();
        s.add(&[1; 32], "laptop").unwrap();
        fs::write(&path, "this is [not toml").unwrap();
        assert!(TrustStore::load(path.clone()).is_err());
        assert!(s.reload().is_err());
        assert!(!s.contains(&[1; 32]));
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }

    #[test]
    fn the_file_is_private() {
        use std::os::unix::fs::PermissionsExt;
        let path = scratch("perm");
        let mut s = TrustStore::load(path.clone()).unwrap();
        s.add(&[1; 32], "laptop").unwrap();
        assert_eq!(fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        fs::remove_dir_all(path.parent().unwrap()).unwrap();
    }
}
