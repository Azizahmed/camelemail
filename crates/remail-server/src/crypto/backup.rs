//! Passphrase-sealed key file backups (`key backup`).
//!
//! The passphrase is stretched with argon2id (m=64 MiB, t=3, p=1, random
//! salt) into an AES-256-GCM key that seals the key file JSON.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::STANDARD as B64;
use base64::Engine as _;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

use super::keys::{KeyFile, KeyRing};
use super::{open, random_bytes, seal, CryptoError, KEY_LEN};

const FORMAT: &str = "remail-key-backup-1";
const AAD: &[u8] = b"remail/key-backup/v1";

#[derive(Serialize, Deserialize)]
struct Kdf {
    alg: String,
    m: u32,
    t: u32,
    p: u32,
    salt: String,
}

#[derive(Serialize, Deserialize)]
struct BackupFile {
    format: String,
    deployment: String,
    kdf: Kdf,
    /// nonce || ciphertext, base64.
    sealed: String,
}

/// Minimum passphrase length accepted for a backup.
pub const MIN_PASSPHRASE: usize = 12;

fn stretch(passphrase: &str, salt: &[u8], m: u32, t: u32, p: u32) -> Result<Zeroizing<[u8; KEY_LEN]>, CryptoError> {
    let params = Params::new(m, t, p, Some(KEY_LEN)).map_err(|e| CryptoError::KeyFile(e.to_string()))?;
    let mut out = Zeroizing::new([0u8; KEY_LEN]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params)
        .hash_password_into(passphrase.as_bytes(), salt, out.as_mut())
        .map_err(|e| CryptoError::KeyFile(e.to_string()))?;
    Ok(out)
}

pub fn seal_backup(ring: &KeyRing, passphrase: &str) -> Result<Vec<u8>, CryptoError> {
    seal_backup_with(ring, passphrase, 64 * 1024, 3, 1)
}

/// Same as [`seal_backup`] with an explicit cost (tests use a cheap one).
pub fn seal_backup_with(ring: &KeyRing, passphrase: &str, m: u32, t: u32, p: u32) -> Result<Vec<u8>, CryptoError> {
    if passphrase.chars().count() < MIN_PASSPHRASE {
        return Err(CryptoError::KeyFile(format!("the passphrase must be at least {MIN_PASSPHRASE} characters")));
    }
    let salt = random_bytes::<16>();
    let key = stretch(passphrase, &salt, m, t, p)?;
    let json = Zeroizing::new(serde_json::to_vec(&ring.to_file()).map_err(|e| CryptoError::KeyFile(e.to_string()))?);
    let file = BackupFile {
        format: FORMAT.into(),
        deployment: ring.deployment.clone(),
        kdf: Kdf { alg: "argon2id".into(), m, t, p, salt: B64.encode(salt) },
        sealed: B64.encode(seal(&key, AAD, &json)),
    };
    serde_json::to_vec_pretty(&file).map_err(|e| CryptoError::KeyFile(e.to_string()))
}

pub fn open_backup(data: &[u8], passphrase: &str) -> Result<KeyRing, CryptoError> {
    let file: BackupFile = serde_json::from_slice(data).map_err(|_| CryptoError::KeyFile("not a Remail key backup".into()))?;
    if file.format != FORMAT || file.kdf.alg != "argon2id" {
        return Err(CryptoError::KeyFile("not a Remail key backup".into()));
    }
    let salt = B64.decode(&file.kdf.salt).map_err(|_| CryptoError::Decrypt)?;
    let sealed = B64.decode(&file.sealed).map_err(|_| CryptoError::Decrypt)?;
    let key = stretch(passphrase, &salt, file.kdf.m, file.kdf.t, file.kdf.p)?;
    let json = open(&key, AAD, &sealed)?;
    let kf: KeyFile = serde_json::from_slice(&json).map_err(|_| CryptoError::Decrypt)?;
    kf.to_ring()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn backup_round_trip() {
        let mut ring = KeyRing::generate("dep_x");
        ring.rotate();
        let sealed = seal_backup_with(&ring, "correct horse battery", 256, 1, 1).unwrap();
        let text = String::from_utf8(sealed.clone()).unwrap();
        assert!(!text.contains(&ring.to_file().keys["1"]), "key material is not in the clear");
        let back = open_backup(&sealed, "correct horse battery").unwrap();
        assert_eq!(back.active, 2);
        assert_eq!(back.fingerprint(1), ring.fingerprint(1));
        assert_eq!(back.fingerprint(2), ring.fingerprint(2));
        assert!(open_backup(&sealed, "wrong passphrase!!").is_err());
        assert!(seal_backup_with(&ring, "short", 256, 1, 1).is_err());
    }
}
