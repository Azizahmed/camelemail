# Hosting remail-server on a platform (CranL, or any Docker host)

The hosted service runs on CranL (project **Remail**, application `remail-server`, region Riyadh) at
`https://remail-server-nzymau.cranl.net`, with a CranL PostgreSQL database (`remail-db`) in the same region.
Release builds of the app use that address unless they're built with `REMAIL_SERVER`.

The image is built on GitHub, not on the platform:

1. A push to `main` that touches the server (`crates/remail-server`, `crates/remail-proto`,
   `Cargo.toml`, `Cargo.lock` or `deploy/server/build.Dockerfile`) runs
   `.github/workflows/server-image.yml`. It builds `deploy/server/build.Dockerfile` and pushes
   `ghcr.io/azizahmed/remail-server` (tags `latest` and the commit SHA).
2. The workflow then pins the one-line root `Dockerfile` (`FROM ghcr.io/azizahmed/remail-server@sha256:…`)
   to the new image and commits that to `main`.
3. In CranL, click **Deploy**. CranL doesn't deploy on push; it pulls the pinned image in seconds.

CranL application settings:

| Setting | Value |
|---|---|
| Repository / branch | `Azizahmed/camelemail`, `main` (public repository) |
| Root directory | `/` |
| Build type | Dockerfile, path `Dockerfile` (the repository root; platforms build in the Dockerfile's folder) |
| Port | `8080` |

It also needs a PostgreSQL database (16 or newer).

## Environment

| Variable | Value |
|---|---|
| `DATABASE_URL` | The database's connection URL (`postgres://user:pass@host:5432/db`) |
| `REMAIL_MASTER_KEY` | The master key: base64 of the key file (see below). **Secret.** |
| `REMAIL_PUBLIC_URL` | The app's public `https://` address (used in e-mail and invite links) |
| `REMAIL_MODE` | `hosted` (the image default) or `onprem` |
| `REMAIL_SMTP_URL`, `REMAIL_MAIL_FROM` | Optional. Without them, nothing is e-mailed (see "First account"). |
| `REMAIL_SNDR_API_KEY` | Alternative to SMTP: an API key for SNDR, CranL's e-mail service (`REMAIL_MAIL_FROM` must use a domain verified there). **Secret.** |
| `REMAIL_ADMIN_EMAILS` | Optional. Comma-separated addresses of the service's owners: their accounts become `owner` on every start (and at sign-up), so they see Settings › Admin in the app. |

The image already sets `PORT=8080`, `REMAIL_TRUST_PROXY=1` (client addresses come from the
platform's proxy) and plain-text logs.

## Master key

Create the key once, on a trusted machine, and keep the file somewhere safe (a password manager,
or an encrypted backup):

```sh
REMAIL_MASTER_KEY_FILE=./remail-master.key cargo run -p remail-server -- key init
base64 -w0 remail-master.key        # paste this into the REMAIL_MASTER_KEY secret
```

- The server refuses to start with a key that doesn't match its database, so a lost or mistyped
  key can't silently start a second, unrelated set of data.
- **Losing the key file and every copy of it loses every user's settings and mailbox
  credentials.** Mail itself stays at the providers.

## First account

Until the service has an owner, every start prints a one-time owner invite in the logs:

```
No owner yet: create the owner account with this one-time invite (7 days):

  https://<your app>/invite?t=…
```

Open it, then in the Remail app choose **Create account** and paste the invitation code. Invited
accounts need no e-mail confirmation. While no mail is configured (`REMAIL_SMTP_URL` or
`REMAIL_SNDR_API_KEY`), sign-up takes invitations only; once mail works, sign-up is open with an
e-mailed confirmation link.

## Checks

- `GET /health` answers `ok`.
- `GET /v1/meta` answers the mode, version and sign-up mode.
