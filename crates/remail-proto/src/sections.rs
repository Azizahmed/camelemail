//! The sections a user's setup is split into, and the record field names each
//! one uses. The engine maps these to its local tables; the server only checks
//! the parts it must enforce (allow-list, sizes).

use crate::org::{ProviderKind, Providers};
use crate::sync::SectionDoc;

pub const PREFS: &str = "prefs";
pub const SHORTCUTS: &str = "shortcuts";
pub const ACCOUNTS: &str = "accounts";
pub const CALENDARS: &str = "calendars";
pub const SIGNATURES: &str = "signatures";
pub const TEMPLATES: &str = "templates";
pub const LAYOUTS: &str = "layouts";
pub const SENDER_RULES: &str = "sender_rules";
pub const SNOOZES: &str = "snoozes";
/// `cred:<acc_…|cal_…>`: one credential per section, so a token refresh never
/// races an edit of the account itself.
pub const CRED_PREFIX: &str = "cred:";

pub const FIXED: [&str; 9] = [PREFS, SHORTCUTS, ACCOUNTS, CALENDARS, SIGNATURES, LAYOUTS, TEMPLATES, SENDER_RULES, SNOOZES];

/// Largest single section (ciphertext is a little bigger).
pub const MAX_SECTION_BYTES: usize = 256 * 1024;
/// All of a user's sections together.
pub const MAX_USER_BYTES: usize = 2 * 1024 * 1024;

pub fn cred_section(owner_id: &str) -> String {
    format!("{CRED_PREFIX}{owner_id}")
}

/// Is `name` a section a client may read and write?
pub fn is_valid(name: &str) -> bool {
    if FIXED.contains(&name) {
        return true;
    }
    match name.strip_prefix(CRED_PREFIX) {
        Some(id) => {
            (id.starts_with("acc_") || id.starts_with("cal_"))
                && id.len() <= 64
                && id.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'_')
        }
        None => false,
    }
}

/// Fields of an `accounts` record.
pub mod account {
    pub const EMAIL: &str = "email";
    pub const DISPLAY_NAME: &str = "displayName";
    pub const USERNAME: &str = "username";
    /// `google` | `microsoft` | `imap`
    pub const PROVIDER: &str = "provider";
    /// `password` | `microsoft` | `google`
    pub const AUTH: &str = "auth";
    /// `{host, port, security}`
    pub const IMAP: &str = "imap";
    pub const SMTP: &str = "smtp";
    pub const COLOR: &str = "color";
    pub const SORT_ORDER: &str = "sortOrder";
    pub const HIDDEN_IN_UNIFIED: &str = "hiddenInUnified";
    pub const CREATED_AT: &str = "createdAt";
    /// Set fields.
    pub const AUTO_BCC: &str = "autoBcc";
    pub const HIDDEN_FOLDERS: &str = "hiddenFolders";
}

/// Fields of a `cred:*` section's singleton record.
pub mod cred {
    /// `password` | `oauth`
    pub const KIND: &str = "kind";
    pub const PASSWORD: &str = "password";
    /// The OAuth grant as JSON: `{provider, clientId, refreshToken, subject}`.
    pub const GRANT: &str = "grant";
}

/// Why the server refused a section write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Violation {
    ProviderNotAllowed { record: String, provider: String },
    ImapHostNotAllowed { record: String, host: String },
    Malformed(String),
}

/// Checks an `accounts` document against the org's allow-list. Only live
/// records are checked: removing a no-longer-allowed account is always fine.
/// `previous` lets accounts that were already there before a provider was
/// switched off keep syncing edits (disconnecting them is the admin's call,
/// not a side effect of a colour change).
pub fn check_accounts(doc: &SectionDoc, previous: Option<&SectionDoc>, providers: &Providers) -> Result<(), Violation> {
    for (id, rec) in doc.live() {
        // Same id, provider and server as before: grandfathered.
        let existed = previous.and_then(|p| p.record(id)).is_some_and(|old| {
            old.get(account::PROVIDER) == rec.get(account::PROVIDER) && old.get(account::IMAP) == rec.get(account::IMAP)
        });
        let provider = rec.get(account::PROVIDER).and_then(|v| v.as_str()).unwrap_or("");
        let kind = ProviderKind::parse(provider)
            .ok_or_else(|| Violation::Malformed(format!("account {id} has no valid provider")))?;
        if existed {
            continue;
        }
        if !providers.allows(kind) {
            return Err(Violation::ProviderNotAllowed { record: id.clone(), provider: provider.into() });
        }
        if kind == ProviderKind::Imap {
            let host = rec
                .get(account::IMAP)
                .and_then(|v| v.get("host"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if !providers.allows_imap_host(host) {
                return Err(Violation::ImapHostNotAllowed { record: id.clone(), host: host.into() });
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sync::Stamp;
    use serde_json::json;

    fn doc(provider: &str, host: &str) -> SectionDoc {
        let mut d = SectionDoc::default();
        let at = Stamp::new(1, "A");
        let r = d.record_mut("acc_1");
        r.set(account::PROVIDER, json!(provider), &at);
        r.set(account::IMAP, json!({"host": host, "port": 993, "security": "tls"}), &at);
        d
    }

    #[test]
    fn section_names() {
        assert!(is_valid("prefs"));
        assert!(is_valid("layouts"));
        assert!(is_valid("cred:acc_0123abcd"));
        assert!(is_valid("cred:cal_x"));
        assert!(!is_valid("cred:../etc"));
        assert!(!is_valid("cred:usr_1"));
        assert!(!is_valid("other"));
    }

    #[test]
    fn allow_list() {
        let mut p = Providers::default();
        assert!(check_accounts(&doc("google", ""), None, &p).is_ok());
        p.google = false;
        assert!(matches!(check_accounts(&doc("google", ""), None, &p), Err(Violation::ProviderNotAllowed { .. })));
        // An account that already existed may still be edited.
        assert!(check_accounts(&doc("google", ""), Some(&doc("google", "")), &p).is_ok());
        // …but not switched to a provider that's off.
        assert!(check_accounts(&doc("google", ""), Some(&doc("microsoft", "")), &p).is_err());
        p.manual_servers = false;
        assert!(matches!(check_accounts(&doc("imap", "mail.x.com"), None, &p), Err(Violation::ImapHostNotAllowed { .. })));
        p.presets.push(crate::org::ServerPreset { imap_host: "mail.x.com".into(), ..Default::default() });
        assert!(check_accounts(&doc("imap", "MAIL.x.com"), None, &p).is_ok());
        assert!(matches!(check_accounts(&doc("nope", ""), None, &p), Err(Violation::Malformed(_))));
    }
}
