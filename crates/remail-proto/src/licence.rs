//! Signed licence files for on-prem deployments.
//!
//! `REMAIL-LICENCE-1.<base64url(payload JSON)>.<base64url(Ed25519 signature)>`,
//! the signature covering `REMAIL-LICENCE-1.<payload b64>`. The server checks it
//! offline against public keys compiled into it; only the issuer holds the
//! private key (`tools/remail-licensor`).

use base64::engine::general_purpose::URL_SAFE_NO_PAD as B64;
use base64::Engine as _;
use chrono::{DateTime, Duration, NaiveDate, Utc};
use ed25519_dalek::{Signature, Signer, SigningKey, Verifier, VerifyingKey};
use serde::{Deserialize, Serialize};

use crate::api::LicenceState;

pub const PREFIX: &str = "REMAIL-LICENCE-1";
/// Warnings start this many days before expiry.
pub const WARN_DAYS: i64 = 30;
/// After expiry, everything keeps working this many days.
pub const GRACE_DAYS: i64 = 14;
/// A clock this far behind the latest time seen counts as rolled back.
pub const ROLLBACK_TOLERANCE_HOURS: i64 = 24;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Licence {
    /// `lic_…`
    pub lid: String,
    pub customer: String,
    /// The deployment it's bound to (`dep_…`).
    pub deployment: String,
    pub seats: u32,
    /// ai, mcp, calendar, push…
    pub features: Vec<String>,
    pub starts: NaiveDate,
    /// Last valid day (inclusive).
    pub expires: NaiveDate,
    pub issued: DateTime<Utc>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub renew_url: Option<String>,
}

impl Licence {
    pub fn has(&self, feature: &str) -> bool {
        self.features.iter().any(|f| f == feature)
    }

    /// End of the last valid day (UTC).
    fn expiry(&self) -> DateTime<Utc> {
        (self.expires + Duration::days(1)).and_hms_opt(0, 0, 0).unwrap().and_utc()
    }

