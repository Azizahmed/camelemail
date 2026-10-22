# remail-server

Accounts, sign-in, encrypted settings sync and org config for Remail (design:
[docs/platform/design.md](../../docs/platform/design.md)). The server stores setup, never mail:
every user section is AES-256-GCM ciphertext under the user's data key, wrapped by a master key
that lives in a key file outside the database.

Milestones M0–M2 are in: config, migrations, envelope crypto and key tooling, `/v1/meta`, auth
(sign-up, verification, login, TOTP + backup codes, refresh rotation, devices, reset, lockout),
sections with `If-Match`, the provider allow-list, org config and providers, the Google code
exchange and refresh (with the org's client secret), change notices over WebSocket, and on-prem
licence enforcement. The admin console, AI proxy and push worker come later.

## Run it locally

```sh
# 1. Postgres (throwaway container on 127.0.0.1:55433, data in tmpfs)
scripts/test-postgres.sh
export DATABASE_URL=postgres://remail:remail@127.0.0.1:55433/postgres

# 2. A master key file (never in the database; back it up)
export REMAIL_MASTER_KEY_FILE=$HOME/.local/share/remail-dev/master.key
cargo run -p remail-server -- key init

# 3. Schema, then the API on 127.0.0.1:8080
export REMAIL_MODE=hosted REMAIL_DEV_MAIL=1   # open sign-up; mail readable at GET /v1/dev/mail
cargo run -p remail-server -- migrate
cargo run -p remail-server -- serve
```

`REMAIL_MODE=onprem` (the default) makes one invite-only org on first `serve` and prints a one-time
admin invite URL. Its token (`t=…`) goes in the `invite` field of `POST /v1/auth/signup`.

Or with Docker only: `docker compose -f deploy/dev/docker-compose.yml up -d --build` (Postgres +
server on `http://127.0.0.1:8080`, key in its own volume).

## Environment

| Variable | Default | |
|---|---|---|
| `DATABASE_URL` | required | Postgres URL |
| `REMAIL_MODE` | `onprem` | `hosted` (public org, open sign-up, e-mail verification) or `onprem` (one org, invites) |
| `REMAIL_BIND` | `127.0.0.1:8080` | Listen address |
| `REMAIL_PUBLIC_URL` | `http://<bind>` | Base of links in e-mails and invites |
| `REMAIL_MASTER_KEY_FILE` | `/etc/remail/master.key` | Key file (JSON keyring, mode 0600) |
| `REMAIL_SMTP_URL` | unset | lettre URL, e.g. `smtps://user:pass@smtp.example.com`. Unset: no mail is sent |
| `REMAIL_MAIL_FROM` | `Remail <no-reply@localhost>` | Sender |
| `REMAIL_DEV_MAIL` | unset | `1` in **debug builds**: keep mail in memory at `GET /v1/dev/mail` |
| `REMAIL_TRUST_PROXY` | unset | `1`: take the client IP from `X-Forwarded-For` (behind Caddy) |
| `REMAIL_ORG_NAME` | `My organisation` | On-prem: name of the org created on first start |
| `REMAIL_GOOGLE_CLIENT_ID` | unset | Hosted: Google OAuth client ("Desktop app" type) for Gmail + Google Calendar sign-in in the public org |
| `REMAIL_GOOGLE_CLIENT_SECRET` | unset | Its client secret (an installed-app secret: sent to devices, which talk to Google themselves; needs `REMAIL_GOOGLE_CLIENT_ID` too) |
| `REMAIL_MICROSOFT_CLIENT_ID` | unset | Hosted: Microsoft Entra app (public client) for Outlook / Microsoft 365 mail + calendar sign-in in the public org |
| `REMAIL_MICROSOFT_TENANT` | `common` | Its tenant (`common`, `organizations`, `consumers` or a tenant id) |
| `REMAIL_GOOGLE_TOKEN_URL` | Google's | Token endpoint for the server-side code exchange (tests) |
| `REMAIL_BACKUP_PASSPHRASE` | prompt | Passphrase for `key backup` / `key restore` |
| `REMAIL_LICENCE_PUBKEYS` | unset | **Build time.** Comma-separated base64url Ed25519 issuer keys. Unset: no licence enforcement |
| `RUST_LOG` | `info,sqlx=warn` | Log filter |

## Commands

```text
remail-server serve                                  # migrates, then serves
remail-server migrate                                # hosted: also creates the public org
remail-server key init | rotate | verify
remail-server key backup --out FILE                  # argon2id-stretched, AES-GCM sealed
remail-server key restore --from FILE [--out PATH]
remail-server key prune --version N [--yes]          # refuses while data keys still use mkN
remail-server org create --name NAME --slug CODE [--signup open|invite]
remail-server org config set --org CODE --file config.json     # remail_proto::org::OrgConfig
remail-server org secrets set --org CODE --file secrets.json   # remail_proto::org::OrgSecrets
remail-server user invite --org CODE --email E --role admin|user
remail-server licence install --file LICENCE
```

`key rotate` adds a master key, makes it active and re-wraps every data key in batches; the data
itself is not re-encrypted. Back up the new key file, then `key prune` the old version.

## Tests

```sh
eval "$(scripts/test-postgres.sh)"     # exports REMAIL_TEST_DATABASE_URL
cargo test -p remail-server
```

Every integration test creates its own database (`test_<uuid>`) and drops it. Without
`REMAIL_TEST_DATABASE_URL` the database tests print a skip notice and pass.

The real desktop client against this server (in-process, feature `testing`), GreenMail and a mock
Microsoft sign-in: `REMAIL_PLATFORM_IT=1 cargo test -p remail-core --test platform_e2e` (needs
`./scripts/test-server.sh` too). Stop the container with
`docker stop remail-test-pg`.

## Layout

- `crypto/`: key file and `KeyProvider`, data keys and their cache, sealed backups.
- `sections/`: the sync path, and the **only** code that can decrypt user sections (the user data
  key is handed out against `SyncPath`, which only this module can build;
  `tests/admin_isolation.rs` checks it).
- `auth/`: sign-up, login, sessions, TOTP; `orgs.rs`, `org_api.rs`: org rows, config, providers.
- `notices.rs`: WebSocket notices, in-process broadcast plus Postgres `NOTIFY` across processes.
- `licence.rs`: on-prem licence state, seat limit, clock-rollback guard.
- `admin/`: empty until M3.
