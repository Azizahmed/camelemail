//! Account passwords live in the OS keyring (Secret Service on Linux),
//! never in the database. When no keyring is available (headless test
//! machines) they fall back to a 0600 file next to the database.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{Context, Result};

const SERVICE: &str = "app.remail.desktop";

pub trait SecretStore: Send + Sync {
    fn set(&self, account_id: &str, secret: &str) -> Result<()>;
    fn get(&self, account_id: &str) -> Result<Option<String>>;
    fn delete(&self, account_id: &str) -> Result<()>;
}

/// OS keyring with a file fallback.
pub struct KeyringStore {
    fallback: FileStore,
    /// Process-lifetime cache so sync doesn't hit D-Bus every tick.
    cache: Mutex<HashMap<String, String>>,
}

impl KeyringStore {
    pub fn new(fallback_path: PathBuf) -> Self {
        Self { fallback: FileStore::new(fallback_path), cache: Mutex::new(HashMap::new()) }
    }
}

impl SecretStore for KeyringStore {
    fn set(&self, account_id: &str, secret: &str) -> Result<()> {
        let keyring_ok = keyring::Entry::new(SERVICE, account_id).and_then(|e| e.set_password(secret)).is_ok();
        if keyring_ok {
            // Don't leave an older copy in the fallback file.
            let _ = self.fallback.delete(account_id);
        } else {
            tracing::warn!("system keyring unavailable; storing credentials in a private file");
            self.fallback.set(account_id, secret)?;
        }
        self.cache.lock().unwrap().insert(account_id.to_string(), secret.to_string());
        Ok(())
    }

    fn get(&self, account_id: &str) -> Result<Option<String>> {
        if let Some(s) = self.cache.lock().unwrap().get(account_id) {
            return Ok(Some(s.clone()));
        }
        let found = match keyring::Entry::new(SERVICE, account_id).and_then(|e| e.get_password()) {
            Ok(s) => Some(s),
            Err(_) => self.fallback.get(account_id)?,
        };
        if let Some(s) = &found {
            self.cache.lock().unwrap().insert(account_id.to_string(), s.clone());
        }
        Ok(found)
    }

    fn delete(&self, account_id: &str) -> Result<()> {
        if let Ok(e) = keyring::Entry::new(SERVICE, account_id) {
            let _ = e.delete_credential();
        }
        self.cache.lock().unwrap().remove(account_id);
        self.fallback.delete(account_id)
    }
}

/// JSON map in a user-only file.
pub struct FileStore {
    path: PathBuf,
    lock: Mutex<()>,
}

impl FileStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path, lock: Mutex::new(()) }
    }

    fn read(&self) -> HashMap<String, String> {
        std::fs::read(&self.path).ok().and_then(|b| serde_json::from_slice(&b).ok()).unwrap_or_default()
    }

    fn write(&self, map: &HashMap<String, String>) -> Result<()> {
        if let Some(dir) = self.path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = self.path.with_extension("tmp");
        // A leftover temp file would keep its old mode; start from a new 0600 file.
        let _ = std::fs::remove_file(&tmp);
        let mut options = std::fs::OpenOptions::new();
        options.write(true).create_new(true);
        #[cfg(unix)]
        std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
        let mut file = options.open(&tmp).context("saving credentials")?;
        std::io::Write::write_all(&mut file, &serde_json::to_vec(map)?)?;
        file.sync_all()?;
        drop(file);
        std::fs::rename(&tmp, &self.path).context("saving credentials")?;
        Ok(())
    }
}

impl SecretStore for FileStore {
    fn set(&self, account_id: &str, secret: &str) -> Result<()> {
        let _g = self.lock.lock().unwrap();
        let mut map = self.read();
        map.insert(account_id.to_string(), secret.to_string());
        self.write(&map)
    }

    fn get(&self, account_id: &str) -> Result<Option<String>> {
        let _g = self.lock.lock().unwrap();
        Ok(self.read().get(account_id).cloned())
    }

    fn delete(&self, account_id: &str) -> Result<()> {
        let _g = self.lock.lock().unwrap();
        let mut map = self.read();
        if map.remove(account_id).is_some() {
            self.write(&map)?;
        }
        Ok(())
    }
}

/// In-memory store for tests.
#[derive(Default)]
pub struct MemoryStore(Mutex<HashMap<String, String>>);

impl SecretStore for MemoryStore {
    fn set(&self, account_id: &str, secret: &str) -> Result<()> {
        self.0.lock().unwrap().insert(account_id.into(), secret.into());
        Ok(())
    }
    fn get(&self, account_id: &str) -> Result<Option<String>> {
        Ok(self.0.lock().unwrap().get(account_id).cloned())
    }
    fn delete(&self, account_id: &str) -> Result<()> {
        self.0.lock().unwrap().remove(account_id);
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn file_store_round_trip_is_private() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("secrets.json");
        // A stale temp file with loose permissions must not be reused.
        std::fs::write(path.with_extension("tmp"), "{}").expect("stale tmp");
        let store = FileStore::new(path.clone());
        store.set("acct", "hunter2").expect("set");
        store.set("other", "x").expect("set");
        assert_eq!(store.get("acct").expect("get").as_deref(), Some("hunter2"));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&path).expect("meta").permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        store.delete("acct").expect("delete");
        assert_eq!(store.get("acct").expect("get"), None);
        assert_eq!(FileStore::new(path).get("other").expect("get").as_deref(), Some("x"));
    }
}
