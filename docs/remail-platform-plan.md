# Task: Remail Platform — Accounts, Server-Stored Settings, Mobile App, Admin Console & On-Prem Licensing

## Context (read the code before designing)
Remail today is a **local-first desktop** email + calendar client: Tauri 2 shell (`src-tauri/`), all
mail, calendar and crypto logic in Rust (`crates/remail-core`), Svelte 5 UI (`src/`), builds for
Linux, Windows and macOS. Settings live in local SQLite (`settings` keys `app`, `ai`, `mcp`) and
the webview's `localStorage`. Secrets live in the OS keyring. There is no server.

Already built and to be reused:
- IMAP/SMTP accounts, and Microsoft OAuth (PKCE, XOAUTH2, Outlook calendars via Graph delta sync).
- CalDAV with `.well-known/caldav` discovery, and iCal subscriptions.
- Smart Inbox, snooze, send later, undo send, AI assistance, an MCP server/client, and Arabic RTL
  everywhere.
- A temporary **Settings → Admin** page (allowed providers, Microsoft app ID, AI settings). This
  moves to the admin console and disappears from the user app.

**Target:** one product, run in two ways from the same code:
- **Hosted:** I operate the server, and users sign up.
- **On-prem:** a customer installs the full solution (server + admin console + apps pointed at
  their server) and pays a **yearly licence**.

**Decided (don't reopen):**
- **Encryption:** server-held, admin-managed encryption (§4). Each user's data is stored encrypted
  on the server and is never shown to anyone, admins included.
- **AI:** every AI call goes through the server. Provider keys are encrypted on the server and
  never reach a client.
- **Mobile:** Tauri 2 mobile, sharing `remail-core`. This plan covers **function**; the mobile
  visual design gets its own plan later, so build plain, usable phone screens and no more.

## 1. Clients
- **Desktop** (Linux, Windows, macOS) and **mobile (iOS, Android)** with Tauri 2.
- Mobile needs a working single-pane flow (list → thread → compose, calendar agenda/day), the
  OAuth redirect on mobile, keychain/keystore storage, background sync and push (§4). Keep the
  styling minimal and reuse the existing components where they fit.
- Every client **requires sign-in**. Nothing works signed out.
- **Server address:** hosted users use the default. On-prem users either enter their organisation's
  server (or an org code that resolves to it), or get a build preconfigured by their IT.
- All text in English and Arabic (RTL), including sign-up and the admin console.

## 2. Remail account (mandatory)
**First run:** a choice between **Create account** and **Sign in**.
- **Create account:** name, email/username, password (with a strength check), email verification
  (hosted) or an invite link (on-prem), then optional 2FA. Then connect the first mailbox.
- **Sign in on any device:** password + 2FA. The user's full setup (mailboxes, calendars,
  settings) comes down from the server, and the device starts syncing mail and calendars with
  progress shown. No reconfiguration.
