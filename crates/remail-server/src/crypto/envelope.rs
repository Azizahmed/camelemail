//! Data keys: creation, unwrap, and a short-lived in-memory cache.

use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use sqlx::{PgPool, Row};

use super::{CryptoError, DataKey, KeyProvider};

/// Unwrapped DKs stay cached this long, then are dropped (and zeroized).
const CACHE_TTL: Duration = Duration::from_secs(5 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OwnerKind {
    User,
    Org,
}

impl OwnerKind {
    pub fn as_str(self) -> &'static str {
        match self {
            OwnerKind::User => "user",
            OwnerKind::Org => "org",
        }
    }
}

pub struct Envelope {
    provider: Arc<dyn KeyProvider>,
    cache: Mutex<HashMap<(OwnerKind, String), (DataKey, Instant)>>,
}

impl Envelope {
    pub fn new(provider: Arc<dyn KeyProvider>) -> Self {
        Envelope { provider, cache: Mutex::new(HashMap::new()) }
    }

    pub fn provider(&self) -> &Arc<dyn KeyProvider> {
        &self.provider
    }

    /// Records every master-key version the provider holds in `master_keys`
    /// (wrapped keys reference it).
    pub async fn register_master_keys(&self, db: &PgPool) -> Result<(), CryptoError> {
        for v in self.provider.versions() {
            let fp = self.provider.fingerprint(v).unwrap_or_default();
            sqlx::query("insert into master_keys (version, fingerprint) values ($1, $2) on conflict (version) do nothing")
                .bind(v)
                .bind(fp)
                .execute(db)
                .await?;
        }
        Ok(())
    }

    /// Makes a fresh data key for `owner_id`, stores it wrapped, and returns it.
    pub async fn create_key(
        &self,
        db: &mut sqlx::PgConnection,
        kind: OwnerKind,
        owner_id: &str,
        org_id: &str,
    ) -> Result<DataKey, CryptoError> {
        let dk = DataKey::generate();
        let (version, wrapped) = self.provider.wrap(org_id, owner_id, &dk)?;
        // A key file rotated on the host may be ahead of `master_keys`.
        let fp = self.provider.fingerprint(version).unwrap_or_default();
        sqlx::query("insert into master_keys (version, fingerprint) values ($1, $2) on conflict (version) do nothing")
            .bind(version)
            .bind(fp)
            .execute(&mut *db)
            .await?;
        sqlx::query("insert into wrapped_keys (owner_kind, owner_id, org_id, mk_version, wrapped) values ($1, $2, $3, $4, $5)")
            .bind(kind.as_str())
            .bind(owner_id)
            .bind(org_id)
            .bind(version)
            .bind(wrapped)
            .execute(&mut *db)
            .await?;
        Ok(dk)
    }

    async fn load(&self, db: &PgPool, kind: OwnerKind, owner_id: &str) -> Result<DataKey, CryptoError> {
        let key = (kind, owner_id.to_string());
        if let Ok(mut cache) = self.cache.lock() {
            let now = Instant::now();
            cache.retain(|_, (_, at)| now.duration_since(*at) < CACHE_TTL);
            if let Some((dk, _)) = cache.get(&key) {
                return Ok(dk.clone());
            }
        }
        let row = sqlx::query("select org_id, mk_version, wrapped from wrapped_keys where owner_kind = $1 and owner_id = $2")
            .bind(kind.as_str())
            .bind(owner_id)
            .fetch_optional(db)
            .await?
            .ok_or_else(|| CryptoError::MissingKey(format!("{} {owner_id}", kind.as_str())))?;
        let org_id: String = row.try_get("org_id")?;
        let version: i32 = row.try_get("mk_version")?;
        let wrapped: Vec<u8> = row.try_get("wrapped")?;
        let dk = self.provider.unwrap(version, &org_id, owner_id, &wrapped)?;
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(key, (dk.clone(), Instant::now()));
        }
        Ok(dk)
    }

    /// The org's data key: org config and secrets, TOTP seeds, push tokens.
    pub async fn org_key(&self, db: &PgPool, org_id: &str) -> Result<DataKey, CryptoError> {
        self.load(db, OwnerKind::Org, org_id).await
    }

    /// A user's data key. Only the sync path can ask for it: the
    /// [`SyncPath`](crate::sections::SyncPath) token can't be built outside
    /// `sections`.
    pub async fn user_key(
        &self,
        db: &PgPool,
        _path: &crate::sections::SyncPath,
        user_id: &str,
    ) -> Result<DataKey, CryptoError> {
        self.load(db, OwnerKind::User, user_id).await
    }

    /// Drops every cached key (tests, and after the key file changed).
    pub fn clear_cache(&self) {
        if let Ok(mut c) = self.cache.lock() {
            c.clear();
        }
    }
}
