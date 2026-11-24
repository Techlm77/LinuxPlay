//! What the server remembers: accounts, their devices, sign-ins and invites,
//! in one SQLite file.

use std::path::Path;
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OptionalExtension, params};

use crate::secrets;

const SCHEMA: &str = "
CREATE TABLE IF NOT EXISTS accounts (
    id            INTEGER PRIMARY KEY,
    username      TEXT NOT NULL UNIQUE COLLATE NOCASE,
    password_hash TEXT NOT NULL,
    created       INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS devices (
    id          TEXT PRIMARY KEY,
    account_id  INTEGER NOT NULL REFERENCES accounts(id) ON DELETE CASCADE,
    name        TEXT NOT NULL,
    fingerprint TEXT NOT NULL,
    created     INTEGER NOT NULL,
    last_seen   INTEGER NOT NULL,
    UNIQUE (account_id, fingerprint)
);
CREATE TABLE IF NOT EXISTS sessions (
    token_hash BLOB PRIMARY KEY,
    device_id  TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    created    INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS invites (
    id         TEXT PRIMARY KEY,
    device_id  TEXT NOT NULL REFERENCES devices(id) ON DELETE CASCADE,
    expires_at INTEGER NOT NULL
);
";

pub fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

/// A signed-in device, as found from its token.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Identity {
    pub account_id: i64,
    pub device_id: String,
}

/// A device row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRow {
    pub id: String,
    pub account_id: i64,
    pub name: String,
    pub fingerprint: String,
    pub last_seen: u64,
}

/// Why an account could not be created.
#[derive(Debug, PartialEq, Eq)]
pub enum CreateError {
    UsernameTaken,
    Database(String),
}

impl From<rusqlite::Error> for CreateError {
    fn from(e: rusqlite::Error) -> Self {
        Self::Database(e.to_string())
    }
}

/// The database. Cheap to clone; calls are short and serialised.
#[derive(Clone)]
pub struct Store {
    db: Arc<Mutex<Connection>>,
}

impl Store {
    pub fn open(path: &Path) -> rusqlite::Result<Self> {
        Self::prepare(Connection::open(path)?)
    }

    /// A store that lives only as long as the process, for tests.
    pub fn in_memory() -> rusqlite::Result<Self> {
        Self::prepare(Connection::open_in_memory()?)
    }

    fn prepare(db: Connection) -> rusqlite::Result<Self> {
        db.pragma_update(None, "foreign_keys", true)?;
        db.pragma_update(None, "journal_mode", "WAL")?;
        db.execute_batch(SCHEMA)?;
        Ok(Self { db: Arc::new(Mutex::new(db)) })
    }

    fn with<T>(&self, work: impl FnOnce(&mut Connection) -> rusqlite::Result<T>) -> rusqlite::Result<T> {
        work(&mut self.db.lock().unwrap())
    }

    /// Create an account; returns its id.
    pub fn create_account(&self, username: &str, password_hash: &str) -> Result<i64, CreateError> {
        self.with(|db| {
            db.execute(
                "INSERT INTO accounts (username, password_hash, created) VALUES (?1, ?2, ?3)",
                params![username, password_hash, now()],
            )?;
            Ok(db.last_insert_rowid())
        })
        .map_err(|e| match e {
            rusqlite::Error::SqliteFailure(f, _) if f.code == rusqlite::ErrorCode::ConstraintViolation => {
                CreateError::UsernameTaken
            }
            other => other.into(),
        })
    }

