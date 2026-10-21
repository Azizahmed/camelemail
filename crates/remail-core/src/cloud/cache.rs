//! This device's copy of the user's sections (and the organisation config)
//! in the `cloud_sections` table, so setup keeps working without the server.
//! Each row is sealed with AES-256-GCM under a 32-byte key kept in the
//! system keyring (`remail-cache-key`); the section id is the associated
//! data, so rows can't be swapped. A lost key just means downloading again.

use std::collections::BTreeSet;

use anyhow::{anyhow, Context, Result};
use base64::Engine as _;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use rusqlite::{params, OptionalExtension};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use remail_proto::sync::SectionDoc;

use crate::secrets::SecretStore;
use crate::store::Store;

pub const CACHE_KEY_ID: &str = "remail-cache-key";

/// A cached section.
#[derive(Debug, Clone, Default, PartialEq)]
pub(crate) struct Cached {
    /// Server version the document builds on (0 = never written).
    pub version: i64,
    /// The server's document merged with this device's changes.
    pub doc: SectionDoc,
    /// Records that were on this device at the last sync: one missing now
    /// was deleted here (a record never applied here, say a snooze whose
    /// mail hasn't synced yet, isn't).
    pub local: BTreeSet<String>,
    /// Changes the server hasn't taken yet.
    pub dirty: bool,
    /// When the server last confirmed this section (ms).
    pub last_sync: i64,
}

#[derive(Serialize, Deserialize)]
struct Payload {
    doc: SectionDoc,
    #[serde(default)]
    local: BTreeSet<String>,
}

pub(crate) struct Cache {
    key: LessSafeKey,
}

impl Cache {
    /// The device key from the keyring, created on first use.
    pub fn open(secrets: &dyn SecretStore) -> Result<Cache> {
        let raw = match secrets.get(CACHE_KEY_ID)?.and_then(|k| base64::engine::general_purpose::STANDARD.decode(k).ok()).filter(|k| k.len() == 32) {
            Some(k) => k,
            None => {
                let mut k = vec![0u8; 32];
                SystemRandom::new().fill(&mut k).map_err(|_| anyhow!("no system random numbers"))?;
                secrets.set(CACHE_KEY_ID, &base64::engine::general_purpose::STANDARD.encode(&k)).context("saving the cache key")?;
                k
            }
        };
        let key = UnboundKey::new(&AES_256_GCM, &raw).map_err(|_| anyhow!("bad cache key"))?;
        Ok(Cache { key: LessSafeKey::new(key) })
    }

