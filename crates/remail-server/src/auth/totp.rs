//! TOTP (RFC 6238: SHA-1, 6 digits, 30 s, ±1 step) and backup codes.

use rand::Rng;
use subtle::ConstantTimeEq;
use totp_rs::{Algorithm, Secret, TOTP};

use crate::crypto::{random_bytes, sha256};

pub const STEP: u64 = 30;
pub const BACKUP_CODES: usize = 10;
const ISSUER: &str = "Remail";
/// No 0/o, 1/l/i: codes get typed from paper.
const BACKUP_ALPHABET: &[u8] = b"abcdefghjkmnpqrstuvwxyz23456789";

/// A fresh 160-bit secret.
pub fn new_secret() -> Vec<u8> {
    random_bytes::<20>().to_vec()
}

pub fn base32(secret: &[u8]) -> String {
    match Secret::Raw(secret.to_vec()).to_encoded() {
        Secret::Encoded(s) => s,
        Secret::Raw(_) => String::new(),
    }
}

fn pct(s: &str) -> String {
    s.bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'.' | b'_' | b'~' | b'@' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

pub fn otpauth_url(secret: &[u8], email: &str) -> String {
    format!(
        "otpauth://totp/{ISSUER}:{}?secret={}&issuer={ISSUER}&algorithm=SHA1&digits=6&period={STEP}",
        pct(email),
        base32(secret)
    )
}

fn totp(secret: &[u8]) -> Option<TOTP> {
    TOTP::new(Algorithm::SHA1, 6, 1, STEP, secret.to_vec()).ok()
}

/// The code for `unix_secs` (tests and the client's own check).
pub fn code_at(secret: &[u8], unix_secs: u64) -> String {
    totp(secret).map(|t| t.generate(unix_secs)).unwrap_or_default()
}

/// The time step `code` matches within ±1 step of `now`, if it's newer than
/// `last_step` (a code can't be replayed).
pub fn verify(secret: &[u8], code: &str, now_secs: u64, last_step: Option<i64>) -> Option<i64> {
    let code = code.trim();
    if code.len() != 6 || !code.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let t = totp(secret)?;
    let current = (now_secs / STEP) as i64;
    let mut hit = None;
    for step in [current - 1, current, current + 1] {
        if step < 0 {
            continue;
        }
        let expected = t.generate(step as u64 * STEP);
        if bool::from(expected.as_bytes().ct_eq(code.as_bytes())) && last_step.is_none_or(|l| step > l) {
            hit = Some(step);
        }
    }
    hit
}

pub fn is_totp_code(code: &str) -> bool {
    let c = code.trim();
    c.len() == 6 && c.bytes().all(|b| b.is_ascii_digit())
}

/// Ten `xxxx-xxxx` codes.
pub fn new_backup_codes() -> Vec<String> {
    let mut rng = rand::rngs::OsRng;
    (0..BACKUP_CODES)
        .map(|_| {
            let mut s = String::with_capacity(9);
            for i in 0..8 {
                if i == 4 {
                    s.push('-');
                }
                s.push(BACKUP_ALPHABET[rng.gen_range(0..BACKUP_ALPHABET.len())] as char);
            }
            s
        })
        .collect()
}

/// Stored form of a backup code: case, spaces and dashes don't matter.
pub fn backup_hash(user_id: &str, code: &str) -> Vec<u8> {
    let norm: String = code.chars().filter(|c| c.is_ascii_alphanumeric()).collect::<String>().to_lowercase();
    sha256(format!("remail/backup/v1|{user_id}|{norm}").as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn totp_window_and_replay() {
        let s = new_secret();
        let now = 1_800_000_000;
        let code = code_at(&s, now);
        let step = (now / STEP) as i64;
        assert_eq!(verify(&s, &code, now, None), Some(step));
        assert_eq!(verify(&s, &code, now + STEP, None), Some(step), "one step late is fine");
        assert_eq!(verify(&s, &code, now + 2 * STEP, None), None, "two steps late is not");
        assert_eq!(verify(&s, &code, now, Some(step)), None, "no replay");
        assert_eq!(verify(&s, "12345", now, None), None);
        assert!(otpauth_url(&s, "a b@x.com").starts_with("otpauth://totp/Remail:a%20b@x.com?secret="));
    }

    #[test]
    fn backup_codes_shape() {
        let codes = new_backup_codes();
        assert_eq!(codes.len(), 10);
        assert!(codes.iter().all(|c| c.len() == 9 && c.as_bytes()[4] == b'-'));
        assert_eq!(backup_hash("u", &codes[0]), backup_hash("u", &codes[0].to_uppercase().replace('-', " ")));
        assert_ne!(backup_hash("u", &codes[0]), backup_hash("v", &codes[0]));
    }
}
