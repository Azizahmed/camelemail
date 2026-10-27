-- End-to-end encrypted mailbox sign-ins (docs/platform/zero-knowledge.md).
-- An account with a kdf signs in with a password proof (password_hash is
-- then argon2id(proof)); vault_key_enc is its vault key, wrapped with a key
-- only the password gives.
alter table users add column kdf jsonb;
alter table users add column vault_key_enc text;
alter table users add column vault_key_id text;
