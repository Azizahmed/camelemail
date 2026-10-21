//! Envelope encryption (design §4).
//!
//! ```text
//! master key MK_v (key file / KMS, never in Postgres)
//!   └ wraps data keys, AAD "remail/dk/v1|<org>|<owner>|mk<v>"
//!       └ DK_user / DK_org encrypt sections, AAD "remail/sec/v1|<owner>|<section>|<version>"
//! ```
//!
//! Every blob is `nonce (12) || ciphertext || tag (16)` with a fresh random
//! nonce. The AAD binds a blob to its owner, section and version, so swapping
//! rows in the database makes them fail to decrypt rather than leak.
//!
//! Who may decrypt what: org data (config, secrets, TOTP seeds, push tokens)
//! goes through [`Envelope::org_key`]. A user's data key is only handed out
//! against a path token ([`crate::sections::SyncPath`]) that only the sync
//! module can build, so admin code cannot read user sections even by mistake.

pub mod backup;
pub mod envelope;
pub mod keys;

use aes_gcm::aead::{Aead, KeyInit, Payload};
use aes_gcm::{Aes256Gcm, Nonce};
use rand::RngCore;
use sha2::{Digest, Sha256};
use zeroize::{Zeroize, Zeroizing};

pub use envelope::{Envelope, OwnerKind};
pub use keys::{FileKeyProvider, KeyFile, KeyProvider, MemoryKeyProvider};

pub const NONCE_LEN: usize = 12;
pub const KEY_LEN: usize = 32;

#[derive(Debug, thiserror::Error)]
pub enum CryptoError {
    /// Wrong key, wrong AAD or a tampered blob (deliberately not more precise).
    #[error("decryption failed")]
    Decrypt,
    #[error("master key version {0} is not in the key file")]
    UnknownVersion(i32),
    #[error("no data key for {0}")]
    MissingKey(String),
    #[error("key file: {0}")]
    KeyFile(String),
    #[error(transparent)]
    Db(#[from] sqlx::Error),
}

/// A 32-byte AES key that is wiped from memory when dropped.
#[derive(Clone)]
pub struct DataKey(Zeroizing<[u8; KEY_LEN]>);

impl DataKey {
    pub fn generate() -> Self {
        let mut k = Zeroizing::new([0u8; KEY_LEN]);
        rand::rngs::OsRng.fill_bytes(k.as_mut());
        DataKey(k)
    }

    pub fn from_slice(bytes: &[u8]) -> Option<Self> {
        let arr: [u8; KEY_LEN] = bytes.try_into().ok()?;
        let key = DataKey(Zeroizing::new(arr));
        // `arr` was copied; wipe our stack copy too.
        let mut arr = arr;
        arr.zeroize();
        Some(key)
    }

    pub fn bytes(&self) -> &[u8; KEY_LEN] {
        &self.0
    }
}

impl std::fmt::Debug for DataKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DataKey([redacted])")
    }
}

/// AES-256-GCM with a fresh random nonce; returns `nonce || ciphertext`.
pub fn seal(key: &[u8; KEY_LEN], aad: &[u8], plaintext: &[u8]) -> Vec<u8> {
    let cipher = Aes256Gcm::new(key.into());
    let mut nonce = [0u8; NONCE_LEN];
    rand::rngs::OsRng.fill_bytes(&mut nonce);
    // Encryption only fails for plaintexts beyond 64 GiB.
    let ct = cipher
        .encrypt(Nonce::from_slice(&nonce), Payload { msg: plaintext, aad })
        .unwrap_or_default();
    let mut out = Vec::with_capacity(NONCE_LEN + ct.len());
    out.extend_from_slice(&nonce);
    out.extend_from_slice(&ct);
    out
}

pub fn open(key: &[u8; KEY_LEN], aad: &[u8], blob: &[u8]) -> Result<Zeroizing<Vec<u8>>, CryptoError> {
    if blob.len() < NONCE_LEN + 16 {
        return Err(CryptoError::Decrypt);
    }
    let (nonce, ct) = blob.split_at(NONCE_LEN);
    let cipher = Aes256Gcm::new(key.into());
    cipher
        .decrypt(Nonce::from_slice(nonce), Payload { msg: ct, aad })
        .map(Zeroizing::new)
        .map_err(|_| CryptoError::Decrypt)
}

/// AAD for wrapping a data key under master key `version`.
pub fn dk_aad(org_id: &str, owner_id: &str, version: i32) -> Vec<u8> {
    format!("remail/dk/v1|{org_id}|{owner_id}|mk{version}").into_bytes()
}

/// AAD for a section (or any blob) owned by `owner_id`.
pub fn section_aad(owner_id: &str, section: &str, version: i64) -> Vec<u8> {
    format!("remail/sec/v1|{owner_id}|{section}|{version}").into_bytes()
}

pub fn random_bytes<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    rand::rngs::OsRng.fill_bytes(&mut b);
    b
}

pub fn sha256(data: &[u8]) -> Vec<u8> {
    Sha256::digest(data).to_vec()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_and_aad_binding() {
        let key = DataKey::generate();
        let aad = section_aad("usr_1", "prefs", 3);
        let blob = seal(key.bytes(), &aad, b"hello");
        assert_eq!(&**open(key.bytes(), &aad, &blob).unwrap(), b"hello");
        // Another owner, section or version: refused.
        for wrong in [section_aad("usr_2", "prefs", 3), section_aad("usr_1", "accounts", 3), section_aad("usr_1", "prefs", 4)] {
            assert!(open(key.bytes(), &wrong, &blob).is_err());
        }
        // Another key: refused.
        assert!(open(DataKey::generate().bytes(), &aad, &blob).is_err());
        // Tampered: refused.
        let mut bad = blob.clone();
        let last = bad.len() - 1;
        bad[last] ^= 1;
        assert!(open(key.bytes(), &aad, &bad).is_err());
        assert!(open(key.bytes(), &aad, &blob[..10]).is_err());
    }

    #[test]
    fn nonces_are_unique() {
        let key = DataKey::generate();
        let mut seen = std::collections::HashSet::new();
        for _ in 0..1000 {
            let blob = seal(key.bytes(), b"a", b"same plaintext");
            assert!(seen.insert(blob[..NONCE_LEN].to_vec()));
        }
    }
}