    fn seal(&self, id: &str, plain: &[u8]) -> Result<Vec<u8>> {
        let mut nonce = [0u8; NONCE_LEN];
        SystemRandom::new().fill(&mut nonce).map_err(|_| anyhow!("no system random numbers"))?;
        let mut buf = plain.to_vec();
        self.key
            .seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(aad(id)), &mut buf)
            .map_err(|_| anyhow!("encrypting the section cache failed"))?;
        let mut out = nonce.to_vec();
        out.extend_from_slice(&buf);
        Ok(out)
    }

    fn open_blob(&self, id: &str, blob: &[u8]) -> Option<Vec<u8>> {
        if blob.len() < NONCE_LEN {
            return None;
        }
        let (n, ct) = blob.split_at(NONCE_LEN);
        let nonce = Nonce::try_assume_unique_for_key(n).ok()?;
        let mut buf = ct.to_vec();
        let plain = self.key.open_in_place(nonce, Aad::from(aad(id)), &mut buf).ok()?;
        Some(plain.to_vec())
    }

    pub fn load(&self, store: &Store, id: &str) -> Result<Option<Cached>> {
        let row: Option<(i64, Vec<u8>, bool, i64)> = store
            .db()
            .query_row("SELECT version, doc_json_encrypted, dirty, last_sync FROM cloud_sections WHERE id = ?", [id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })
            .optional()?;
        let Some((version, blob, dirty, last_sync)) = row else { return Ok(None) };
        // Unreadable (another key, say after the keyring was reset): download it again.
        let Some(p) = self.open_blob(id, &blob).and_then(|b| serde_json::from_slice::<Payload>(&b).ok()) else {
            tracing::info!(section = id, "cached section unreadable; it will be downloaded again");
            return Ok(None);
        };
        Ok(Some(Cached { version, doc: p.doc, local: p.local, dirty, last_sync }))
    }

    pub fn save(&self, store: &Store, id: &str, c: &Cached) -> Result<()> {
        let plain = serde_json::to_vec(&Payload { doc: c.doc.clone(), local: c.local.clone() })?;
        let blob = self.seal(id, &plain)?;
        store.db().execute(
            "INSERT INTO cloud_sections(id, version, doc_json_encrypted, dirty, last_sync) VALUES(?,?,?,?,?)
             ON CONFLICT(id) DO UPDATE SET version = excluded.version, doc_json_encrypted = excluded.doc_json_encrypted,
               dirty = excluded.dirty, last_sync = excluded.last_sync",
            params![id, c.version, blob, c.dirty, c.last_sync],
        )?;
        Ok(())
    }

    /// A non-section value (`org`, `providers`), sealed the same way.
    pub fn load_json<T: DeserializeOwned>(&self, store: &Store, id: &str) -> Option<T> {
        let blob: Vec<u8> =
            store.db().query_row("SELECT doc_json_encrypted FROM cloud_sections WHERE id = ?", [id], |r| r.get(0)).optional().ok()??;
        serde_json::from_slice(&self.open_blob(id, &blob)?).ok()
    }

    pub fn save_json<T: Serialize>(&self, store: &Store, id: &str, value: &T, version: i64) -> Result<()> {
        let blob = self.seal(id, &serde_json::to_vec(value)?)?;
        store.db().execute(
            "INSERT INTO cloud_sections(id, version, doc_json_encrypted, dirty, last_sync) VALUES(?,?,?,0,?)
             ON CONFLICT(id) DO UPDATE SET version = excluded.version, doc_json_encrypted = excluded.doc_json_encrypted,
               last_sync = excluded.last_sync",
            params![id, version, blob, crate::store::now_ms()],
        )?;
        Ok(())
    }

    /// Ids of every cached row.
    pub fn ids(store: &Store) -> Result<Vec<String>> {
        let db = store.db();
        let mut stmt = db.prepare("SELECT id FROM cloud_sections")?;
        let rows = stmt.query_map([], |r| r.get(0))?;
        Ok(rows.collect::<rusqlite::Result<_>>()?)
    }

    pub fn clear(store: &Store) -> Result<()> {
        store.db().execute("DELETE FROM cloud_sections", [])?;
        Ok(())
    }
}

fn aad(id: &str) -> Vec<u8> {
    format!("remail/cache/v1|{id}").into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets::MemoryStore;
    use remail_proto::sync::Stamp;

    #[test]
    fn sealed_round_trip() {
        let store = Store::open_in_memory().unwrap();
        let secrets = MemoryStore::default();
        let cache = Cache::open(&secrets).unwrap();
        let mut doc = SectionDoc::default();
        doc.record_mut("_").set("password", serde_json::json!("hunter2"), &Stamp::new(1, "d"));
        let c = Cached { version: 3, doc, local: ["_".to_string()].into(), dirty: true, last_sync: 9 };
        cache.save(&store, "cred:acc_1", &c).unwrap();
        assert_eq!(cache.load(&store, "cred:acc_1").unwrap(), Some(c.clone()));
        // Encrypted at rest.
        let blob: Vec<u8> = store.db().query_row("SELECT doc_json_encrypted FROM cloud_sections", [], |r| r.get(0)).unwrap();
        assert!(!String::from_utf8_lossy(&blob).contains("hunter2"));
        // The same key opens it again; a row moved under another id doesn't open.
        assert_eq!(Cache::open(&secrets).unwrap().load(&store, "cred:acc_1").unwrap(), Some(c));
        store.db().execute("UPDATE cloud_sections SET id = 'cred:acc_2'", []).unwrap();
        assert_eq!(cache.load(&store, "cred:acc_2").unwrap(), None);
        // Another device key can't read it.
        let other = Cache::open(&MemoryStore::default()).unwrap();
        assert_eq!(other.load(&store, "cred:acc_2").unwrap(), None);
    }
}
