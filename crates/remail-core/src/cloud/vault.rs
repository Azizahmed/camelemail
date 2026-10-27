//! End-to-end encrypted mailbox sign-ins (docs/platform/zero-knowledge.md).
//!
//! The Camel Mail password becomes, on the device, a proof the server checks
//! and a key that wraps the account's vault key; the server never sees the
//! password. Mailbox credentials (`cred:*` sections) are sealed with the
//! vault key, which each signed-in device keeps in its secret store.

use argon2::{Algorithm, Argon2, Params, Version};
use base64::engine::general_purpose::{STANDARD as B64, URL_SAFE_NO_PAD as B64URL};
use base64::Engine as _;
use ring::aead::{Aad, LessSafeKey, Nonce, UnboundKey, AES_256_GCM, NONCE_LEN};
use ring::rand::{SecureRandom, SystemRandom};
use ring::{hkdf, hmac};
use serde::{Deserialize, Serialize};
use zeroize::{Zeroize, Zeroizing};

use remail_proto::api::{Kdf, VaultSetup, PROOF_PREFIX};

use super::CloudError;
use crate::engine::Engine;

/// Where a device keeps the vault key.
pub(crate) const SECRET_ID: &str = "camel-vault-key";

const AUTH_INFO: &[u8] = b"camel-mail/auth/v1";
const KEK_INFO: &[u8] = b"camel-mail/vault/v1";
const NONCE_INFO: &[u8] = b"camel-mail/cred-nonce/v1";
const MIN_PASSWORD: usize = 10;

/// What the password gives: the proof for the server and the vault's wrapping key.
pub(crate) struct Keys {
    pub proof: String,
    kek: Zeroizing<[u8; 32]>,
}

/// The account's vault key, as a device holds it.
#[derive(Clone, Serialize, Deserialize)]
pub(crate) struct VaultKey {
    pub id: String,
    key: String,
}

impl Drop for VaultKey {
    fn drop(&mut self) {
        self.key.zeroize();
    }
}

impl std::fmt::Debug for VaultKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("VaultKey").field("id", &self.id).finish_non_exhaustive()
    }
}

fn internal(msg: impl std::fmt::Display) -> CloudError {
    CloudError::new("internal", msg.to_string())
}

fn random<const N: usize>() -> [u8; N] {
    let mut b = [0u8; N];
    SystemRandom::new().fill(&mut b).expect("the system's random source failed");
    b
}

/// Settings for a new password: Argon2id, 64 MiB, 3 passes (a second or so
/// on a phone), a fresh salt.
pub(crate) fn new_kdf() -> Kdf {
    Kdf { alg: "argon2id".into(), m_kib: 65_536, t: 3, p: 1, salt: B64.encode(random::<16>()) }
}

struct Len(usize);

impl hkdf::KeyType for Len {
    fn len(&self) -> usize {
        self.0
    }
}

fn expand(prk: &hkdf::Prk, info: &[u8]) -> Zeroizing<[u8; 32]> {
    let mut out = Zeroizing::new([0u8; 32]);
    let info = [info];
    prk.expand(&info, Len(32)).and_then(|okm| okm.fill(out.as_mut())).expect("HKDF output of 32 bytes");
    out
}

/// The proof and wrapping key for `password`. Slow on purpose: run it off the
/// async threads (`derive_blocking`).
pub(crate) fn derive(password: &str, kdf: &Kdf) -> Result<Keys, CloudError> {
    if !kdf.is_reasonable() {
        return Err(CloudError::new("bad_request", "The server asked for unusual key settings."));
    }
    let salt = B64.decode(&kdf.salt).map_err(|_| CloudError::new("bad_request", "The server sent a damaged salt."))?;
    let params = Params::new(kdf.m_kib, kdf.t, kdf.p, Some(32)).map_err(internal)?;
    let mut master = Zeroizing::new([0u8; 32]);
    Argon2::new(Algorithm::Argon2id, Version::V0x13, params).hash_password_into(password.as_bytes(), &salt, master.as_mut()).map_err(internal)?;
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(master.as_ref());
    let auth = expand(&prk, AUTH_INFO);
    Ok(Keys { proof: format!("{PROOF_PREFIX}{}", B64URL.encode(auth.as_ref())), kek: expand(&prk, KEK_INFO) })
}

