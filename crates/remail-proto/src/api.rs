//! Request and response bodies of the `/v1` API (camelCase JSON).

use serde::{Deserialize, Serialize};

use crate::org::{OAuthApp, OrgConfig, Policies, ProviderKind, ServerPreset};
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
    pub password: String,
    pub device: DeviceInfo,
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
    pub password: String,
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