    /// An account's id, canonical username and password hash.
    pub fn account(&self, username: &str) -> rusqlite::Result<Option<(i64, String, String)>> {
        self.with(|db| {
            db.query_row("SELECT id, username, password_hash FROM accounts WHERE username = ?1", [username], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .optional()
        })
    }

    /// Sign a device in: remember it (or refresh its name), end its earlier
    /// sign-ins and store the new token's hash. Returns the device's id.
    pub fn sign_in(&self, account_id: i64, name: &str, fingerprint: &str, token: &str) -> rusqlite::Result<String> {
        self.with(|db| {
            let tx = db.transaction()?;
            let existing: Option<String> = tx
                .query_row(
                    "SELECT id FROM devices WHERE account_id = ?1 AND fingerprint = ?2",
                    params![account_id, fingerprint],
                    |r| r.get(0),
                )
                .optional()?;
            let device_id = match existing {
                Some(id) => {
                    tx.execute("UPDATE devices SET name = ?1, last_seen = ?2 WHERE id = ?3", params![name, now(), id])?;
                    tx.execute("DELETE FROM sessions WHERE device_id = ?1", [&id])?;
                    id
                }
                None => {
                    let id = secrets::new_id();
                    tx.execute(
                        "INSERT INTO devices (id, account_id, name, fingerprint, created, last_seen)
                         VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
                        params![id, account_id, name, fingerprint, now()],
                    )?;
                    id
                }
            };
            tx.execute(
                "INSERT INTO sessions (token_hash, device_id, created) VALUES (?1, ?2, ?3)",
                params![secrets::token_hash(token), device_id, now()],
            )?;
            tx.commit()?;
            Ok(device_id)
        })
    }

    /// Who a token belongs to, if it is a current sign-in.
    pub fn identify(&self, token: &str) -> rusqlite::Result<Option<Identity>> {
        self.with(|db| {
            db.query_row(
                "SELECT d.account_id, d.id FROM sessions s JOIN devices d ON d.id = s.device_id WHERE s.token_hash = ?1",
                [secrets::token_hash(token)],
                |r| Ok(Identity { account_id: r.get(0)?, device_id: r.get(1)? }),
            )
            .optional()
        })
    }

    /// End a device's sign-ins (it stays in the account's list).
    pub fn sign_out(&self, device_id: &str) -> rusqlite::Result<()> {
        self.with(|db| db.execute("DELETE FROM sessions WHERE device_id = ?1", [device_id]).map(drop))
    }

    pub fn devices(&self, account_id: i64) -> rusqlite::Result<Vec<DeviceRow>> {
        self.with(|db| {
            let mut query = db.prepare(
                "SELECT id, account_id, name, fingerprint, last_seen FROM devices WHERE account_id = ?1 ORDER BY name",
            )?;
            let rows = query.query_map([account_id], device_row)?;
            rows.collect()
        })
    }

    pub fn device(&self, device_id: &str) -> rusqlite::Result<Option<DeviceRow>> {
        self.with(|db| {
            db.query_row(
                "SELECT id, account_id, name, fingerprint, last_seen FROM devices WHERE id = ?1",
                [device_id],
                device_row,
            )
            .optional()
        })
    }

    /// Remove a device from an account, with its sign-ins and invites.
    /// Returns whether it was there.
    pub fn remove_device(&self, account_id: i64, device_id: &str) -> rusqlite::Result<bool> {
        self.with(|db| {
            let n = db.execute("DELETE FROM devices WHERE id = ?1 AND account_id = ?2", params![device_id, account_id])?;
            Ok(n > 0)
        })
    }

    pub fn touch(&self, device_id: &str) -> rusqlite::Result<()> {
        self.with(|db| db.execute("UPDATE devices SET last_seen = ?1 WHERE id = ?2", params![now(), device_id]).map(drop))
    }

    /// Create an invite to `device_id`; returns its id.
    pub fn create_invite(&self, device_id: &str, expires_at: u64) -> rusqlite::Result<String> {
        let id = secrets::new_id();
        self.with(|db| {
            db.execute("DELETE FROM invites WHERE expires_at <= ?1", [now()])?;
            db.execute("INSERT INTO invites (id, device_id, expires_at) VALUES (?1, ?2, ?3)", params![id, device_id, expires_at])
        })?;
        Ok(id)
    }

    /// The device an invite leads to, if the invite exists and hasn't expired.
    pub fn invite_host(&self, invite_id: &str) -> rusqlite::Result<Option<String>> {
        self.with(|db| {
            db.query_row("SELECT device_id FROM invites WHERE id = ?1 AND expires_at > ?2", params![invite_id, now()], |r| {
                r.get(0)
            })
            .optional()
        })
    }

    /// Remove one of a device's invites. Returns whether it was there.
    pub fn revoke_invite(&self, device_id: &str, invite_id: &str) -> rusqlite::Result<bool> {
        self.with(|db| {
            let n = db.execute("DELETE FROM invites WHERE id = ?1 AND device_id = ?2", params![invite_id, device_id])?;
            Ok(n > 0)
        })
    }
}

fn device_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<DeviceRow> {
    Ok(DeviceRow {
        id: row.get(0)?,
        account_id: row.get(1)?,
        name: row.get(2)?,
        fingerprint: row.get(3)?,
        last_seen: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const FP_A: &str = "aa";
    const FP_B: &str = "bb";

    #[test]
    fn accounts_are_unique_whatever_the_case() {
        let store = Store::in_memory().unwrap();
        let id = store.create_account("Dominic", "hash").unwrap();
        assert_eq!(store.create_account("dominic", "other"), Err(CreateError::UsernameTaken));
        assert_eq!(store.account("DOMINIC").unwrap(), Some((id, "Dominic".into(), "hash".into())));
        assert_eq!(store.account("nobody").unwrap(), None);
    }

    #[test]
    fn signing_in_again_replaces_the_devices_old_token() {
        let store = Store::in_memory().unwrap();
        let account = store.create_account("a_user", "hash").unwrap();
        let first = store.sign_in(account, "Laptop", FP_A, "token-1").unwrap();
        let again = store.sign_in(account, "Laptop (renamed)", FP_A, "token-2").unwrap();
        assert_eq!(first, again, "the same certificate is the same device");
        assert_eq!(store.identify("token-1").unwrap(), None);
        assert_eq!(store.identify("token-2").unwrap(), Some(Identity { account_id: account, device_id: first.clone() }));
        assert_eq!(store.devices(account).unwrap()[0].name, "Laptop (renamed)");

        let other = store.sign_in(account, "Desktop", FP_B, "token-3").unwrap();
        assert_ne!(first, other);
        assert_eq!(store.devices(account).unwrap().len(), 2);
        store.sign_out(&first).unwrap();
        assert_eq!(store.identify("token-2").unwrap(), None);
        assert!(store.identify("token-3").unwrap().is_some());
    }

    #[test]
    fn removing_a_device_ends_its_sign_ins_and_invites_but_only_for_its_owner() {
        let store = Store::in_memory().unwrap();
        let (mine, theirs) = (store.create_account("mine", "h").unwrap(), store.create_account("theirs", "h").unwrap());
        let device = store.sign_in(mine, "Host", FP_A, "token").unwrap();
        let invite = store.create_invite(&device, now() + 60).unwrap();
        assert!(!store.remove_device(theirs, &device).unwrap(), "another account can't remove it");
        assert!(store.remove_device(mine, &device).unwrap());
        assert_eq!(store.identify("token").unwrap(), None);
        assert_eq!(store.invite_host(&invite).unwrap(), None);
    }

    #[test]
    fn invites_expire_and_can_be_revoked_by_their_host_only() {
        let store = Store::in_memory().unwrap();
        let account = store.create_account("host", "h").unwrap();
        let host = store.sign_in(account, "Host", FP_A, "t1").unwrap();
        let other = store.sign_in(account, "Other", FP_B, "t2").unwrap();
        let live = store.create_invite(&host, now() + 60).unwrap();
        let expired = store.create_invite(&host, now().saturating_sub(1)).unwrap();
        assert_eq!(store.invite_host(&live).unwrap(), Some(host.clone()));
        assert_eq!(store.invite_host(&expired).unwrap(), None);
        assert!(!store.revoke_invite(&other, &live).unwrap());
        assert!(store.revoke_invite(&host, &live).unwrap());
        assert_eq!(store.invite_host(&live).unwrap(), None);
    }
}
