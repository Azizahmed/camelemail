# Zero-knowledge mailbox sign-ins

Decided 2026-10-27. Mailbox credentials (IMAP/SMTP passwords, Microsoft and
Google sign-ins, CalDAV passwords) sync between a user's devices, and the
server can't read them. One Camel Mail sign-in on a new device brings them
all back. This replaces the server-held encryption of `cred:<id>` sections
(design.md §4) for credentials; other sections stay as they are.

## Keys

Everything is made on the device; the server sees only the proof and the
wrapped vault key.

```
password ──Argon2id(salt, m=64 MiB, t=3, p=1)──► master key (32 B, never leaves the device)
master key ──HKDF-SHA256 "camel-mail/auth/v1"──► auth key ──► proof "v1.<base64url>"  → server
master key ──HKDF-SHA256 "camel-mail/vault/v1"─► vault KEK
vault key (32 B random, one per account) ──AES-256-GCM(vault KEK, AAD "camel-mail/vault-key/v1|<key id>")──► key_enc → server
```

- The server stores `argon2id(proof)` where it stored `argon2id(password)`,
  plus `kdf` (algorithm, cost, salt), `vault_key_enc` and `vault_key_id`.
  It never receives the password.
- Each device keeps the vault key in its secret store (keyring / Android
  Keystore-sealed file) until it signs out.
- A credential section's record is `{kind: "sealed", vk: <key id>, sealed:
  base64(nonce ‖ AES-256-GCM(vault key, JSON, AAD "camel-mail/cred/v1|<owner>|<key id>"))}`.
  The server's own envelope encryption still wraps the whole section.

## Flows

| Flow | Device | Server |
|---|---|---|
| Prelogin | `POST /v1/auth/prelogin {email}` | `{kdf}`; unknown addresses get a stable made-up kdf, so it doesn't tell who has an account. `kdf: null` = an account not converted yet. |
| Sign up | Checks the password's strength, derives keys, makes the vault key | Stores hash(proof), kdf, vault key; no strength check (it can't see the password) |
| Sign in | Derives the proof, signs in, unwraps the vault key from the tokens' `vault` | Checks hash(proof) as before; adds `vault` to the sign-in tokens |
| Convert (first sign-in on the new app) | Sends the password once more, with the new proof and vault | Checks the password, then stores the proof and vault instead. Done once. |
| Unlock (a device signed in before conversion, or whose vault key is missing) | Asks for the password; `GET /v1/me/vault`, derives, unwraps. Unconverted account: `POST /v1/me/vault` converts it. | |
| Change password | Unwraps with the old password, wraps the same vault key with the new one | Checks the old proof, stores the new proof and wrapping. Credentials stay readable. |
| Forgot password | New vault key | Stores it and **deletes the sealed credentials** (nobody can read them any more). Mailboxes need signing in again. The e-mail link's web page only shows the code; the new password is chosen in the app. |
| Old app on a converted account | — | `update_required`, without counting a failed sign-in |

Old apps keep working on accounts that haven't converted, so devices can
update one at a time. Once an account has converted, the server refuses a
credential section holding a readable password or grant (an old app would
otherwise upload them in the clear), so it only ever stores sealed ones.

## Google

Google sign-ins now sync too, sealed like the rest: the server holds only
data it can't read (relevant to Google's restricted-scope review). Gmail
snoozes still stay on the device.

## What it does not protect

A weak password can be guessed offline by whoever holds the server's
database (Argon2id slows that down; sign-up asks for a strong password). A
signed-in device holds the vault key, like it holds the mailbox tokens.
