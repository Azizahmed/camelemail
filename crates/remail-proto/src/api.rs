//! Request and response bodies of the `/v1` API (camelCase JSON).

use serde::{Deserialize, Serialize};

use crate::org::{OAuthApp, OrgAi, OrgConfig, Policies, ProviderKind, Providers, ServerPreset};
use crate::sync::SectionDoc;

// ---------- meta ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Meta {
    /// `hosted` | `onprem`
    pub mode: String,
    pub version: String,
    /// Oldest client version the server still talks to.
    pub min_client: String,
    #[serde(default)]
    pub org_name: Option<String>,
    /// `open` | `invite`
    pub signup: String,
    /// Hosted only: e-mail must be verified before signing in.
    pub verify_email: bool,
    pub licence: LicenceState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum LicenceState {
    /// Hosted service: no licence file.
    None,
    Valid,
    Warning,
    Grace,
    Expired,
    Invalid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgCodeResolved {
    pub server: String,
}

// ---------- auth ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub name: String,
    /// linux | windows | macos | ios | android
    pub platform: String,
    #[serde(default)]
    pub app_version: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignupRequest {
    pub name: String,
    pub email: String,
    pub password: String,
    /// Invite token (on-prem, invite-only orgs).
    #[serde(default)]
    pub invite: Option<String>,
    /// `en` | `ar`, for the e-mails the server sends.
    #[serde(default)]
    pub locale: Option<String>,
    /// From apps that keep mailbox sign-ins end-to-end encrypted: `password`
    /// is then a proof ([`is_proof`]), never the password itself.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<VaultSetup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SignupResponse {
    /// `email`: check your inbox; `none`: sign in now.
    pub verify: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TokenRequest {
    pub token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoginRequest {
    pub email: String,
    /// The proof, for an account with a `kdf`; the password for one without.
    pub password: String,
    pub device: DeviceInfo,
    /// Converts an account without a `kdf`: `password` is then the password
    /// (checked once more), and the account keeps this proof and vault instead.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upgrade: Option<VaultUpgrade>,
}

/// How a password becomes keys on the device: Argon2id with these costs and
/// salt gives the master key, from which come the proof and the vault's
/// wrapping key (docs/platform/zero-knowledge.md).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Kdf {
    /// `argon2id`
    pub alg: String,
    pub m_kib: u32,
    pub t: u32,
    pub p: u32,
    /// base64
    pub salt: String,
}

impl Kdf {
    /// Costs a server accepts (and a device is willing to run).
    pub fn is_reasonable(&self) -> bool {
        self.alg == "argon2id"
            && (19_456..=1_048_576).contains(&self.m_kib)
            && (1..=10).contains(&self.t)
            && (1..=4).contains(&self.p)
            && (16..=88).contains(&self.salt.len())
    }
}

/// The account's vault key, wrapped with a key only the password gives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultSetup {
    pub kdf: Kdf,
    /// base64(nonce ‖ AES-256-GCM(vault KEK, vault key))
    pub key_enc: String,
    /// Changes only when the vault key does (a password reset).
    pub key_id: String,
}

impl VaultSetup {
    pub fn is_reasonable(&self) -> bool {
        self.kdf.is_reasonable()
            && (40..=200).contains(&self.key_enc.len())
            && (8..=64).contains(&self.key_id.len())
            && self.key_id.chars().all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultUpgrade {
    pub proof: String,
    pub vault: VaultSetup,
}

/// What a password proof looks like: `v1.` and 32 bytes, base64url.
pub const PROOF_PREFIX: &str = "v1.";

pub fn is_proof(s: &str) -> bool {
    s.strip_prefix(PROOF_PREFIX).is_some_and(|b| b.len() == 43 && b.bytes().all(|c| c.is_ascii_alphanumeric() || c == b'-' || c == b'_'))
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreloginRequest {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PreloginResponse {
    /// None: an account that still signs in with its password.
    pub kdf: Option<Kdf>,
}

/// `POST /v1/me/vault`: convert the signed-in account (it has no `kdf` yet).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VaultConvert {
    pub password: String,
    pub upgrade: VaultUpgrade,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TwoFactorRequest {
    pub challenge: String,
    /// TOTP code or a backup code.
    pub code: String,
}

/// `login` answers with tokens, or with a 2FA challenge.
// Built once per sign-in; boxing would only complicate callers.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(untagged)]
pub enum LoginResponse {
    Tokens(Tokens),
    Challenge(Challenge),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Challenge {
    pub challenge: String,
    pub methods: Vec<String>,
    /// The org requires 2FA and this user hasn't set it up: the challenge
    /// is for `2fa/setup` + `2fa/enable` instead of a code.
    #[serde(default)]
    pub setup_required: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Tokens {
    pub access_token: String,
    /// ms since epoch
    pub access_expires_at: i64,
    pub refresh_token: String,
    pub device_id: String,
    pub user: Me,
    /// On sign-in only: the account's wrapped vault key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<VaultSetup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RefreshRequest {
    pub refresh_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetRequest {
    pub email: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResetPassword {
    pub token: String,
    /// The new password's proof when `vault` is set (a new vault key).
    pub password: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<VaultSetup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Me {
    pub id: String,
    pub name: String,
    pub email: String,
    /// owner | admin | user
    pub role: String,
    pub org_id: String,
    pub org_name: String,
    pub totp: bool,
    pub policies: Policies,
    /// Set once the account keeps mailbox sign-ins end-to-end encrypted: the
    /// id of the vault key they're sealed with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault_key_id: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UpdateMe {
    #[serde(default)]
    pub name: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ChangePassword {
    pub current: String,
    pub new: String,
    /// The same vault key, wrapped for the new password (`new` is then its proof).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vault: Option<VaultSetup>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TotpSetup {
    /// Base32, for manual entry.
    pub secret: String,
    pub otpauth_url: String,
}

/// `POST /v1/me/2fa/setup` body: empty when signed in; with the sign-in
/// challenge when the org requires 2FA and the user has none yet.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TotpSetupRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub challenge: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TotpEnable {
    pub code: String,
    /// Set when enabling during a required-2FA sign-in.
    #[serde(default)]
    pub challenge: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct BackupCodes {
    pub backup_codes: Vec<String>,
    /// Present when enabling finished a sign-in.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<Tokens>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TotpDisable {
    pub password: String,
    pub code: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Device {
    pub id: String,
    pub name: String,
    pub platform: String,
    pub app_version: String,
    pub created_at: i64,
    pub last_seen_at: Option<i64>,
    pub current: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PushRegistration {
    /// apns | fcm | none
    pub kind: String,
    #[serde(default)]
    pub token: String,
}

// ---------- sections ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SectionInfo {
    pub id: String,
    pub version: i64,
    pub updated_at: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SectionIndex {
    pub sections: Vec<SectionInfo>,
    /// Server clock, ms (devices keep their stamps at or after it).
    pub server_time: i64,
}

/// GET answer, and the body of a `412` (the current document).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct Section {
    pub id: String,
    /// 0 = never written.
    pub version: i64,
    pub doc: SectionDoc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutSection {
    pub doc: SectionDoc,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PutSectionResponse {
    pub version: i64,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VersionMismatch {
    pub error: crate::ApiError,
    pub current: Section,
}

/// Over the notices WebSocket: which section changed (never its content).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "type")]
pub enum Notice {
    /// First message after connecting.
    Hello { server_time: i64 },
    Section { section: String, version: i64 },
    /// The org config changed (`GET /v1/org/config`).
    Org { version: i64 },
    /// This device was signed out.
    SignedOut,
}

/// First client message on the notices socket.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct NoticeAuth {
    pub access_token: String,
}

// ---------- org / providers ----------

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgConfigResponse {
    pub version: i64,
    pub config: OrgConfig,
}

/// One entry of "Add mailbox".
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderOption {
    pub kind: ProviderKind,
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oauth: Option<OAuthApp>,
    /// `imap`: may the user type any server?
    #[serde(default)]
    pub manual_servers: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub presets: Vec<ServerPreset>,
    /// Google: the server holds a client secret and does the code exchange.
    #[serde(default)]
    pub server_exchange: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleExchange {
    pub code: String,
    pub code_verifier: String,
    pub redirect_uri: String,
}

/// `POST /v1/providers/google/refresh`: a new Google access token from a
/// refresh token, with the org's client secret (only when the server did
/// the code exchange: a "Web" client can't refresh without its secret).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleRefresh {
    pub refresh_token: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoogleTokens {
    pub access_token: String,
    pub expires_in: i64,
    #[serde(default)]
    pub refresh_token: Option<String>,
    #[serde(default)]
    pub id_token: Option<String>,
}

pub fn providers_for(config: &OrgConfig, google_secret_set: bool) -> Vec<ProviderOption> {
    let p = &config.providers;
    vec![
        ProviderOption {
            kind: ProviderKind::Google,
            enabled: p.google && !p.google_app.client_id.is_empty(),
            oauth: Some(p.google_app.clone()),
            manual_servers: false,
            presets: Vec::new(),
            server_exchange: google_secret_set,
        },
        ProviderOption {
            kind: ProviderKind::Microsoft,
            enabled: p.microsoft && !p.microsoft_app.client_id.is_empty(),
            oauth: Some(p.microsoft_app.clone()),
            manual_servers: false,
            presets: Vec::new(),
            server_exchange: false,
        },
        ProviderOption {
            kind: ProviderKind::Imap,
            enabled: p.imap,
            oauth: None,
            manual_servers: p.manual_servers,
            presets: p.presets.clone(),
            server_exchange: false,
        },
    ]
}

// ---------- org AI key ----------

/// `GET /v1/org/ai-key`: the organisation's AI key, for a member's device
/// while org AI is on (admins may read it while it's off, to test the
/// setup). Devices call the AI provider themselves.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OrgAiKey {
    pub api_key: String,
}

impl std::fmt::Debug for OrgAiKey {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OrgAiKey([redacted])")
    }
}

// ---------- admin (owner or admin of the org) ----------

/// `GET /v1/admin/config`, and the answer to the admin `PUT`s. Secrets are
/// write-only: only whether they're set.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminConfig {
    /// Org config version (devices pick up each change).
    pub version: i64,
    /// `googleApp.clientSecret` is always empty here: see `googleSecretSet`.
    pub providers: Providers,
    pub ai: OrgAi,
    /// The Google app's client secret is set.
    pub google_secret_set: bool,
    /// The AI key is set.
    pub ai_key_set: bool,
}

/// `PUT /v1/admin/providers`. `providers.googleApp.clientSecret` is ignored;
/// `googleClientSecret` null keeps the saved one, `""` clears it, anything
/// else replaces it.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminProvidersUpdate {
    pub providers: Providers,
    #[serde(default)]
    pub google_client_secret: Option<String>,
}

impl std::fmt::Debug for AdminProvidersUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminProvidersUpdate").finish_non_exhaustive()
    }
}

/// `PUT /v1/admin/ai`. `apiKey` null keeps the saved key, `""` clears it,
/// anything else replaces it.
#[derive(Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminAiUpdate {
    pub ai: OrgAi,
    #[serde(default)]
    pub api_key: Option<String>,
}

impl std::fmt::Debug for AdminAiUpdate {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("AdminAiUpdate").field("ai", &self.ai).finish_non_exhaustive()
    }
}

/// `GET /v1/admin/users`: account metadata only (never settings, mailboxes
/// or mail).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AdminUser {
    pub id: String,
    pub name: String,
    pub email: String,
    /// owner | admin | user
    pub role: String,
    /// ms since epoch
    pub created_at: i64,
    pub last_active_at: Option<i64>,
    pub verified: bool,
    pub disabled: bool,
    pub totp: bool,
}

/// `PUT /v1/admin/users/{id}/role`: `admin` or `user`. Owners are set on the
/// server (`REMAIL_ADMIN_EMAILS`), never through the API.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RoleUpdate {
    pub role: String,
}
