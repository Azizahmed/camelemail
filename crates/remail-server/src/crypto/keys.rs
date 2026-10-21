//! Master keys: the key file and the [`KeyProvider`] trait.
//!
//! The key file is a small keyring so a rotation never strands data:
//! `{"deployment":"dep_…","active":2,"keys":{"1":"<b64>","2":"<b64>"}}`.
//! It lives outside the database volume, mode 0600, and is always replaced
//! atomically (write a temp file, fsync, rename).

use std::collections::BTreeMap;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::RwLock;
use std::time::SystemTime;

use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use super::{dk_aad, open, seal, sha256, CryptoError, DataKey, KEY_LEN};

/// Wraps and unwraps data keys. Implementations: the key file (on-prem, dev),
/// memory (tests); KMS for hosted comes later behind the same trait.
pub trait KeyProvider: Send + Sync {
    /// `dep_…`, the ID a licence is bound to.
    fn deployment(&self) -> String;
    fn active_version(&self) -> i32;
    fn versions(&self) -> Vec<i32>;
    /// Short public fingerprint of a master key, for the console and `key verify`.
    fn fingerprint(&self, version: i32) -> Option<String>;
    fn wrap_with(&self, version: i32, org_id: &str, owner_id: &str, dk: &DataKey) -> Result<Vec<u8>, CryptoError>;
    fn unwrap(&self, version: i32, org_id: &str, owner_id: &str, blob: &[u8]) -> Result<DataKey, CryptoError>;

    /// Wraps under the active master key.
    fn wrap(&self, org_id: &str, owner_id: &str, dk: &DataKey) -> Result<(i32, Vec<u8>), CryptoError> {
        let v = self.active_version();
        Ok((v, self.wrap_with(v, org_id, owner_id, dk)?))
    }
}

/// Parsed master keys.
#[derive(Clone)]
pub struct KeyRing {
    pub deployment: String,
    pub active: i32,
    keys: BTreeMap<i32, Zeroizing<[u8; KEY_LEN]>>,
}

impl KeyRing {
    pub fn generate(deployment: impl Into<String>) -> Self {
        let mut keys = BTreeMap::new();
        keys.insert(1, Zeroizing::new(super::random_bytes::<KEY_LEN>()));
        KeyRing { deployment: deployment.into(), active: 1, keys }
    }

    /// Adds MK_{max+1} and makes it active.
    pub fn rotate(&mut self) -> i32 {
        let next = self.keys.keys().max().copied().unwrap_or(0) + 1;
        self.keys.insert(next, Zeroizing::new(super::random_bytes::<KEY_LEN>()));
        self.active = next;
        next
    }

    pub fn remove(&mut self, version: i32) -> Result<(), CryptoError> {
        if version == self.active {
            return Err(CryptoError::KeyFile("the active master key can't be removed".into()));
        }
        self.keys
            .remove(&version)
            .map(|_| ())
            .ok_or(CryptoError::UnknownVersion(version))
    }

    pub fn versions(&self) -> Vec<i32> {
        self.keys.keys().copied().collect()
    }

    fn key(&self, version: i32) -> Result<&[u8; KEY_LEN], CryptoError> {
        self.keys.get(&version).map(|k| &**k).ok_or(CryptoError::UnknownVersion(version))
    }

    pub fn fingerprint(&self, version: i32) -> Option<String> {
        let k = self.keys.get(&version)?;
        let mut data = b"remail/mk-fp/v1|".to_vec();
        data.extend_from_slice(&k[..]);
        let fp = sha256(&data);
        data.zeroize();
        Some(fp[..8].iter().map(|b| format!("{b:02x}")).collect())
    }

    pub fn wrap_with(&self, version: i32, org_id: &str, owner_id: &str, dk: &DataKey) -> Result<Vec<u8>, CryptoError> {
        let mk = self.key(version)?;
        Ok(seal(mk, &dk_aad(org_id, owner_id, version), dk.bytes()))
    }