    pub fn grace_ends(&self) -> DateTime<Utc> {
        self.expiry() + Duration::days(GRACE_DAYS)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum LicenceError {
    #[error("not a Remail licence")]
    Format,
    #[error("the licence signature is not valid")]
    Signature,
    #[error("the licence is for another deployment ({0})")]
    WrongDeployment(String),
    #[error("the licence starts on {0}")]
    NotYetValid(NaiveDate),
    #[error("the system clock is behind the latest time this server has seen")]
    ClockRollback,
}

pub fn sign(licence: &Licence, key: &SigningKey) -> String {
    let payload = B64.encode(serde_json::to_vec(licence).expect("licence serialises"));
    let signed = format!("{PREFIX}.{payload}");
    let sig = key.sign(signed.as_bytes());
    format!("{signed}.{}", B64.encode(sig.to_bytes()))
}

/// Checks the signature only (no dates, no deployment).
pub fn decode(raw: &str, keys: &[VerifyingKey]) -> Result<Licence, LicenceError> {
    let raw = raw.trim();
    let mut parts = raw.split('.');
    let (Some(prefix), Some(payload), Some(sig), None) = (parts.next(), parts.next(), parts.next(), parts.next()) else {
        return Err(LicenceError::Format);
    };
    if prefix != PREFIX {
        return Err(LicenceError::Format);
    }
    let sig: [u8; 64] = B64
        .decode(sig)
        .ok()
        .and_then(|b| b.try_into().ok())
        .ok_or(LicenceError::Format)?;
    let sig = Signature::from_bytes(&sig);
    let signed = &raw[..prefix.len() + 1 + payload.len()];
    if !keys.iter().any(|k| k.verify(signed.as_bytes(), &sig).is_ok()) {
        return Err(LicenceError::Signature);
    }
    let json = B64.decode(payload).map_err(|_| LicenceError::Format)?;
    serde_json::from_slice(&json).map_err(|_| LicenceError::Format)
}

/// Where a valid licence stands at `now`.
pub fn state_at(licence: &Licence, now: DateTime<Utc>) -> LicenceState {
    let expiry = licence.expiry();
    if now >= licence.grace_ends() {
        LicenceState::Expired
    } else if now >= expiry {
        LicenceState::Grace
    } else if now >= expiry - Duration::days(WARN_DAYS) {
        LicenceState::Warning
    } else {
        LicenceState::Valid
    }
}

/// Full offline check: signature, deployment, start date, clock rollback.
/// `last_seen` is the latest wall-clock time this deployment has recorded.
pub fn verify(
    raw: &str,
    keys: &[VerifyingKey],
    deployment: &str,
    now: DateTime<Utc>,
    last_seen: Option<DateTime<Utc>>,
) -> Result<(Licence, LicenceState), LicenceError> {
    let lic = decode(raw, keys)?;
    if lic.deployment != deployment {
        return Err(LicenceError::WrongDeployment(lic.deployment));
    }
    if let Some(seen) = last_seen {
        if now < seen - Duration::hours(ROLLBACK_TOLERANCE_HOURS) {
            return Err(LicenceError::ClockRollback);
        }
    }
    if now.date_naive() < lic.starts {
        return Err(LicenceError::NotYetValid(lic.starts));
    }
    let state = state_at(&lic, now);
    Ok((lic, state))
}

/// Public key from its base64url form (as compiled into the server).
pub fn parse_public_key(b64: &str) -> Option<VerifyingKey> {
    let bytes: [u8; 32] = B64.decode(b64.trim()).ok()?.try_into().ok()?;
    VerifyingKey::from_bytes(&bytes).ok()
}

pub fn encode_public_key(key: &VerifyingKey) -> String {
    B64.encode(key.to_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;
    use rand_core::OsRng;

    fn lic() -> Licence {
        Licence {
            lid: "lic_1".into(),
            customer: "Acme".into(),
            deployment: "dep_1".into(),
            seats: 10,
            features: vec!["ai".into()],
            starts: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            expires: NaiveDate::from_ymd_opt(2026, 12, 31).unwrap(),
            issued: "2025-12-20T00:00:00Z".parse().unwrap(),
            renew_url: None,
        }
    }

    fn at(s: &str) -> DateTime<Utc> {
        s.parse().unwrap()
    }

    #[test]
    fn round_trip_and_states() {
        let key = SigningKey::generate(&mut OsRng);
        let raw = sign(&lic(), &key);
        let keys = [key.verifying_key()];
        let check = |now: &str| verify(&raw, &keys, "dep_1", at(now), None).map(|(_, s)| s);
        assert_eq!(check("2026-06-01T00:00:00Z"), Ok(LicenceState::Valid));
        assert_eq!(check("2026-12-02T00:00:00Z"), Ok(LicenceState::Warning));
        assert_eq!(check("2026-12-31T23:59:59Z"), Ok(LicenceState::Warning));
        assert_eq!(check("2027-01-01T00:00:00Z"), Ok(LicenceState::Grace));
        assert_eq!(check("2027-01-14T23:59:59Z"), Ok(LicenceState::Grace));
        assert_eq!(check("2027-01-15T00:00:00Z"), Ok(LicenceState::Expired));
        assert_eq!(check("2025-12-31T00:00:00Z"), Err(LicenceError::NotYetValid(lic().starts)));
    }

    #[test]
    fn tampering_is_caught() {
        let key = SigningKey::generate(&mut OsRng);
        let raw = sign(&lic(), &key);
        let keys = [key.verifying_key()];
        let now = at("2026-06-01T00:00:00Z");

        // More seats in the payload, old signature.
        let mut more = lic();
        more.seats = 10_000;
        let forged_payload = B64.encode(serde_json::to_vec(&more).unwrap());
        let sig = raw.rsplit('.').next().unwrap();
        let forged = format!("{PREFIX}.{forged_payload}.{sig}");
        assert_eq!(verify(&forged, &keys, "dep_1", now, None).unwrap_err(), LicenceError::Signature);

        // Flipped signature byte.
        let mut bytes = B64.decode(sig).unwrap();
        bytes[0] ^= 1;
        let bad = format!("{}.{}", &raw[..raw.rfind('.').unwrap()], B64.encode(bytes));
        assert_eq!(verify(&bad, &keys, "dep_1", now, None).unwrap_err(), LicenceError::Signature);

        // Signed by somebody else.
        let other = SigningKey::generate(&mut OsRng);
        assert_eq!(verify(&sign(&lic(), &other), &keys, "dep_1", now, None).unwrap_err(), LicenceError::Signature);

        // Garbage.
        assert_eq!(verify("hello", &keys, "dep_1", now, None).unwrap_err(), LicenceError::Format);
        assert_eq!(verify(&format!("{raw}.x"), &keys, "dep_1", now, None).unwrap_err(), LicenceError::Format);
    }

    #[test]
    fn bound_to_deployment_and_clock() {
        let key = SigningKey::generate(&mut OsRng);
        let raw = sign(&lic(), &key);
        let keys = [key.verifying_key()];
        let now = at("2026-06-01T00:00:00Z");
        assert_eq!(
            verify(&raw, &keys, "dep_2", now, None).unwrap_err(),
            LicenceError::WrongDeployment("dep_1".into())
        );
        let seen = at("2026-08-01T00:00:00Z");
        assert_eq!(verify(&raw, &keys, "dep_1", now, Some(seen)).unwrap_err(), LicenceError::ClockRollback);
        // Small drift is tolerated.
        assert!(verify(&raw, &keys, "dep_1", now, Some(now + Duration::hours(2))).is_ok());
    }

    #[test]
    fn rotated_issuer_keys() {
        let old = SigningKey::generate(&mut OsRng);
        let new = SigningKey::generate(&mut OsRng);
        let keys = [new.verifying_key(), old.verifying_key()];
        let now = at("2026-06-01T00:00:00Z");
        assert!(verify(&sign(&lic(), &old), &keys, "dep_1", now, None).is_ok());
        let pk = encode_public_key(&new.verifying_key());
        assert_eq!(parse_public_key(&pk), Some(new.verifying_key()));
    }
}