pub(crate) async fn derive_blocking(password: &str, kdf: &Kdf) -> Result<Keys, CloudError> {
    let (password, kdf) = (Zeroizing::new(password.to_string()), kdf.clone());
    tokio::task::spawn_blocking(move || derive(&password, &kdf)).await.map_err(internal)?
}

/// A password's proof and `vk` wrapped for it, with fresh key settings
/// (sign-up, a new or reset password, converting an account).
pub(crate) async fn new_secret(password: &str, vk: &VaultKey) -> Result<(String, VaultSetup), CloudError> {
    let kdf = new_kdf();
    let keys = derive_blocking(password, &kdf).await?;
    let setup = wrap(&keys, kdf, vk)?;
    Ok((keys.proof.clone(), setup))
}

/// A new account's (or a reset password's) vault key.
pub(crate) fn new_vault_key() -> VaultKey {
    let id: String = random::<8>().iter().map(|b| format!("{b:02x}")).collect();
    VaultKey { id: format!("vk_{id}"), key: B64.encode(random::<32>()) }
}

impl VaultKey {
    fn bytes(&self) -> Result<Zeroizing<Vec<u8>>, CloudError> {
        let b = Zeroizing::new(B64.decode(&self.key).map_err(|_| internal("damaged vault key"))?);
        if b.len() != 32 {
            return Err(internal("damaged vault key"));
        }
        Ok(b)
    }
}

fn seal_with(key: &[u8], nonce: [u8; NONCE_LEN], aad: &[u8], plain: &[u8]) -> Result<String, CloudError> {
    let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).map_err(|_| internal("bad key"))?);
    let mut buf = plain.to_vec();
    k.seal_in_place_append_tag(Nonce::assume_unique_for_key(nonce), Aad::from(aad), &mut buf).map_err(|_| internal("encryption failed"))?;
    let mut out = nonce.to_vec();
    out.extend_from_slice(&buf);
    buf.zeroize();
    Ok(B64.encode(out))
}

fn open_with(key: &[u8], aad: &[u8], sealed: &str) -> Option<Zeroizing<Vec<u8>>> {
    let raw = B64.decode(sealed).ok()?;
    if raw.len() < NONCE_LEN + 16 {
        return None;
    }
    let (nonce, ct) = raw.split_at(NONCE_LEN);
    let k = LessSafeKey::new(UnboundKey::new(&AES_256_GCM, key).ok()?);
    let mut buf = Zeroizing::new(ct.to_vec());
    let plain = k.open_in_place(Nonce::try_assume_unique_for_key(nonce).ok()?, Aad::from(aad), buf.as_mut_slice()).ok()?;
    Some(Zeroizing::new(plain.to_vec()))
}

fn key_aad(key_id: &str) -> Vec<u8> {
    format!("camel-mail/vault-key/v1|{key_id}").into_bytes()
}

/// The vault key wrapped for `keys`, as the server keeps it.
pub(crate) fn wrap(keys: &Keys, kdf: Kdf, vk: &VaultKey) -> Result<VaultSetup, CloudError> {
    let key_enc = seal_with(keys.kek.as_ref(), random::<NONCE_LEN>(), &key_aad(&vk.id), &vk.bytes()?)?;
    Ok(VaultSetup { kdf, key_enc, key_id: vk.id.clone() })
}

/// The vault key from what the server keeps; `None` means a wrong password.
pub(crate) fn unwrap(keys: &Keys, setup: &VaultSetup) -> Option<VaultKey> {
    let plain = open_with(keys.kek.as_ref(), &key_aad(&setup.key_id), &setup.key_enc)?;
    (plain.len() == 32).then(|| VaultKey { id: setup.key_id.clone(), key: B64.encode(plain.as_slice()) })
}

fn cred_aad(owner: &str, key_id: &str) -> Vec<u8> {
    format!("camel-mail/cred/v1|{owner}|{key_id}").into_bytes()
}

/// Seals a credential section's content. The nonce comes from the content, so
/// the same content seals the same way (sync compares what it uploads) and
/// different content never shares a nonce.
pub(crate) fn seal(vk: &VaultKey, owner: &str, plain: &[u8]) -> Result<String, CloudError> {
    let key = vk.bytes()?;
    let prk = hkdf::Salt::new(hkdf::HKDF_SHA256, &[]).extract(&key);
    let mac_key = expand(&prk, NONCE_INFO);
    let tag = hmac::sign(&hmac::Key::new(hmac::HMAC_SHA256, mac_key.as_ref()), &[cred_aad(owner, &vk.id).as_slice(), b"|", plain].concat());
    let mut nonce = [0u8; NONCE_LEN];
    nonce.copy_from_slice(&tag.as_ref()[..NONCE_LEN]);
    seal_with(&key, nonce, &cred_aad(owner, &vk.id), plain)
}