    pub fn unwrap(&self, version: i32, org_id: &str, owner_id: &str, blob: &[u8]) -> Result<DataKey, CryptoError> {
        let mk = self.key(version)?;
        let raw = open(mk, &dk_aad(org_id, owner_id, version), blob)?;
        DataKey::from_slice(&raw).ok_or(CryptoError::Decrypt)
    }

    pub fn to_file(&self) -> KeyFile {
        KeyFile {
            deployment: self.deployment.clone(),
            active: self.active,
            keys: self.keys.iter().map(|(v, k)| (v.to_string(), B64.encode(&k[..]))).collect(),
        }
    }
}

/// The on-disk JSON form.
#[derive(Serialize, Deserialize)]
pub struct KeyFile {
    pub deployment: String,
    pub active: i32,
    pub keys: BTreeMap<String, String>,
}

impl Drop for KeyFile {
    fn drop(&mut self) {
        for v in self.keys.values_mut() {
            v.zeroize();
        }
    }
}

impl KeyFile {
    pub fn load(path: &Path) -> Result<KeyRing, CryptoError> {
        let raw = Zeroizing::new(
            std::fs::read(path).map_err(|e| CryptoError::KeyFile(format!("can't read {}: {e}", path.display())))?,
        );
        let file: KeyFile =
            serde_json::from_slice(&raw).map_err(|_| CryptoError::KeyFile(format!("{} is not a Remail key file", path.display())))?;
        file.to_ring()
    }

    /// The key file's content held in a secret instead of on disk
    /// (`REMAIL_MASTER_KEY`): the JSON itself, or the JSON in base64 so it
    /// fits a one-line environment variable.
    pub fn parse_secret(value: &str) -> Result<KeyRing, CryptoError> {
        let value = value.trim();
        let json = if value.starts_with('{') {
            Zeroizing::new(value.as_bytes().to_vec())
        } else {
            let compact: String = value.chars().filter(|c| !c.is_whitespace()).collect();
            Zeroizing::new(
                B64.decode(compact.as_bytes())
                    .map_err(|_| CryptoError::KeyFile("REMAIL_MASTER_KEY is neither key file JSON nor base64 of it".into()))?,
            )
        };
        let file: KeyFile = serde_json::from_slice(&json)
            .map_err(|_| CryptoError::KeyFile("REMAIL_MASTER_KEY does not hold a Remail key file".into()))?;
        file.to_ring()
    }

    pub fn to_ring(&self) -> Result<KeyRing, CryptoError> {
        let mut keys = BTreeMap::new();
        for (v, k) in &self.keys {
            let v: i32 = v.parse().map_err(|_| CryptoError::KeyFile(format!("bad key version {v:?}")))?;
            let bytes = Zeroizing::new(B64.decode(k).map_err(|_| CryptoError::KeyFile(format!("key {v} is not base64")))?);
            let arr: [u8; KEY_LEN] = bytes
                .as_slice()
                .try_into()
                .map_err(|_| CryptoError::KeyFile(format!("key {v} is not 32 bytes")))?;
            keys.insert(v, Zeroizing::new(arr));
        }
        if !keys.contains_key(&self.active) {
            return Err(CryptoError::KeyFile(format!("active key {} is missing", self.active)));
        }
        if !self.deployment.starts_with("dep_") {
            return Err(CryptoError::KeyFile("deployment id is missing".into()));
        }
        Ok(KeyRing { deployment: self.deployment.clone(), active: self.active, keys })
    }

    /// Writes `ring` to `path` atomically with mode 0600.
    pub fn save(path: &Path, ring: &KeyRing) -> Result<(), CryptoError> {
        let json = Zeroizing::new(serde_json::to_vec_pretty(&ring.to_file()).map_err(|e| CryptoError::KeyFile(e.to_string()))?);
        write_private(path, &json).map_err(|e| CryptoError::KeyFile(format!("can't write {}: {e}", path.display())))
    }
}

