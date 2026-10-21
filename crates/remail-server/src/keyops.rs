//! Master-key maintenance behind `remail-server key …`: rotation (re-wrap
//! data keys, never re-encrypt data), verification and pruning.

use anyhow::{bail, Result};
use sqlx::{FromRow, PgPool};

use crate::crypto::KeyProvider;

/// Rows re-wrapped per batch.
pub const BATCH: i64 = 500;

#[derive(FromRow)]
struct Wrapped {
    owner_kind: String,
    owner_id: String,
    org_id: String,
    mk_version: i32,
    wrapped: Vec<u8>,
}

/// Re-wraps every data key that isn't under the active master key. Data
/// itself is untouched: only `wrapped_keys` rows change. Safe to re-run; it
/// loops until nothing is left on an old version.
pub async fn rewrap_all(db: &PgPool, keys: &dyn KeyProvider, mut progress: impl FnMut(u64)) -> Result<u64> {
    let active = keys.active_version();
    let mut done = 0u64;
    loop {
        let batch: Vec<Wrapped> = sqlx::query_as(
            "select owner_kind, owner_id, org_id, mk_version, wrapped from wrapped_keys
             where mk_version <> $1 order by owner_kind, owner_id limit $2",
        )
        .bind(active)
        .bind(BATCH)
        .fetch_all(db)
        .await?;
        if batch.is_empty() {
            break;
        }
        let mut tx = db.begin().await?;
        for w in &batch {
            let dk = keys.unwrap(w.mk_version, &w.org_id, &w.owner_id, &w.wrapped)?;
            let rewrapped = keys.wrap_with(active, &w.org_id, &w.owner_id, &dk)?;
            // Conditional on the old version, in case another run got here first.
            sqlx::query(
                "update wrapped_keys set mk_version = $3, wrapped = $4
                 where owner_kind = $1 and owner_id = $2 and mk_version = $5",
            )
            .bind(&w.owner_kind)
            .bind(&w.owner_id)
            .bind(active)
            .bind(rewrapped)
            .bind(w.mk_version)
            .execute(&mut *tx)
            .await?;
        }
        tx.commit().await?;
        done += batch.len() as u64;
        progress(done);
    }
    sqlx::query(
        "update master_keys set retired_at = coalesce(retired_at, now())
         where version <> $1 and not exists (select 1 from wrapped_keys w where w.mk_version = master_keys.version)",
    )
    .bind(active)
    .execute(db)
    .await?;
    Ok(done)
}

#[derive(Debug, Default)]
pub struct VerifyReport {
    pub total: u64,
    pub failed: Vec<String>,
    /// (version, rows) for every version still referenced.
    pub by_version: Vec<(i32, i64)>,
    /// Versions whose recorded fingerprint differs from the key file.
    pub fingerprint_mismatch: Vec<i32>,
}

impl VerifyReport {
    pub fn ok(&self) -> bool {
        self.failed.is_empty() && self.fingerprint_mismatch.is_empty()
    }
}

/// Refuses keys that don't belong to this database: a master key whose
/// fingerprint differs from the one recorded, or missing versions that data
/// keys are still wrapped with. Run before serving, so a lost or mistyped key
/// can't quietly start wrapping new data under a key nobody has.
pub async fn check_keys(db: &PgPool, keys: &dyn KeyProvider) -> Result<()> {
    let fps: Vec<(i32, String)> = sqlx::query_as("select version, fingerprint from master_keys order by version")
        .fetch_all(db)
        .await?;
    for (v, fp) in &fps {
        if let Some(have) = keys.fingerprint(*v) {
            if &have != fp {
                anyhow::bail!(
                    "master key mk{v} is not the one this database was set up with (fingerprint {have}, expected {fp}); \
                     restore the original key (key restore) instead of creating a new one"
                );
            }
        }
    }
    let used: Vec<(i32, i64)> = sqlx::query_as("select mk_version, count(*) from wrapped_keys group by mk_version")
        .fetch_all(db)
        .await?;
    let have = keys.versions();
    let missing: Vec<String> = used.iter().filter(|(v, _)| !have.contains(v)).map(|(v, n)| format!("mk{v} ({n} keys)")).collect();
    if !missing.is_empty() {
        anyhow::bail!("the master key is missing {} that this database still uses; restore the original key", missing.join(", "));
    }
    Ok(())
}

/// Unwraps every data key (nothing is decrypted beyond the keys themselves).
pub async fn verify(db: &PgPool, keys: &dyn KeyProvider) -> Result<VerifyReport> {
    let mut report = VerifyReport::default();
    let fps: Vec<(i32, String)> = sqlx::query_as("select version, fingerprint from master_keys order by version")
        .fetch_all(db)
        .await?;
    for (v, fp) in fps {
        if let Some(have) = keys.fingerprint(v) {
            if have != fp {
                report.fingerprint_mismatch.push(v);
            }
        }
    }
    report.by_version = sqlx::query_as("select mk_version, count(*) from wrapped_keys group by mk_version order by mk_version")
        .fetch_all(db)
        .await?;
    let mut after: Option<(String, String)> = None;
    loop {
        let batch: Vec<Wrapped> = sqlx::query_as(
            "select owner_kind, owner_id, org_id, mk_version, wrapped from wrapped_keys
             where ($1::text is null or (owner_kind, owner_id) > ($1, $2))
             order by owner_kind, owner_id limit $3",
        )
        .bind(after.as_ref().map(|a| a.0.clone()))
        .bind(after.as_ref().map(|a| a.1.clone()))
        .bind(BATCH)
        .fetch_all(db)
        .await?;
        let Some(last) = batch.last() else { break };
        after = Some((last.owner_kind.clone(), last.owner_id.clone()));
        for w in &batch {
            report.total += 1;
            if keys.unwrap(w.mk_version, &w.org_id, &w.owner_id, &w.wrapped).is_err() {
                report.failed.push(format!("{} {} (mk{})", w.owner_kind, w.owner_id, w.mk_version));
            }
        }
    }
    Ok(report)
}

/// Refuses to drop a master key that is active or still wraps a data key.
pub async fn check_prunable(db: &PgPool, keys: &dyn KeyProvider, version: i32) -> Result<()> {
    if version == keys.active_version() {
        bail!("mk{version} is the active master key; rotate first");
    }
    if !keys.versions().contains(&version) {
        bail!("mk{version} is not in the key file");
    }
    let n: i64 = sqlx::query_scalar("select count(*) from wrapped_keys where mk_version = $1")
        .bind(version)
        .fetch_one(db)
        .await?;
    if n > 0 {
        bail!("{n} data keys are still wrapped with mk{version}; run `key rotate` (it re-wraps them) and try again");
    }
    Ok(())
}

/// Drops the database record of a pruned version.
pub async fn forget_version(db: &PgPool, version: i32) -> Result<()> {
    sqlx::query("delete from master_keys where version = $1").bind(version).execute(db).await?;
    Ok(())
}
