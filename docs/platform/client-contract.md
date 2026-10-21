# Client contract (M1–M2)

This is the boundary between the Rust side (`remail-core::cloud` + `src-tauri` commands) and the
Svelte UI.
- The HTTP API between the client and the server is [design.md §7](design.md#7-api-v1). Its exact
  shapes are `crates/remail-proto/src/api.rs`.
- Server access happens **only in Rust**. The webview never sees access or refresh tokens, and the
  CSP stays as is.

All Tauri command args and results are camelCase. Errors are rejected with a string. Server errors
are encoded as `"cloud:<code>:<message>"`, where `<code>` is an `ErrorCode` wire name such as
`invalid_credentials` or `weak_password`. Network failures use `cloud:unavailable:…`. The UI shows
`t("err.<code>")`.

## Types

```ts
type CloudStatus = {
  server: string | null;          // base URL in use, null = not chosen yet
  defaultServer: string | null;   // build-time default (REMAIL_SERVER), if any
  signedIn: boolean;
  me: Me | null;                  // proto api::Me
  // signed in, but this device still has to bring setup down / offer migration
  phase: "signedOut" | "migrate" | "syncing" | "ready";
  offline: boolean;               // server unreachable (cached setup in use)
  progress: { done: number; total: number; label: string } | null; // first sync
};
type LoginResult = { status: "ok"; me: Me } | { status: "2fa"; challenge: string; setupRequired: boolean };
type ProviderOption = { kind: "google" | "microsoft" | "imap"; enabled: boolean; oauth?: { clientId: string; tenant: string };
                        manualServers: boolean; presets: ServerPreset[]; serverExchange: boolean };
type MigrationInfo = { localAccounts: number; serverAccounts: number };
type UiPrefs = { shortcuts: Record<string, string[]>; sidebarHidden: string[] | null; lastComposeAccount: string | null };
```

## Commands

| Command | Args | Result |
|---|---|---|
| `cloud_status` | — | `CloudStatus` |
| `cloud_set_server` | `{ address }` (a URL, a bare host, or an org code) | `Meta` (proto), and the server is saved |
| `cloud_signup` | `{ name, email, password, invite?, locale }` | `{ verify: "email" \| "none" }` |
| `cloud_verify_email` | `{ token }` | — |
| `cloud_login` | `{ email, password }` | `LoginResult` |
| `cloud_login_2fa` | `{ challenge, code }` | `{ me }` |
| `cloud_logout` | — | — (wipes this device's synced setup and credentials, stops mail) |
| `cloud_request_reset` | `{ email }` | — |
| `cloud_reset_password` | `{ token, password }` | — |
| `cloud_me` | — | `Me` |
| `cloud_update_me` | `{ name }` | `Me` |
| `cloud_change_password` | `{ current, new }` | — |
| `cloud_totp_setup` | `{ challenge? }` | `{ secret, otpauthUrl }` |
| `cloud_totp_enable` | `{ code, challenge? }` | `{ backupCodes: string[] }` (with a challenge, it also completes the sign-in) |
| `cloud_totp_disable` | `{ password, code }` | — |
| `cloud_devices` | — | `Device[]` |
| `cloud_revoke_device` | `{ id }` | — |
| `cloud_providers` | — | `ProviderOption[]` (cached org config when offline) |
| `cloud_migration` | — | `MigrationInfo` |
| `cloud_migrate` | `{ upload: boolean }` | — (`upload: false` discards local setup and takes the server's) |
| `cloud_sync_now` | — | — |
| `ui_prefs` | — | `UiPrefs` |
| `set_ui_prefs` | `{ prefs: Partial<UiPrefs> }` | — |
| `add_google_account` | `{ displayName }` | `Account` (same shape as `add_microsoft_account`) |

Existing commands that change:
- `add_account` refuses providers the org disallows, with `cloud:provider_not_allowed`.
- `reauthorize_account` also handles Google.
- `get_settings` / `set_settings` no longer carry `providers` or `microsoftClientId`.
- `ai_*` is unchanged in M2. M4 changes it.
- `microsoft_sign_in_ready` reports whether the org config has a Microsoft app ID.

## Events

There is a new Tauri event, `cloud-event`, with payload
`{ kind: "status", status: CloudStatus } | { kind: "signedOut", reason: "revoked" | "expired" | "user" }`.
- `status` fires on every phase, progress or offline change.
- `signedOut` fires when the server revoked this device, or the refresh token died.

`engine-event` keeps its current shape. After a remote section is applied, the engine emits
`MailboxChanged` / `CalendarChanged` as it does today when local data changes. A new
`{ type: "settingsChanged" }` event fires when `prefs` or `shortcuts` changed from another device,
so open windows reload settings and shortcuts.

## Gate (App.svelte)

1. `phase == "signedOut"` → the auth screens (welcome, server or org code, create account, verify,
   sign in, 2FA, reset).
2. `"migrate"` → "Move this computer's setup to your Remail account?"
3. `"syncing"` → "Setting up your mailboxes", with `progress`.
4. `"ready"` with zero accounts → connect the first mailbox (the existing AccountSetup).
5. `"ready"` → the app.

Compose and settings windows render nothing until `ready`.
