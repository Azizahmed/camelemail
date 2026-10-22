//! Account passwords live in the OS keyring (Secret Service on Linux),
//! never in the database. When no keyring is available (headless test
//! machines) they fall back to a 0600 file next to the database. Phones
//! have no such keyring: they keep the same file, sealed with a key that
//! lives in the Android Keystore or iOS Keychain ([`FileStore::sealed`]).

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Mutex;

use anyhow::{anyhow, Context, Result};
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};

#[cfg(not(any(target_os = "android", target_os = "ios")))]
const SERVICE: &str = "app.remail.desktop";

pub trait SecretStore: Send + Sync {
    fn set(&self, account_id: &str, secret: &str) -> Result<()>;
    fn get(&self, account_id: &str) -> Result<Option<String>>;
    fn delete(&self, account_id: &str) -> Result<()>;
}

/// OS keyring with a file fallback.
#[cfg(not(any(target_os = "android", target_os = "ios")))]
pub struct KeyringStore {
    fallback: FileStore,
    /// Process-lifetime cache so sync doesn't hit D-Bus every tick.
    cache: Mutex<HashMap<String, String>>,
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
impl KeyringStore {
    pub fn new(fallback_path: PathBuf) -> Self {
        Self { fallback: FileStore::new(fallback_path), cache: Mutex::new(HashMap::new()) }
    }
}

#[cfg(not(any(target_os = "android", target_os = "ios")))]
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

/// JSON map in a user-only file, optionally sealed (AES-256-GCM) with a key
/// kept outside the file.
pub struct FileStore {
    path: PathBuf,
    lock: Mutex<()>,
    seal: Option<LessSafeKey>,
}

impl FileStore {
    pub fn new(path: PathBuf) -> Self {
        Self { path, lock: Mutex::new(()), seal: None }
    }

    /// A store whose file is encrypted with `key` (32 bytes), for platforms
    /// whose secure storage holds a key rather than every secret.
    pub fn sealed(path: PathBuf, key: &[u8]) -> Result<Self> {
        let key = UnboundKey::new(&AES_256_GCM, key).map_err(|_| anyhow!("the credentials key must be 32 bytes"))?;
        Ok(Self { path, lock: Mutex::new(()), seal: Some(LessSafeKey::new(key)) })
    }

    fn read(&self) -> HashMap<String, String> {
        let Ok(bytes) = std::fs::read(&self.path) else { return HashMap::new() };
        let plain = match &self.seal {
            None => bytes,
            Some(key) => {
                if bytes.len() < NONCE_LEN {
                    return HashMap::new();
                }
                let (n, ct) = bytes.split_at(NONCE_LEN);
                let mut buf = ct.to_vec();
                let opened = Nonce::try_assume_unique_for_key(n).ok().and_then(|n| key.open_in_place(n, Aad::from(b"remail-credentials"), &mut buf).ok());
                match opened {
                    Some(p) => p.to_vec(),
                    None => {
                        // Another key (the app's Keystore entry was reset): start over.
                        tracing::warn!("stored credentials can't be decrypted; they will be fetched again");
                        return HashMap::new();
                    }
                }
            }
        };
        serde_json::from_slice(&plain).unwrap_or_default()
    }

    fn encode(&self, map: &HashMap<String, String>) -> Result<Vec<u8>> {
        let mut buf = serde_json::to_vec(map)?;
        let Some(key) = &self.seal else { return Ok(buf) };
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new().fill(&mut nonce).map_err(|_| anyhow!("no system random numbers"))?;
        key.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(b"remail-credentials"), &mut buf)
            .map_err(|_| anyhow!("encrypting credentials failed"))?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&buf);
        Ok(out)
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
        std::io::Write::write_all(&mut file, &self.encode(map)?)?;
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

    #[test]
    fn sealed_file_store_hides_secrets_and_needs_its_key() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("credentials.json");
        let store = FileStore::sealed(path.clone(), &[7u8; 32]).expect("store");
        store.set("acct", "hunter2").expect("set");
        let raw = std::fs::read(&path).expect("file");
        assert!(!String::from_utf8_lossy(&raw).contains("hunter2"));
        assert_eq!(FileStore::sealed(path.clone(), &[7u8; 32]).expect("store").get("acct").expect("get").as_deref(), Some("hunter2"));
        // Another key reads nothing (and doesn't fail).
        assert_eq!(FileStore::sealed(path, &[8u8; 32]).expect("store").get("acct").expect("get"), None);
        assert!(FileStore::sealed(dir.path().join("x"), &[1u8; 16]).is_err());
    }
}