- **Password reset:** by email (hosted) or by the org admin (on-prem). A reset restores access only;
  it never shows the admin any user data (the data stays encrypted with the org's keys, §4).
- Settings → **Account:** profile, password, 2FA, signed-in devices (with "sign out this device"),
  sign out.

## 3. Connecting mailboxes and calendars (users don't configure providers)
Users only **pick a provider and sign in**. The client gets the provider list and every technical
value (OAuth app IDs, scopes, tenant, server hosts, allowed providers) from the server's **org
configuration**. Users never see client IDs, server fields or an Admin menu.
- **Microsoft / Outlook / Microsoft 365:** existing OAuth flow (mail + calendars), using the org's
  Entra app ID.
- **Google / Gmail:** new OAuth (Gmail + Google Calendar scopes, PKCE). For the hosted product,
  Google's app verification and security assessment are needed before public use. On-prem
  customers can register their own Google Cloud app (an "Internal" Workspace app needs no
  verification), entered once in their admin console.
- **Other IMAP/SMTP + CalDAV:** only if the admin allows it. The admin can preset servers so users
  enter just email + password. Auto-detect and `.well-known/caldav` discovery stay as a fallback
  when manual servers are allowed.
- The allow-list is enforced **on the server**, not only hidden in the UI.

## 4. Data on the server: storage and encryption (decided)
**Everything the user sets up is stored on the server and follows them to every device.**

| Data | Contents | Stored as |
|---|---|---|
| Mailboxes | Accounts with stable IDs (`acc_…`), provider, display name, colour, unified-inbox flag, auto-BCC, signatures, folder prefs, **credentials** (OAuth refresh tokens, IMAP passwords) | user data, encrypted |
| Calendars | Calendar accounts and credentials, iCal subscriptions, colours/visibility, default calendar, week start | user data, encrypted |
| Settings | `AppSettings`, shortcuts, templates, sender rules, notification prefs, language | user data, encrypted |
| Device-only | Window/pane sizes, sidebar state, tray behaviour, cache size | stays on the device |
| Org config | Allowed providers, OAuth app IDs, preset servers, policies, branding | org data, encrypted |
| Org secrets | AI provider keys, any OAuth client secrets | org data, encrypted, never sent to clients |

**Encryption model: envelope encryption under admin-managed keys.**
- An **org master key** is created when the org/deployment is set up. On-prem, it's generated at
  install and kept where the customer's admin chooses: a key file/secret outside the database by
  default, with an optional KMS/HSM integration. Hosted: one master key per org, in my KMS.
- Each user gets a random **data key**, wrapped by the org master key. Every user data section is
  encrypted with it (AES-256-GCM, fresh nonce, section name + version as associated data). Org
  config and org secrets use their own data key under the master key.
- **Nobody sees user data:** no admin-console screen or API ever returns a user's settings,
  credentials, mailbox list or content. The server decrypts user data only inside defined service
  paths:
  - sending it over TLS to that user's own signed-in devices;
  - the push worker;
  - the AI proxy.

  Each of those paths is audited. Admins can manage users (invite, disable, reset password/2FA,
  sign out devices, see seat counts) but never read their data.
- **Key operations:** rotate the master key (re-wrap data keys without re-encrypting the data),
  back up the key separately from the database, and document clearly that losing the master key
  and its backup loses all user settings and credentials. Mail itself stays safe at the providers.
- **Devices** cache their user's data locally, protected by the OS keychain/keystore.
- **Not stored on the server:** mail messages, attachments and provider calendar events. Devices
  fetch them directly from the providers. The push worker only checks for new mail and events; it
  doesn't keep content.

**Sync:** versioned per section, optimistic concurrency (`If-Match`), field-level merge
(last-write-wins with timestamp + device ID for scalars, set-union with 30-day tombstones for
lists). Change notices over WebSocket/SSE (section + version only), with polling as fallback.
Provider token refreshes must not conflict with settings edits. Produce the full **settings
registry**: key, type, default, scope (user / device / org), and where it lives today (SQLite key,
`localStorage` key, keyring entry).

**Push (enabled by this model):** a server-side worker uses each user's encrypted provider
credentials to watch for new mail (IMAP IDLE, Graph/Gmail change notifications), snooze wake-ups,
send-later times and calendar reminders, and notifies devices through APNs/FCM (desktop: the open
connection). Notification payloads carry no message content unless a per-org policy allows a
preview. Snooze and send-later move from "this computer only" to the server, so they work with every
device off.

## 5. AI through the server (decided)
- Clients call the Remail server's AI endpoints (summarize, reply, rewrite, assistant chat with
  tools). The server forwards to the org's configured provider (OpenAI-compatible or
  Anthropic-compatible, or a local model on-prem), using the key held encrypted in org secrets.
- Keys never leave the server, and clients never see the provider URL or key.
- The server keeps no prompt or response bodies. Logs hold only metadata (user, feature, tokens,
  timing) for usage limits and billing.
- Org policies: AI on/off, which features, per-user/month limits, model choice.
- The assistant's mailbox tools keep running on the device (the email content is there). Only the
  model call goes through the server. Keep the existing "ask before sending/moving" approvals.

