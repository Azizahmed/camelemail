# Settings registry

Every piece of persisted setup in Remail today, where it lives, and where it goes once the server exists.
Mail content (messages, drafts, attachments, calendar events, contacts cache) is out of scope: it stays
on devices and at the providers.

**Scope:** `user` = synced through the server, encrypted with the user's data key. `device` = stays on
this device. `org` = set by the admin in the console, encrypted with the org data key, read-only for
clients. `org-secret` = never leaves the server.

**Section** is the server-side sync unit the value moves into (see [design.md §5](design.md#5-sync)).

Paths: `core/` = `crates/remail-core/src/`, `tauri/` = `src-tauri/src/`.

## 1. `settings` table, key `app` (`AppSettings`, `core/engine.rs:48`)

| Key | Type | Default | Scope | Section | Today |
|---|---|---|---|---|---|
| `undoSendSeconds` | u32, 0 = off | 5 | user | `prefs` | SQLite `settings.app` |
| `theme` | `system\|light\|dark` | `light` | user | `prefs` | 〃 |
| `loadRemoteImages` | bool | false | user | `prefs` | 〃 |
| `notifications` | bool | true | device | — | 〃 |
| `syncIntervalSeconds` | u64, min 5 | 15 | device | — | 〃 |
| `defaultReply` | `reply\|replyAll` | `reply` | user | `prefs` | 〃 |
| `markReadOnOpen` | bool | true | user | `prefs` | 〃 |
| `notifyOnSnoozeWake` | bool | true | user | `prefs` (used by the push worker) | 〃 |
| `snoozeLaterMinutes` | i64 | 120 | user | `prefs` | 〃 |
| `snoozeMorning` / `Afternoon` / `Evening` / `Tomorrow` / `DayAfter` / `Saturday` / `Monday` | `HH:MM` | 08:00 / 15:00 / 18:00 / 10:00 / 11:00 / 10:00 / 10:00 | user | `prefs` | 〃 |
| `confirmBeforeSend` | bool | false | user | `prefs` | 〃 |
| `closeToTray` | bool | false | device | — | 〃 (`tauri/lib.rs:402`) |
| `afterRemove` | `nextUnread\|next\|previous\|list` | `nextUnread` | user | `prefs` | 〃 |
| `layout` | `split\|list` | `split` | device | — | 〃 (forced single-pane on phones) |
| `smartInbox` | bool | true | user | `prefs` | 〃 |
| `notifyImportantOnly` | bool | false | user | `prefs` (used by the push worker) | 〃 |
| `autoClean.enabled` / `.action` | bool / `trash\|archive` | false / `trash` | user | `prefs` | 〃 |
| `autoClean.rules[]` `{kind, afterMinutes}` | list keyed by `kind` | `[]` | user | `prefs` (keyed set) | 〃 |
| `autoClean.rules[].since` | i64 ms, engine-managed | 0 | device | — (derived; each device restarts it) | 〃 |
| `calendar.weekStart` | 0–6 | 0 | user | `calendar_prefs` | 〃 |
| `calendar.weekNumbers` | bool | true | user | `calendar_prefs` | 〃 |
| `calendar.weather` | `Place?` | null | user | `calendar_prefs` | 〃 |
| `calendar.units` | `c\|f` | `c` | user | `calendar_prefs` | 〃 |
| `calendar.defaultCalendar` | calendar ref `(calAccountId, href)` | null | user | `calendar_prefs` | 〃 (today a local `calendar_…` UUID, which isn't portable) |
| `calendar.view` | `day\|week\|month\|quarter\|year` | `month` | device | — | 〃 |
| `microsoftClientId` | string | `""` | **org** | org config `providers.microsoft.clientId` | 〃 (removed from the client) |
| `providers.gmail/outlook/other` | bool | true | **org** | org config `providers.*.enabled` | 〃 (removed from the client) |
| **new** `language` | `en\|ar\|system` | `system` | user | `prefs` | — |

## 2. `settings` key `ai` (`AiSettings`, `core/ai.rs:24`)

| Key | Type | Default | Scope | Destination |
|---|---|---|---|---|
| `enabled` | bool | false | user (and the org policy must allow it) | `prefs.ai.enabled` |
| `instructions` | string | `""` | user | `prefs.ai.instructions` |
| `smartSort` | bool | false | user | `prefs.ai.smartSort` |
| `provider` | `openai\|anthropic` | `openai` | org-secret | org secrets `ai.provider` |
| `baseUrl` | URL | OpenAI | org-secret | org secrets `ai.baseUrl` |
| `model` | string | `""` | org (the policy may let users pick from an allowed list) | org config `ai.models[]`; the user's choice goes in `prefs.ai.model` |
| `maxTokens` | u32 | 4096 | org | org config `ai.maxTokens` |
| keyring `ai-api-key` | secret | — | org-secret | org secrets `ai.apiKey` (deleted from devices on migration) |

## 3. `settings` key `mcp` (`McpSettings`, `core/engine.rs:349`)

MCP is desktop-only and machine-specific, so it stays on the device. The org can switch the feature off (licence feature `mcp` plus policy `mcp.enabled`).

| Key | Scope | Notes |
|---|---|---|
| `serverEnabled`, `port` | device | — |
| `permissions.{drafts,send,organize}` | device, capped by the org policy | — |
| `servers[]` (stdio) | device | Local executable paths |
| `servers[]` (http) | device | Could move to `user` later. Not in this plan. |
| keyring `mcp-server-token`, `mcp:<id>:env:*`, `mcp:<id>:header:*` | device | — |

## 4. Other `settings` keys

| Key | Scope | Destination |
|---|---|---|
| `cal_outlook_off` (`Vec<accountId>`) | user | `calendars.outlookOff` (set, with tombstones) |
| `fts_reindex_below` | device | Unchanged (an internal migration cursor) |

## 5. Tables

| Table / field | Scope | Section | Notes |
|---|---|---|---|
| `accounts`: `id, email, displayName, username, imap, smtp, color, sortOrder, hiddenInUnified, autoBcc, auth` | user | `accounts` (record per `acc_…`) | The server becomes the ID authority. `autoBcc` is a set. |
| `accounts.allowInvalidCerts` | device | — | Never synced |
| `accounts.isGmail`, `status`, `lastError` | device | — | Derived or runtime |
| *(new)* per-account `provider` | user | `accounts` | `google\|microsoft\|imap`. The server checks it against the allow-list. |
| *(new)* per-account folder prefs (hidden folders) | user | `accounts[].hiddenFolders` (set) | Taken from localStorage `sidebarHidden` |
| `signatures` (`sig_…`: account, name, html, isDefault) | user | `signatures` | — |
| `templates` (`tmpl_…`: name, subject, html) | user | `templates` | — |
| `sender_rules` (email → category) | user | `sender_rules` | Record per email |
| `autoclean_exempt` (account, thread) | device | — | Thread IDs are local |
| `snoozes` (account, thread, until) | user | `snoozes` | Record key = account + provider Message-ID of the newest message. Local thread IDs don't travel. |
| `outbox` kind `scheduled` | user | server table `scheduled_sends` (not a sync section) | The server sends it (§ push) |
| `outbox` kind `undo` | device | — | The undo window is local |
| `cal_accounts`: `id, kind, name, url, username, mailAccountId, createdAt` | user | `calendars.accounts` | — |
| `cal_accounts.allowInvalidCerts`, `status`, `lastSync`, `refreshSecs` | device | — | — |
| `calendars`: `color`, `enabled` overrides | user | `calendars.overrides` keyed by `(calAccountId, href)` | — |
| `calendars`: `href, name, readOnly, ctag, sortOrder`, local `id` | device | — | Re-discovered on each device |
| `drafts`, `messages`, `folders`, `folder_state`, `op_queue`, `contacts`, `cal_objects` | device | — | Content or cache |

## 6. Keyring entries (`SERVICE = "app.remail.desktop"`, `core/secrets.rs:11`)

| Entry | Scope | Destination |
|---|---|---|
| `<acc_id>` = IMAP password | user credential | Section `cred:<acc_id>`, plus the device keychain cache |
| `<acc_id>` = Microsoft `Grant` JSON | user credential | Section `cred:<acc_id>`, plus the device cache |
| `cal:<cal_id>` = CalDAV / feed password | user credential | Section `cred:<cal_id>`, plus the device cache |
| `ai-api-key` | org-secret | Removed from devices |
| *(new)* `remail-session` = `{server, refreshToken, deviceId}` | device | Keychain / Keystore only |
| *(new)* `remail-cache-key` = 32-byte key for the local section cache | device | Keychain / Keystore only |
| `mcp-server-token`, `mcp:*` | device | Unchanged |

Access tokens (`oauth_tokens`, in memory) stay in memory.

## 7. `localStorage`

| Key | Shape | Scope | Destination |
|---|---|---|---|
| `shortcuts` | `Record<actionId, string[]>` | user | Section `shortcuts` (field per action) |
| `sidebarHidden` | `string[]` (`SPAM`, `folder:<acc>:<label>`) | user | `prefs.sidebarHidden` (set). Folder entries go to `accounts[].hiddenFolders`. |
| `lastComposeAccount` | `acc_…` | user | `prefs.lastComposeAccount` |
| `sidebarMore` | `"1"\|"0"` | device | Unchanged |
| `sidebarDocked` | `"0"` = undocked | device | Unchanged |
| `sidebarWidth` | px | device | Unchanged |
| `listWidth` | px | device | Unchanged |

## 8. Environment and build-time configuration

| Name | Kind | After the platform work |
|---|---|---|
| `REMAIL_DATA_DIR` | runtime | Unchanged (profiles, tests) |
| `REMAIL_MS_CLIENT_ID` | build-time | Removed. The org config supplies it. |
| `REMAIL_MS_AUTHORITY`, `REMAIL_MS_GRAPH` | debug-only runtime | Unchanged. Also accepted by the server in debug builds and tests. |
| *(new)* `REMAIL_SERVER` | build-time default plus runtime override | The default server address. Branded or MDM builds set it. |
| *(new)* `REMAIL_ORG_DIRECTORY` | build-time | Where org codes resolve (the hosted directory) |
| *(new)* server `REMAIL_MODE` | runtime | `hosted\|onprem` |
| *(new)* server `REMAIL_MASTER_KEY_FILE` / `REMAIL_KMS_*` | runtime | Master-key provider |
| *(new)* server `DATABASE_URL`, `REMAIL_PUBLIC_URL`, `REMAIL_SMTP_*`, `REMAIL_APNS_*`, `REMAIL_FCM_*` | runtime | — |