pub(crate) fn open(vk: &VaultKey, owner: &str, sealed: &str) -> Option<Zeroizing<Vec<u8>>> {
    open_with(&vk.bytes().ok()?, &cred_aad(owner, &vk.id), sealed)
}

/// The vault key this device holds, if any.
pub(crate) fn load(engine: &Engine) -> Option<VaultKey> {
    let raw = Zeroizing::new(engine.secrets().get(SECRET_ID).ok()??);
    serde_json::from_str(&raw).ok()
}

pub(crate) fn save(engine: &Engine, vk: &VaultKey) -> Result<(), CloudError> {
    let json = Zeroizing::new(serde_json::to_string(vk).map_err(internal)?);
    engine.secrets().set(SECRET_ID, &json).map_err(|e| internal(format!("couldn't keep the vault key: {e:#}")))
}

pub(crate) fn forget(engine: &Engine) {
    if let Err(e) = engine.secrets().delete(SECRET_ID) {
        tracing::warn!("couldn't delete the vault key: {e:#}");
    }
}

/// The rule for a new password, checked here because the server can't see
/// it (the sign-up screen also scores it).
pub(crate) fn check_new_password(password: &str, email: &str) -> Result<(), CloudError> {
    let weak = |m: &str| Err(CloudError::new("weak_password", m));
    if password.chars().count() < MIN_PASSWORD {
        return weak("Use at least 10 characters.");
    }
    if password.chars().count() > 256 {
        return weak("That password is too long.");
    }
    let lower = password.to_lowercase();
    let local = email.trim().split('@').next().unwrap_or("").to_lowercase();
    if local.chars().count() >= 3 && lower.contains(&local) {
        return weak("Don't use your e-mail address in your password.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn quick_kdf() -> Kdf {
        // The smallest costs a server accepts, so the test stays fast.
        Kdf { m_kib: 19_456, t: 1, ..new_kdf() }
    }

    #[test]
    fn keys_wrap_and_seal() {
        let kdf = quick_kdf();
        let a = derive("correct horse battery", &kdf).unwrap();
        let again = derive("correct horse battery", &kdf).unwrap();
        assert_eq!(a.proof, again.proof, "the same password gives the same proof");
        assert!(remail_proto::api::is_proof(&a.proof), "{}", a.proof);
        let other = derive("correct horse battery!", &kdf).unwrap();
        assert_ne!(a.proof, other.proof);
        assert_ne!(derive("correct horse battery", &new_kdf_quick()).unwrap().proof, a.proof, "another salt");

        let vk = new_vault_key();
        let setup = wrap(&a, kdf.clone(), &vk).unwrap();
        assert!(setup.is_reasonable(), "{setup:?}");
        let back = unwrap(&again, &setup).unwrap();
        assert_eq!((back.id.as_str(), back.key.as_str()), (vk.id.as_str(), vk.key.as_str()));
        assert!(unwrap(&other, &setup).is_none(), "a wrong password can't unwrap it");

        let sealed = seal(&vk, "acc_1", b"{\"password\":\"p\"}").unwrap();
        assert_eq!(sealed, seal(&vk, "acc_1", b"{\"password\":\"p\"}").unwrap(), "same content, same seal");
        assert_ne!(sealed, seal(&vk, "acc_1", b"{\"password\":\"q\"}").unwrap());
        assert_eq!(open(&vk, "acc_1", &sealed).unwrap().as_slice(), b"{\"password\":\"p\"}");
        assert!(open(&vk, "acc_2", &sealed).is_none(), "bound to its section");
        assert!(open(&new_vault_key(), "acc_1", &sealed).is_none(), "and to its key");
    }

    fn new_kdf_quick() -> Kdf {
        quick_kdf()
    }

    #[test]
    fn unusual_settings_are_refused() {
        let mut k = quick_kdf();
        k.m_kib = 8;
        assert!(derive("whatever whatever", &k).is_err());
        assert!(check_new_password("short", "a@b.c").is_err());
        assert!(check_new_password("my name is sara123!", "sara@x.com").is_err());
        assert!(check_new_password("violet canyon lamp", "sara@x.com").is_ok());
    }
}