/// Atomic 0600 write: temp file in the same directory, fsync, rename.
pub fn write_private(path: &Path, data: &[u8]) -> std::io::Result<()> {
    let dir = path.parent().filter(|p| !p.as_os_str().is_empty()).unwrap_or(Path::new("."));
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join(format!(".{}.tmp-{}", path.file_name().and_then(|n| n.to_str()).unwrap_or("key"), std::process::id()));
    let mut opts = std::fs::OpenOptions::new();
    opts.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let result = (|| {
        let mut f = opts.open(&tmp)?;
        f.write_all(data)?;
        f.sync_all()?;
        std::fs::rename(&tmp, path)?;
        if let Ok(d) = std::fs::File::open(dir) {
            let _ = d.sync_all();
        }
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
    result
}

/// Master keys from the key file. The file is re-read when it changes on disk
/// (after `key rotate` on the host), so a running server picks up the new
/// active key without a restart.
pub struct FileKeyProvider {
    path: PathBuf,
    state: RwLock<(KeyRing, Option<SystemTime>)>,
}

impl FileKeyProvider {
    pub fn open(path: impl Into<PathBuf>) -> Result<Self, CryptoError> {
        let path = path.into();
        let ring = KeyFile::load(&path)?;
        let mtime = std::fs::metadata(&path).and_then(|m| m.modified()).ok();
        Ok(FileKeyProvider { path, state: RwLock::new((ring, mtime)) })
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    fn refresh(&self) {
        let mtime = std::fs::metadata(&self.path).and_then(|m| m.modified()).ok();
        let stale = self.state.read().map(|s| s.1 != mtime).unwrap_or(false);
        if !stale {
            return;
        }
        match KeyFile::load(&self.path) {
            Ok(ring) => {
                if let Ok(mut s) = self.state.write() {
                    *s = (ring, mtime);
                }
            }
            Err(e) => tracing::warn!(error = %e, "key file changed but can't be reloaded; keeping the keys in memory"),
        }
    }

    fn with<T>(&self, f: impl FnOnce(&KeyRing) -> T) -> T {
        self.refresh();
        let s = self.state.read().unwrap_or_else(|p| p.into_inner());
        f(&s.0)
    }
}

impl KeyProvider for FileKeyProvider {
    fn deployment(&self) -> String {
        self.with(|r| r.deployment.clone())
    }
    fn active_version(&self) -> i32 {
        self.with(|r| r.active)
    }
    fn versions(&self) -> Vec<i32> {
        self.with(|r| r.versions())
    }
    fn fingerprint(&self, version: i32) -> Option<String> {
        self.with(|r| r.fingerprint(version))
    }
    fn wrap_with(&self, version: i32, org_id: &str, owner_id: &str, dk: &DataKey) -> Result<Vec<u8>, CryptoError> {
        self.with(|r| r.wrap_with(version, org_id, owner_id, dk))
    }
    fn unwrap(&self, version: i32, org_id: &str, owner_id: &str, blob: &[u8]) -> Result<DataKey, CryptoError> {
        self.with(|r| r.unwrap(version, org_id, owner_id, blob))
    }
}

/// In-memory master keys (tests).
pub struct MemoryKeyProvider {
    ring: RwLock<KeyRing>,
}

impl MemoryKeyProvider {
    pub fn new(deployment: impl Into<String>) -> Self {
        MemoryKeyProvider { ring: RwLock::new(KeyRing::generate(deployment)) }
    }

    /// Keys loaded from somewhere other than a key file (a platform secret).
    pub fn from_ring(ring: KeyRing) -> Self {
        MemoryKeyProvider { ring: RwLock::new(ring) }
    }

    pub fn rotate(&self) -> i32 {
        self.ring.write().unwrap_or_else(|p| p.into_inner()).rotate()
    }

    pub fn remove(&self, version: i32) -> Result<(), CryptoError> {
        self.ring.write().unwrap_or_else(|p| p.into_inner()).remove(version)
    }

    fn with<T>(&self, f: impl FnOnce(&KeyRing) -> T) -> T {
        f(&self.ring.read().unwrap_or_else(|p| p.into_inner()))
    }
}

impl KeyProvider for MemoryKeyProvider {
    fn deployment(&self) -> String {
        self.with(|r| r.deployment.clone())
    }
    fn active_version(&self) -> i32 {
        self.with(|r| r.active)
    }
    fn versions(&self) -> Vec<i32> {
        self.with(|r| r.versions())
    }
    fn fingerprint(&self, version: i32) -> Option<String> {
        self.with(|r| r.fingerprint(version))
    }
    fn wrap_with(&self, version: i32, org_id: &str, owner_id: &str, dk: &DataKey) -> Result<Vec<u8>, CryptoError> {
        self.with(|r| r.wrap_with(version, org_id, owner_id, dk))
    }
    fn unwrap(&self, version: i32, org_id: &str, owner_id: &str, blob: &[u8]) -> Result<DataKey, CryptoError> {
        self.with(|r| r.unwrap(version, org_id, owner_id, blob))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wrap_binds_owner_and_version() {
        let p = MemoryKeyProvider::new("dep_test");
        let dk = DataKey::generate();
        let (v, blob) = p.wrap("org_1", "usr_1", &dk).unwrap();
        assert_eq!(v, 1);
        assert_eq!(p.unwrap(1, "org_1", "usr_1", &blob).unwrap().bytes(), dk.bytes());
        assert!(p.unwrap(1, "org_1", "usr_2", &blob).is_err());
        assert!(p.unwrap(1, "org_2", "usr_1", &blob).is_err());
        assert!(matches!(p.unwrap(2, "org_1", "usr_1", &blob), Err(CryptoError::UnknownVersion(2))));
        // After rotation the old version still unwraps, new wraps use v2.
        assert_eq!(p.rotate(), 2);
        assert!(p.unwrap(1, "org_1", "usr_1", &blob).is_ok());
        let (v2, blob2) = p.wrap("org_1", "usr_1", &dk).unwrap();
        assert_eq!(v2, 2);
        // A v2 blob claimed as v1 fails (the version is in the AAD).
        assert!(p.unwrap(1, "org_1", "usr_1", &blob2).is_err());
        assert!(p.remove(2).is_err(), "active key can't be removed");
        p.remove(1).unwrap();
        assert!(p.unwrap(1, "org_1", "usr_1", &blob).is_err());
    }

    #[test]
    fn key_file_round_trip_is_private_and_atomic() {
        let dir = std::env::temp_dir().join(format!("remail-keys-{}", uuid::Uuid::new_v4().simple()));
        let path = dir.join("master.key");
        let mut ring = KeyRing::generate("dep_abc");
        KeyFile::save(&path, &ring).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(std::fs::metadata(&path).unwrap().permissions().mode() & 0o777, 0o600);
        }
        let provider = FileKeyProvider::open(&path).unwrap();
        let dk = DataKey::generate();
        let (_, blob) = provider.wrap("org_1", "org_1", &dk).unwrap();
        // Rotate on disk: the running provider notices.
        ring.rotate();
        std::thread::sleep(std::time::Duration::from_millis(20));
        KeyFile::save(&path, &ring).unwrap();
        // Make sure the mtime differs even on coarse filesystems.
        let later = SystemTime::now() + std::time::Duration::from_secs(2);
        std::fs::File::options().write(true).open(&path).unwrap().set_modified(later).unwrap();
        assert_eq!(provider.active_version(), 2);
        assert!(provider.unwrap(1, "org_1", "org_1", &blob).is_ok());
        assert_eq!(provider.fingerprint(1), ring.fingerprint(1));
        let leftovers: Vec<_> = std::fs::read_dir(&dir).unwrap().filter_map(|e| e.ok()).collect();
        assert_eq!(leftovers.len(), 1, "no temp files left behind");
        std::fs::remove_dir_all(&dir).unwrap();
    }
}
