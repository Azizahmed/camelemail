//! Password hashing (argon2id) and the strength rule.

use anyhow::{anyhow, Result};
use argon2::password_hash::{PasswordHash, PasswordHasher, PasswordVerifier, SaltString};
use argon2::{Algorithm, Argon2, Params, Version};
use remail_proto::ErrorCode;
use zeroize::Zeroizing;

use crate::config::{Argon2Cost, Config};
use crate::error::AppError;

pub const MIN_LENGTH: usize = 10;
pub const MAX_LENGTH: usize = 256;

fn argon(cost: Argon2Cost) -> Result<Argon2<'static>> {
    let params = Params::new(cost.m_kib, cost.t, cost.p, None).map_err(|e| anyhow!("argon2 params: {e}"))?;
    Ok(Argon2::new(Algorithm::Argon2id, Version::V0x13, params))
}

/// argon2id PHC string. Runs off the async threads (64 MiB, ~100 ms).
pub async fn hash(cfg: &Config, password: &str) -> Result<String> {
    let cost = cfg.argon2;
    let pw = Zeroizing::new(password.to_string());
    tokio::task::spawn_blocking(move || {
        let salt = SaltString::generate(&mut rand::rngs::OsRng);
        argon(cost)?
            .hash_password(pw.as_bytes(), &salt)
            .map(|h| h.to_string())
            .map_err(|e| anyhow!("argon2: {e}"))
    })
    .await?
}

/// Checks `password` against a PHC string (its own parameters apply).
pub async fn verify(password: &str, phc: &str) -> bool {
    let pw = Zeroizing::new(password.to_string());
    let phc = phc.to_string();
    tokio::task::spawn_blocking(move || {
        let Ok(parsed) = PasswordHash::new(&phc) else { return false };
        Argon2::default().verify_password(pw.as_bytes(), &parsed).is_ok()
    })
    .await
    .unwrap_or(false)
}

/// A hash to verify against when the e-mail is unknown, so the answer takes
/// as long as for a real account.
pub async fn dummy_hash(cfg: &Config) -> Result<String> {
    let mut pw = [0u8; 16];
    rand::RngCore::fill_bytes(&mut rand::rngs::OsRng, &mut pw);
    let pw: String = pw.iter().map(|b| format!("{b:02x}")).collect();
    hash(cfg, &pw).await
}

/// At least 10 characters, zxcvbn score ≥ 3 (with the user's name and e-mail
/// as known words), and not containing the e-mail's local part.
pub fn check_strength(password: &str, email: &str, name: &str) -> Result<(), AppError> {
    let weak = |msg: &str| Err(AppError::new(ErrorCode::WeakPassword, msg));
    let len = password.chars().count();
    if len < MIN_LENGTH {
        return weak("Use at least 10 characters.");
    }
    if len > MAX_LENGTH {
        return weak("That password is too long.");
    }
    let lower = password.to_lowercase();
    let local = email.split('@').next().unwrap_or("").to_lowercase();
    if lower.contains(&email.to_lowercase()) || (local.chars().count() >= 3 && lower.contains(&local)) {
        return weak("Don't use your e-mail address in your password.");
    }
    let mut inputs: Vec<&str> = vec![email, local.as_str()];
    inputs.extend(name.split_whitespace());
    let entropy = zxcvbn::zxcvbn(password, &inputs);
    if (entropy.score() as u8) < 3 {
        return weak("That password is too easy to guess. Try a longer phrase.");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn strength_rule() {
        let ok = |p: &str| check_strength(p, "sara.ahmed@example.com", "Sara Ahmed").is_ok();
        assert!(!ok("short"));
        assert!(!ok("password123"));
        assert!(!ok("aaaaaaaaaaaa"));
        assert!(!ok("sara.ahmed-Lighthouse-91"), "contains the e-mail local part");
        assert!(ok("correct horse battery staple"));
        assert!(ok("Tq7#mVz!p2Lw"));
    }

    #[tokio::test]
    async fn hash_and_verify() {
        let mut cfg = Config::new("postgres://unused", crate::Mode::Hosted);
        cfg.argon2 = Argon2Cost { m_kib: 256, t: 1, p: 1 };
        let h = hash(&cfg, "correct horse battery staple").await.unwrap();
        assert!(h.starts_with("$argon2id$"));
        assert!(verify("correct horse battery staple", &h).await);
        assert!(!verify("wrong horse battery staple", &h).await);
        assert!(!verify("x", "not a hash").await);
    }
}
