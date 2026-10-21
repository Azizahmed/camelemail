-- Remail server schema (design §6). Every secret or piece of user setup is
-- AES-GCM ciphertext under a data key that is itself wrapped by a master key
-- kept outside this database. E-mail addresses are stored lower-cased.

create table orgs (
  id text primary key,                        -- org_<uuid>
  name text not null,
  slug text unique not null,                  -- org code
  signup_mode text not null default 'invite', -- open | invite
  allowed_domains text[] not null default '{}',
  server_url text,                            -- hosted directory: where this org's server lives
  kms_key_id text,
  disabled_at timestamptz,
  created_at timestamptz not null default now()
);

create table users (
  id text primary key,                        -- usr_<uuid>
  org_id text not null references orgs(id),
  email text not null,
  name text not null,
  role text not null default 'user',          -- owner | admin | user
  locale text not null default 'en',
  password_hash text not null,                -- argon2id PHC string
  email_verified_at timestamptz,
  totp_secret_enc bytea,                      -- DK_org; pending until totp_enabled_at is set
  totp_enabled_at timestamptz,
  totp_last_step bigint,                      -- replay guard
  disabled_at timestamptz,
  failed_logins int not null default 0,
  locked_until timestamptz,
  last_active_at timestamptz,
  created_at timestamptz not null default now(),
  unique (org_id, email)
);
-- Sign-in is by e-mail alone, so an address belongs to one user server-wide.
create unique index users_email on users (email);

create table backup_codes (
  user_id text not null references users(id) on delete cascade,
  code_hash bytea not null,
  used_at timestamptz,
  primary key (user_id, code_hash)
);

create table devices (
  id text primary key,                        -- dev_<uuid>
  user_id text not null references users(id) on delete cascade,
  name text not null,
  platform text not null,                     -- linux | windows | macos | ios | android
  app_version text,
  push_kind text,
  push_token_enc bytea,
  created_at timestamptz not null default now(),
  last_seen_at timestamptz,
  revoked_at timestamptz
);
create index devices_user on devices (user_id);

create table sessions (
  id text primary key,                        -- ses_<uuid>
  device_id text not null references devices(id) on delete cascade,
  user_id text not null references users(id) on delete cascade,
  family text not null,                       -- refresh-token family (reuse revokes it)
  refresh_hash bytea not null unique,         -- SHA-256 of the opaque refresh token
  access_hash bytea unique,
  access_expires_at timestamptz,
  created_at timestamptz not null default now(),
  expires_at timestamptz not null,
  rotated_at timestamptz,
  revoked_at timestamptz
);
create index sessions_family on sessions (family);
create index sessions_device on sessions (device_id);

-- E-mail verification, password reset, invites and 2FA challenges.
create table auth_tokens (
  hash bytea primary key,                     -- SHA-256 of the token
  kind text not null,                         -- verify | reset | invite | 2fa | 2fa_setup
  user_id text,
  org_id text not null,
  email text,
  role text,
  meta jsonb not null default '{}',           -- e.g. the device a 2FA challenge signs in
  attempts int not null default 0,
  created_at timestamptz not null default now(),
  expires_at timestamptz not null,
  used_at timestamptz
);

create table master_keys (
  version int primary key,
  fingerprint text not null,
  created_at timestamptz not null default now(),
  retired_at timestamptz
);

create table wrapped_keys (
  owner_kind text not null,                   -- user | org
  owner_id text not null,
  org_id text not null references orgs(id),
  mk_version int not null references master_keys(version),
  wrapped bytea not null,                     -- nonce || ciphertext || tag
  primary key (owner_kind, owner_id)
);
create index wrapped_keys_version on wrapped_keys (mk_version);

create table user_sections (
  user_id text not null references users(id) on delete cascade,
  section text not null,
  version bigint not null,
  ciphertext bytea not null,
  updated_at timestamptz not null default now(),
  updated_by text,                            -- device id
  primary key (user_id, section)
);

create table org_config (
  org_id text primary key references orgs(id),
  version bigint not null,
  ciphertext bytea not null,
  updated_at timestamptz not null default now()
);

create table org_secrets (
  org_id text primary key references orgs(id),
  version bigint not null,
  ciphertext bytea not null,
  updated_at timestamptz not null default now()
);

-- Server send-later (M5).
create table scheduled_sends (
  id text primary key,
  user_id text not null references users(id) on delete cascade,
  account_id text not null,
  send_at timestamptz not null,
  ciphertext bytea not null,
  status text not null default 'pending',
  attempts int not null default 0,
  last_error_code text,
  claimed_by text,
  claimed_at timestamptz
);

-- Snooze wake and reminders (ids and times only, M5).
create table push_jobs (
  id bigserial primary key,
  user_id text not null,
  kind text not null,
  due_at timestamptz not null,
  ref text not null,
  done_at timestamptz
);

-- AI proxy usage: metadata only, never bodies (M4).
create table ai_usage (
  id bigserial primary key,
  org_id text not null,
  user_id text not null,
  feature text not null,
  model text not null,
  input_tokens int,
  output_tokens int,
  ms int,
  status text,
  at timestamptz not null default now()
);

create table licences (
  id text primary key,
  raw text not null,
  customer text not null,
  seats int not null,
  features text[] not null,
  starts_at date not null,
  expires_at date not null,
  installed_at timestamptz not null default now(),
  active boolean not null default true
);

create table deployment (
  id text primary key,
  mode text not null,
  last_seen_clock timestamptz not null,
  clock_mac bytea not null                    -- HMAC (key derived from DK_org) of last_seen_clock
);

create table audit_log (
  id bigserial primary key,
  at timestamptz not null default now(),
  org_id text,
  actor_kind text not null,                   -- user | admin | owner | system
  actor_id text,
  action text not null,                       -- e.g. auth.login, decrypt.sync
  target text,
  ip inet,
  meta jsonb not null default '{}'            -- never secrets or content
);
create index audit_log_org_at on audit_log (org_id, at desc);
create index audit_log_action_target on audit_log (action, target, at desc);