## 6. Roles and the admin console
- **Roles:** `owner` (me, across the hosted service), `org admin` (the customer's IT), `user`.
- Regular users never see any admin UI. Remove Settings → Admin from the user app.
- The **admin console** is a web app served by the Remail server, in English and Arabic:
  - **Organisation:** name, logo/branding, allowed email domains, sign-up mode (open, invite-only,
    SSO later).
  - **Email providers:** turn Google, Microsoft and Other on or off; OAuth app IDs and tenants;
    preset IMAP/SMTP/CalDAV servers.
  - **AI:** provider, model, key (write-only: it can be replaced, never shown), policies and usage.
  - **Users:** invite, disable, reset password/2FA, see devices, sign out devices, seat usage. No
    access to user data.
  - **Policies:** require 2FA, session lifetime, allowed platforms, notification previews, cache
    retention on devices.
  - **Security:** master key status, rotation and backup reminders.
  - **Licence:** status, expiry, seats used/allowed, upload a renewed licence (on-prem).
  - **Audit log** of admin actions and of every decryption path.

## 7. On-prem deployment and licensing
- **Package:** one Docker Compose bundle (server + admin console + push worker + Postgres +
  optional reverse proxy/TLS). Document install, master-key setup and backup, upgrade, and
  database backup/restore. It must run fully offline (air-gapped) except for the email and AI
  providers it connects to. Push through APNs/FCM needs outbound internet; document that and a
  no-push mode.
- **Clients for on-prem:** the same store apps, pointed at the customer's server (address or org
  code), plus optional branded builds and MDM-friendly configuration.
- **Licence:** a licence file signed by me (Ed25519). It contains the customer, deployment ID,
  seats, enabled features (AI, MCP, calendar, push…), start and expiry (yearly).
  - The server checks it **offline** with an embedded public key; optional online renewal check.
  - Warnings 30 days before expiry, then a 14-day grace period, then no new sign-ins or devices
    (existing data stays usable and exportable).
  - Seat counting uses active users. Over the limit, block new users, not existing ones.
  - Tamper resistance: signature, clock-rollback detection, licence bound to the deployment ID.
- **Licence generator tool**, for me only: issue, renew and revoke licences, and record customers.
- The hosted service runs the same server multi-tenant. On-prem is single-tenant. One codebase with
  a deployment-mode switch.

## 8. Security and hardening
- 2FA: TOTP + backup codes (hashed), with a per-org policy to require it. Passkeys later.
- Rate limiting and progressive lockout on sign-in, 2FA, reset and invite endpoints.
- Short-lived access token + rotating refresh token, kept in the OS keychain/keystore on every
  platform.
- No secrets or email content in logs, analytics, crash reports or MCP tool output.
- Use vetted libraries only (`argon2` for password hashing, `ring`/`aes-gcm`, `ed25519-dalek`). No
  custom crypto.
- **Threat model:** what a database-only leak, a full server compromise (database + master key), a
  malicious org admin, and a stolen unlocked phone or laptop each expose.

## 9. Server
Rust + `axum` (already used in `remail-core`) + Postgres, with the admin console in Svelte so it
shares UI pieces with the app. Propose the crate layout (`remail-server`, a shared `remail-proto`
for API types) and ask only if something blocks this. Clients handle the server being unreachable:
cached settings, mail still syncing directly from the providers, AI unavailable until the server
is back.

## Deliverables (phased; get the plan approved first)
1. **Phased plan** with milestones:
   - accounts + sign-in gate;
   - server-stored settings;
   - admin console;
   - AI proxy;
   - push worker;
   - mobile;
   - on-prem bundle + licensing.
2. **Architecture and sequence diagrams:** sign-up, sign-in on a new phone, connect
   Google/Microsoft/IMAP, admin changes a provider setting, master-key rotation, an AI request, a
   push notification, licence expiry, on-prem install.
3. **Server:** DB schema (orgs, users, devices, sessions, encrypted user sections, org config, org
   secrets, wrapped keys, licences, audit log) and API (request/response shapes).
4. **Client changes:** a sign-in gate, moving settings from SQLite/`localStorage` to server sync,
   provider list from the server, AI calls to the server, Admin removed from user Settings, and
   plain phone screens.
5. **Admin console** screens (functional; visual polish comes with the design plan).
6. **On-prem bundle,** master-key tooling, the licence format + verifier + generator, and
   install/upgrade/backup docs.
7. **Tests:**
   - Envelope encryption round trips, key rotation, and a test that no admin endpoint returns user
     data.
   - Licence verification, with tampered and expired licences.
   - Sync conflicts: two devices offline, an account removed while a device is offline, a token
     refresh racing a settings edit.
   - Provider allow-list enforced server-side; the AI proxy never exposes the key or stores bodies.
   - End-to-end against GreenMail + Radicale + the mock Microsoft identity platform
     (`REMAIL_MS_AUTHORITY`).

Check with `cargo test`, `cargo clippy`, `pnpm check` and `pnpm test`. For desktop builds during
development, build only the local `.deb`.
