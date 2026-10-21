//! Encrypting and decrypting user sections. Private to `sections`: nothing
//! else in the server can reach these functions.

use remail_proto::sync::SectionDoc;

use super::SyncPath;
use crate::app::AppState;
use crate::auth::AuthCtx;
use crate::crypto::{open, seal, section_aad, DataKey};
use crate::error::AppResult;

/// The caller's own data key.
pub(super) async fn user_key(s: &AppState, ctx: &AuthCtx) -> AppResult<DataKey> {
    Ok(s.env.user_key(&s.db, &SyncPath(()), &ctx.user_id).await?)
}

/// Decrypts one of the caller's sections. The handler audits it
/// (`decrypt.sync`) once the request is done.
pub(super) fn decrypt_user_section(
    dk: &DataKey,
    ctx: &AuthCtx,
    section: &str,
    version: i64,
    ciphertext: &[u8],
) -> AppResult<SectionDoc> {
    let plain = open(dk.bytes(), &section_aad(&ctx.user_id, section, version), ciphertext)?;
    Ok(serde_json::from_slice(&plain).map_err(anyhow::Error::from)?)
}

pub(super) fn encrypt_user_section(dk: &DataKey, ctx: &AuthCtx, section: &str, version: i64, plaintext: &[u8]) -> Vec<u8> {
    seal(dk.bytes(), &section_aad(&ctx.user_id, section, version), plaintext)
}
