//! Organisation configuration as clients see it. Secrets (AI key, OAuth
//! client secrets) live in [`OrgSecrets`] and never leave the server.

use serde::{Deserialize, Serialize};

/// The three kinds of mailbox a user can connect.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ProviderKind {
    /// Gmail and Google Workspace (OAuth).
    Google,
    /// Outlook.com, Hotmail, Microsoft 365 (OAuth).
    Microsoft,
    /// Any other IMAP/SMTP (+ CalDAV) server.
    Imap,
}

impl ProviderKind {
    pub fn as_str(self) -> &'static str {
        match self {
            ProviderKind::Google => "google",
            ProviderKind::Microsoft => "microsoft",
            ProviderKind::Imap => "imap",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "google" | "gmail" => Some(ProviderKind::Google),
            "microsoft" | "outlook" => Some(ProviderKind::Microsoft),
            "imap" | "other" | "password" => Some(ProviderKind::Imap),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OAuthApp {
    pub client_id: String,
    /// Microsoft: `common`, `organizations`, `consumers` or a tenant id.
    pub tenant: String,
    /// Google "Desktop app" clients: the client secret Google's token
    /// endpoint still asks for. It isn't confidential (Google ships it in
    /// installed apps), so devices get it and talk to Google themselves.
    #[serde(skip_serializing_if = "String::is_empty")]
    pub client_secret: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct ServerPreset {
    /// Shown to the user ("Acme mail").
    pub name: String,
    /// E-mail domain this preset applies to (`acme.com`); empty = any.
    pub domain: String,
    pub imap_host: String,
    pub imap_port: u16,
    /// `tls` | `starttls` | `none`
    pub imap_security: String,
    pub smtp_host: String,
    pub smtp_port: u16,
    pub smtp_security: String,
    /// CalDAV base URL; empty = no calendar.
    pub caldav_url: String,
}

impl Default for ServerPreset {
    fn default() -> Self {
        ServerPreset {
            name: String::new(),
            domain: String::new(),
            imap_host: String::new(),
            imap_port: 993,
            imap_security: "tls".into(),
            smtp_host: String::new(),
            smtp_port: 465,
            smtp_security: "tls".into(),
            caldav_url: String::new(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Providers {
    pub google: bool,
    pub microsoft: bool,
    /// Other IMAP/SMTP servers.
    pub imap: bool,
    /// With `imap` on: may users type any server (auto-detect, manual
    /// hosts), or only pick a preset?
    pub manual_servers: bool,
    pub google_app: OAuthApp,
    pub microsoft_app: OAuthApp,
    pub presets: Vec<ServerPreset>,
}

impl Default for Providers {
    fn default() -> Self {
        Providers {
            google: true,
            microsoft: true,
            imap: true,
            manual_servers: true,
            google_app: OAuthApp::default(),
            microsoft_app: OAuthApp { tenant: "common".into(), ..Default::default() },
            presets: Vec::new(),
        }
    }
}

impl Providers {
    pub fn allows(&self, kind: ProviderKind) -> bool {
        match kind {
            ProviderKind::Google => self.google,
            ProviderKind::Microsoft => self.microsoft,
            ProviderKind::Imap => self.imap,
        }
    }

    /// May an IMAP account use this host? Presets are always fine; anything
    /// else needs `manual_servers`.
    pub fn allows_imap_host(&self, host: &str) -> bool {
        if !self.imap {
            return false;
        }
        self.manual_servers || self.presets.iter().any(|p| p.imap_host.eq_ignore_ascii_case(host))
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiPolicy {
    pub enabled: bool,
    /// summarize, reply, compose, rewrite, chat, sort
    pub features: Vec<String>,
    pub monthly_requests_per_user: u32,
    pub monthly_tokens_per_user: u64,
}

impl Default for AiPolicy {
    fn default() -> Self {
        AiPolicy {
            enabled: false,
            features: ["summarize", "reply", "compose", "rewrite", "chat", "sort"].map(String::from).to_vec(),
            monthly_requests_per_user: 0,
            monthly_tokens_per_user: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Policies {
    pub require_2fa: bool,
    /// Refresh tokens live at most this long without use.
    pub session_days: u32,
    /// linux, windows, macos, ios, android; empty = all.
    pub platforms: Vec<String>,
    /// May push notifications show sender and subject?
    pub notification_preview: bool,
    /// Days a device may keep its cache without reaching the server; 0 = no limit.
    pub device_cache_days: u32,
    pub ai: AiPolicy,
    pub mcp: bool,
}

impl Default for Policies {
    fn default() -> Self {
        Policies {
            require_2fa: false,
            session_days: 30,
            platforms: Vec::new(),
            notification_preview: false,
            device_cache_days: 0,
            ai: AiPolicy::default(),
            mcp: true,
        }
    }
}

impl Policies {
    pub fn allows_platform(&self, platform: &str) -> bool {
        self.platforms.is_empty() || self.platforms.iter().any(|p| p == platform)
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Branding {
    /// `data:image/png;base64,…`, at most 256 KiB.
    pub logo: String,
    pub accent: String,
}

/// What every signed-in client of the org receives (section `org`).
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OrgConfig {
    pub name: String,
    pub providers: Providers,
    pub policies: Policies,
    pub branding: Branding,
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AiSecret {
    /// `openai` | `anthropic`
    pub provider: String,
    pub base_url: String,
    pub api_key: String,
    pub models: Vec<String>,
    pub default_model: String,
    pub max_tokens: u32,
}

/// Server-only. Never serialised into any response.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct OrgSecrets {
    pub ai: AiSecret,
    pub google_client_secret: String,
}
