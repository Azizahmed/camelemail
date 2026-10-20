//! IMAP transport: connecting (TLS / STARTTLS / plain), folder discovery with
//! special-use mapping, and the fetch/store/move/append primitives sync uses.

use std::collections::HashMap;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context as TaskContext, Poll};
use std::time::Duration;

use anyhow::{anyhow, bail, Context, Result};
use async_imap::types::{Fetch, Flag, NameAttribute};
use async_imap::{Client, Session};
use futures::TryStreamExt;
use tokio::io::{AsyncRead, AsyncWrite, ReadBuf};
use tokio::net::TcpStream;
use tokio_rustls::client::TlsStream;

use crate::model::{labels, Account, Login, Security, ServerConfig};

pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(30);
/// Per-command ceiling so a stuck server can't hang sync forever.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(120);
/// UID sets are chunked to keep command lines under this length.
const UID_SET_MAX_LEN: usize = 4000;

/// Returned for login failures so callers can mark the account for re-auth.
#[derive(Debug, thiserror::Error)]
#[error("authentication failed: {0}")]
pub struct AuthError(pub String);

#[derive(Debug)]
pub enum MaybeTls {
    Plain(TcpStream),
    Tls(Box<TlsStream<TcpStream>>),
}

impl AsyncRead for MaybeTls {
    fn poll_read(self: Pin<&mut Self>, cx: &mut TaskContext<'_>, buf: &mut ReadBuf<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_read(cx, buf),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_read(cx, buf),
        }
    }
}

impl AsyncWrite for MaybeTls {
    fn poll_write(self: Pin<&mut Self>, cx: &mut TaskContext<'_>, buf: &[u8]) -> Poll<std::io::Result<usize>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_write(cx, buf),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_write(cx, buf),
        }
    }
    fn poll_flush(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_flush(cx),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_flush(cx),
        }
    }
    fn poll_shutdown(self: Pin<&mut Self>, cx: &mut TaskContext<'_>) -> Poll<std::io::Result<()>> {
        match self.get_mut() {
            MaybeTls::Plain(s) => Pin::new(s).poll_shutdown(cx),
            MaybeTls::Tls(s) => Pin::new(s.as_mut()).poll_shutdown(cx),
        }
    }
}

// ----------------------------------------------------------------------------
// TLS
// ----------------------------------------------------------------------------

#[derive(Debug)]
struct AcceptAnyCert(Arc<rustls::crypto::CryptoProvider>);

impl rustls::client::danger::ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &rustls::pki_types::CertificateDer<'_>,
        _intermediates: &[rustls::pki_types::CertificateDer<'_>],
        _server_name: &rustls::pki_types::ServerName<'_>,
        _ocsp: &[u8],
        _now: rustls::pki_types::UnixTime,
    ) -> Result<rustls::client::danger::ServerCertVerified, rustls::Error> {
        Ok(rustls::client::danger::ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &rustls::pki_types::CertificateDer<'_>,
        dss: &rustls::DigitallySignedStruct,
    ) -> Result<rustls::client::danger::HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(message, cert, dss, &self.0.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<rustls::SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

fn root_store() -> rustls::RootCertStore {
    let mut roots = rustls::RootCertStore::empty();
    let native = rustls_native_certs::load_native_certs();
    for cert in native.certs {
        let _ = roots.add(cert);
    }
    if roots.is_empty() {
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    }
    roots
}

pub fn tls_config(allow_invalid_certs: bool) -> Result<Arc<rustls::ClientConfig>> {
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone()).with_safe_default_protocol_versions()?;
    let config = if allow_invalid_certs {
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
            .with_no_client_auth()
    } else {
        builder.with_root_certificates(root_store()).with_no_client_auth()
    };
    Ok(Arc::new(config))
}

async fn tls_wrap(tcp: TcpStream, host: &str, allow_invalid: bool) -> Result<MaybeTls> {
    let connector = tokio_rustls::TlsConnector::from(tls_config(allow_invalid)?);
    let name = rustls::pki_types::ServerName::try_from(host.to_string()).context("invalid server name")?;
    let tls = tokio::time::timeout(CONNECT_TIMEOUT, connector.connect(name, tcp))
        .await
        .context("TLS handshake timed out")?
        .context("TLS handshake failed")?;
    Ok(MaybeTls::Tls(Box::new(tls)))
}

pub async fn open_stream(server: &ServerConfig, allow_invalid: bool) -> Result<Client<MaybeTls>> {
    let addr = (server.host.as_str(), server.port);
    let tcp = tokio::time::timeout(CONNECT_TIMEOUT, TcpStream::connect(addr))
        .await
        .with_context(|| format!("connecting to {}:{} timed out", server.host, server.port))?
        .with_context(|| format!("could not connect to {}:{}", server.host, server.port))?;
    tcp.set_nodelay(true).ok();

    match server.security {
        Security::Tls => {
            let stream = tls_wrap(tcp, &server.host, allow_invalid).await?;
            let mut client = Client::new(stream);
            read_greeting(&mut client).await?;
            Ok(client)
        }
        Security::None => {
            let mut client = Client::new(MaybeTls::Plain(tcp));
            read_greeting(&mut client).await?;
            Ok(client)
        }
        Security::Starttls => {
            let mut client = Client::new(MaybeTls::Plain(tcp));
            read_greeting(&mut client).await?;
            client.run_command_and_check_ok("STARTTLS", None).await.context("STARTTLS rejected")?;
            let MaybeTls::Plain(tcp) = client.into_inner() else { bail!("unexpected stream state") };
            Ok(Client::new(tls_wrap(tcp, &server.host, allow_invalid).await?))
        }
    }
}

async fn read_greeting(client: &mut Client<MaybeTls>) -> Result<()> {
    tokio::time::timeout(CONNECT_TIMEOUT, client.read_response())
        .await
        .context("server greeting timed out")??
        .ok_or_else(|| anyhow!("connection closed before greeting"))?;
    Ok(())
}

// ----------------------------------------------------------------------------
// Folder discovery
// ----------------------------------------------------------------------------

#[derive(Debug, Clone)]
pub struct RemoteFolder {
    /// Raw (modified UTF-7) mailbox name, used in commands.
    pub path: String,
    /// Decoded, human-readable name.
    pub display: String,
    pub delimiter: Option<String>,
    pub special_use: Option<&'static str>,
    pub selectable: bool,
}

/// Decode IMAP modified UTF-7 (RFC 3501 §5.1.3).
pub fn decode_utf7(s: &str) -> String {
    use base64::Engine;
    let mut out = String::new();
    let mut rest = s;
    while let Some(start) = rest.find('&') {
        out.push_str(&rest[..start]);
        let after = &rest[start + 1..];
        let Some(end) = after.find('-') else {
            out.push_str(&rest[start..]);
            return out;
        };
        let chunk = &after[..end];
        if chunk.is_empty() {
            out.push('&');
        } else {
            let b64 = chunk.replace(',', "/");
            let decoded = base64::engine::general_purpose::STANDARD_NO_PAD.decode(b64.as_bytes()).ok();
            match decoded {
                Some(bytes) if bytes.len() % 2 == 0 => {
                    let units: Vec<u16> = bytes.chunks(2).map(|c| u16::from_be_bytes([c[0], c[1]])).collect();
                    out.push_str(&String::from_utf16_lossy(&units));
                }
                _ => {
                    out.push('&');
                    out.push_str(chunk);
                    out.push('-');
                }
            }
        }
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    out
}

/// Encode a folder name to modified UTF-7 for CREATE.
pub fn encode_utf7(s: &str) -> String {
    use base64::Engine;
    let mut out = String::new();
    let mut pending: Vec<u16> = Vec::new();
    let flush = |pending: &mut Vec<u16>, out: &mut String| {
        if pending.is_empty() {
            return;
        }
        let bytes: Vec<u8> = pending.iter().flat_map(|u| u.to_be_bytes()).collect();
        let b64 = base64::engine::general_purpose::STANDARD_NO_PAD.encode(bytes).replace('/', ",");
        out.push('&');
        out.push_str(&b64);
        out.push('-');
        pending.clear();
    };
    for c in s.chars() {
        if (' '..='~').contains(&c) {
            flush(&mut pending, &mut out);
            if c == '&' {
                out.push_str("&-");
            } else {
                out.push(c);
            }
        } else {
            let mut buf = [0u16; 2];
            pending.extend_from_slice(c.encode_utf16(&mut buf));
        }
    }
    flush(&mut pending, &mut out);
    out
}

/// Well-known folder names → system label (fallback when the server has no
/// SPECIAL-USE). Matches the full lowercased path first, then the leaf.
fn label_from_name(name: &str) -> Option<&'static str> {
    let n = name.trim().to_lowercase();
    let lookup = |s: &str| -> Option<&'static str> {
        Some(match s {
            "inbox" => labels::INBOX,
            "sent" | "sent items" | "sent messages" | "sent mail" | "gesendet" | "gesendete elemente" | "envoyés"
            | "éléments envoyés" | "enviados" | "elementos enviados" | "posta inviata" | "inviati" | "verzonden"
            | "отправленные" | "المرسلة" | "البريد المرسل" => labels::SENT,
            "drafts" | "draft" | "entwürfe" | "brouillons" | "borradores" | "bozze" | "concepten" | "черновики"
            | "المسودات" => labels::DRAFTS,
            "trash" | "deleted items" | "deleted messages" | "bin" | "papierkorb" | "gelöschte elemente" | "corbeille"
            | "papelera" | "elementos eliminados" | "cestino" | "prullenbak" | "корзина" | "المحذوفات" => {
                labels::TRASH
            }
            "spam" | "junk" | "junk mail" | "junk e-mail" | "junk email" | "bulk mail" | "bulk" | "spamverdacht"
            | "courrier indésirable" | "correo no deseado" | "posta indesiderata" | "ongewenste e-mail" | "спам"
            | "البريد العشوائي" => labels::SPAM,
            "archive" | "archives" | "archiv" | "archivo" | "archivio" | "archief" | "архив" | "الأرشيف" => {
                labels::ARCHIVE
            }
            _ => return None,
        })
    };
    if let Some(l) = lookup(&n) {
        return Some(l);
    }
    let leaf = n.rsplit(['/', '.']).next().unwrap_or(&n);
    if leaf == "inbox" {
        return None;
    }
    lookup(leaf)
}

/// Pseudo-label for virtual folders we never sync (they duplicate others).
pub const SKIP_FLAGGED: &str = "\\Flagged";
pub const SKIP_IMPORTANT: &str = "\\Important";

/// Pick a label for each folder. Returns `(folder, label_id, kind_is_system)`.
/// Each system label is claimed by at most one folder: INBOX first, then
/// SPECIAL-USE folders, then name matches.
pub fn map_folders(folders: &[RemoteFolder]) -> Vec<(RemoteFolder, String, bool)> {
    let mut sorted: Vec<&RemoteFolder> = folders.iter().filter(|f| f.selectable).collect();
    sorted.sort_by_key(|f| {
        let rank = if f.path.eq_ignore_ascii_case("INBOX") {
            0
        } else if f.special_use.is_some() {
            1
        } else {
            2
        };
        (rank, f.path.clone())
    });
    let mut claimed = std::collections::HashSet::new();
    let mut out = Vec::new();
    for f in sorted {
        let candidate = if f.path.eq_ignore_ascii_case("INBOX") {
            Some(labels::INBOX)
        } else {
            match f.special_use {
                Some("\\Sent") => Some(labels::SENT),
                Some("\\Drafts") => Some(labels::DRAFTS),
                Some("\\Trash") => Some(labels::TRASH),
                Some("\\Junk") => Some(labels::SPAM),
                Some("\\Archive") => Some(labels::ARCHIVE),
                Some("\\All") => Some(labels::ALL),
                Some(_) => None,
                None => label_from_name(&f.display),
            }
        };
        match candidate {
            Some(l) if claimed.insert(l) => out.push((f.clone(), l.to_string(), true)),
            _ => out.push((f.clone(), f.path.clone(), false)),
        }
    }
    out
}

fn attr_special_use(attrs: &[NameAttribute<'_>]) -> (Option<&'static str>, bool) {
    let mut special = None;
    let mut selectable = true;
    for a in attrs {
        match a {
            NameAttribute::NoSelect => selectable = false,
            NameAttribute::All => special = Some("\\All"),
            NameAttribute::Archive => special = Some("\\Archive"),
            NameAttribute::Drafts => special = Some("\\Drafts"),
            NameAttribute::Flagged => special = Some("\\Flagged"),
            NameAttribute::Junk => special = Some("\\Junk"),
            NameAttribute::Sent => special = Some("\\Sent"),
            NameAttribute::Trash => special = Some("\\Trash"),
            NameAttribute::Extension(e) => {
                let e = e.to_ascii_lowercase();
                if e == "\\nonexistent" {
                    selectable = false;
                } else if e == "\\important" {
                    special = Some("\\Important");
                }
            }
            _ => {}
        }
    }
    (special, selectable)
}

// ----------------------------------------------------------------------------
// Connection
// ----------------------------------------------------------------------------

#[derive(Debug, Clone, Default)]
pub struct Caps {
    pub condstore: bool,
    pub uidplus: bool,
    pub r#move: bool,
    pub idle: bool,
    pub gmail: bool,
}

#[derive(Debug, Clone)]
pub struct MailboxStatus {
    pub uid_validity: u32,
    pub uid_next: u32,
    pub exists: u32,
    pub highest_modseq: Option<u64>,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct MsgFlags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub deleted: bool,
}

impl MsgFlags {
    fn from_fetch(f: &Fetch) -> Self {
        let mut out = MsgFlags::default();
        for flag in f.flags() {
            match flag {
                Flag::Seen => out.seen = true,
                Flag::Flagged => out.flagged = true,
                Flag::Answered => out.answered = true,
                Flag::Draft => out.draft = true,
                Flag::Deleted => out.deleted = true,
                _ => {}
            }
        }
        out
    }
}

#[derive(Debug, Clone)]
pub struct FetchedMessage {
    pub uid: u32,
    pub flags: MsgFlags,
    pub internal_date: Option<i64>,
    pub size: Option<u32>,
    /// Full message, or headers only for large messages.
    pub raw: Vec<u8>,
    pub complete: bool,
}

/// Messages larger than this are synced headers-only and fetched on open.
pub const FULL_FETCH_MAX_BYTES: u32 = 1_500_000;

pub struct ImapConn {
    session: Session<MaybeTls>,
    pub caps: Caps,
    selected: Option<String>,
}

pub fn uid_sets(uids: &[u32]) -> Vec<String> {
    let mut sorted = uids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut sets = Vec::new();
    let mut cur = String::new();
    let mut i = 0;
    while i < sorted.len() {
        let start = sorted[i];
        let mut end = start;
        while i + 1 < sorted.len() && sorted[i + 1] == end + 1 {
            i += 1;
            end = sorted[i];
        }
        let piece = if start == end { start.to_string() } else { format!("{start}:{end}") };
        if !cur.is_empty() && cur.len() + piece.len() + 1 > UID_SET_MAX_LEN {
            sets.push(std::mem::take(&mut cur));
        }
        if !cur.is_empty() {
            cur.push(',');
        }
        cur.push_str(&piece);
        i += 1;
    }
    if !cur.is_empty() {
        sets.push(cur);
    }
    sets
}

fn quote(s: &str) -> String {
    format!("\"{}\"", s.replace('\\', "\\\\").replace('"', "\\\""))
}

/// SASL XOAUTH2: the token first; if the server answers with an error
/// challenge, an empty reply ends the exchange so it can report the failure.
struct XOAuth2 {
    response: String,
    sent: bool,
}

impl async_imap::Authenticator for XOAuth2 {
    type Response = Vec<u8>;

    fn process(&mut self, _challenge: &[u8]) -> Vec<u8> {
        if std::mem::replace(&mut self.sent, true) {
            Vec::new()
        } else {
            self.response.clone().into_bytes()
        }
    }
}

/// A command that didn't finish in time; the session may be mid-response.
#[derive(Debug, thiserror::Error)]
#[error("IMAP command timed out")]
pub struct TimedOut;

async fn timed<T>(fut: impl std::future::Future<Output = async_imap::error::Result<T>>) -> Result<T> {
    match tokio::time::timeout(COMMAND_TIMEOUT, fut).await {
        Ok(r) => Ok(r?),
        Err(_) => Err(TimedOut.into()),
    }
}

impl ImapConn {
    pub async fn connect(account: &Account, login: &Login) -> Result<ImapConn> {
        Self::connect_with(&account.imap, &account.username, login, account.allow_invalid_certs).await
    }

    pub async fn connect_with(server: &ServerConfig, username: &str, login: &Login, allow_invalid: bool) -> Result<ImapConn> {
        let client = open_stream(server, allow_invalid).await?;
        let attempt = match login {
            Login::Password(password) => tokio::time::timeout(CONNECT_TIMEOUT, client.login(username, password)).await,
            Login::OAuth(token) => {
                tokio::time::timeout(CONNECT_TIMEOUT, client.authenticate("XOAUTH2", XOAuth2 { response: crate::oauth::xoauth2(username, token), sent: false }))
                    .await
            }
        };
        let session = match attempt {
            Err(_) => bail!("login timed out"),
            Ok(Err((e, _))) => match e {
                async_imap::error::Error::No(msg) | async_imap::error::Error::Bad(msg) => {
                    return Err(AuthError(msg).into())
                }
                other => return Err(anyhow!(other).context("login failed")),
            },
            Ok(Ok(s)) => s,
        };
        let mut conn = ImapConn { session, caps: Caps::default(), selected: None };
        let caps = timed(conn.session.capabilities()).await?;
        conn.caps = Caps {
            condstore: caps.has_str("CONDSTORE"),
            uidplus: caps.has_str("UIDPLUS"),
            r#move: caps.has_str("MOVE"),
            idle: caps.has_str("IDLE"),
            gmail: caps.has_str("X-GM-EXT-1"),
        };
        if conn.caps.condstore {
            // Enables HIGHESTMODSEQ in SELECT responses on servers that need it.
            let _ = timed(conn.session.run_command_and_check_ok("ENABLE CONDSTORE")).await;
        }
        Ok(conn)
    }

    pub async fn logout(mut self) {
        let _ = tokio::time::timeout(Duration::from_secs(5), self.session.logout()).await;
    }

    pub async fn list_folders(&mut self) -> Result<Vec<RemoteFolder>> {
        let names: Vec<_> = timed(async { self.session.list(Some(""), Some("*")).await?.try_collect::<Vec<_>>().await }).await?;
        let mut out = Vec::new();
        for n in names {
            let (special_use, selectable) = attr_special_use(n.attributes());
            out.push(RemoteFolder {
                path: n.name().to_string(),
                display: decode_utf7(n.name()),
                delimiter: n.delimiter().map(String::from),
                special_use,
                selectable,
            });
        }
        if !out.iter().any(|f| f.path.eq_ignore_ascii_case("INBOX")) {
            out.push(RemoteFolder {
                path: "INBOX".into(),
                display: "INBOX".into(),
                delimiter: Some("/".into()),
                special_use: None,
                selectable: true,
            });
        }
        Ok(out)
    }

    pub async fn select(&mut self, path: &str) -> Result<MailboxStatus> {
        let mbox = if self.caps.condstore {
            timed(self.session.select_condstore(path)).await
        } else {
            timed(self.session.select(path)).await
        }
        .with_context(|| format!("SELECT {path}"))?;
        self.selected = Some(path.to_string());
        let mut status = MailboxStatus {
            uid_validity: mbox.uid_validity.unwrap_or(0),
            uid_next: mbox.uid_next.unwrap_or(0),
            exists: mbox.exists,
            highest_modseq: mbox.highest_modseq,
        };
        // The response codes are optional in practice (and can get lost to
        // unsolicited data from an earlier command). Never guess: ask.
        if status.uid_validity == 0 || status.uid_next == 0 {
            let st = timed(self.session.status(path, "(UIDVALIDITY UIDNEXT)")).await?;
            if status.uid_validity == 0 {
                status.uid_validity = st.uid_validity.unwrap_or(0);
            }
            if status.uid_next == 0 {
                status.uid_next = st.uid_next.unwrap_or(0);
            }
            tracing::debug!(folder = path, uid_validity = status.uid_validity, "SELECT lacked UIDVALIDITY/UIDNEXT; used STATUS");
        }
        Ok(status)
    }

    async fn ensure_selected(&mut self, path: &str) -> Result<()> {
        if self.selected.as_deref() != Some(path) {
            self.select(path).await?;
        }
        Ok(())
    }

    /// Unread count without selecting the folder.
    pub async fn status_unseen(&mut self, path: &str) -> Result<u32> {
        let mbox = timed(self.session.status(path, "(UNSEEN)")).await?;
        Ok(mbox.unseen.unwrap_or(0))
    }

    pub async fn search(&mut self, path: &str, criteria: &str) -> Result<Vec<u32>> {
        self.ensure_selected(path).await?;
        let mut uids: Vec<u32> = timed(self.session.uid_search(criteria)).await?.into_iter().collect();
        uids.sort_unstable();
        Ok(uids)
    }

    /// Fetch messages by UID. Small messages come back complete; large ones
    /// as headers only (`complete = false`).
    pub async fn fetch_messages(&mut self, path: &str, uids: &[u32]) -> Result<Vec<FetchedMessage>> {
        if uids.is_empty() {
            return Ok(Vec::new());
        }
        self.ensure_selected(path).await?;
        let mut meta: Vec<(u32, MsgFlags, Option<i64>, Option<u32>)> = Vec::new();
        for set in uid_sets(uids) {
            let fetches: Vec<Fetch> = timed(async {
                self.session.uid_fetch(&set, "(UID FLAGS INTERNALDATE RFC822.SIZE)").await?.try_collect().await
            })
            .await?;
            for f in fetches {
                if let Some(uid) = f.uid {
                    meta.push((uid, MsgFlags::from_fetch(&f), f.internal_date().map(|d| d.timestamp_millis()), f.size));
                }
            }
        }
        let small: Vec<u32> =
            meta.iter().filter(|m| m.3.unwrap_or(0) <= FULL_FETCH_MAX_BYTES).map(|m| m.0).collect();
        let large: Vec<u32> = meta.iter().filter(|m| m.3.unwrap_or(0) > FULL_FETCH_MAX_BYTES).map(|m| m.0).collect();

        let mut bodies: std::collections::HashMap<u32, (Vec<u8>, bool)> = std::collections::HashMap::new();
        for set in uid_sets(&small) {
            let fetches: Vec<Fetch> =
                timed(async { self.session.uid_fetch(&set, "(UID BODY.PEEK[])").await?.try_collect().await }).await?;
            for f in fetches {
                if let (Some(uid), Some(body)) = (f.uid, f.body()) {
                    bodies.insert(uid, (body.to_vec(), true));
                }
            }
        }
        for set in uid_sets(&large) {
            let fetches: Vec<Fetch> = timed(async {
                self.session.uid_fetch(&set, "(UID BODY.PEEK[HEADER])").await?.try_collect().await
            })
            .await?;
            for f in fetches {
                if let (Some(uid), Some(h)) = (f.uid, f.header()) {
                    bodies.insert(uid, (h.to_vec(), false));
                }
            }
        }

        let mut out = Vec::new();
        for (uid, flags, internal_date, size) in meta {
            if flags.deleted {
                continue;
            }
            if let Some((raw, complete)) = bodies.remove(&uid) {
                out.push(FetchedMessage { uid, flags, internal_date, size, raw, complete });
            }
        }
        Ok(out)
    }

    /// Full RFC 822 source of one message.
    pub async fn fetch_raw(&mut self, path: &str, uid: u32) -> Result<Option<Vec<u8>>> {
        self.ensure_selected(path).await?;
        let fetches: Vec<Fetch> = timed(async {
            self.session.uid_fetch(uid.to_string(), "(UID BODY.PEEK[])").await?.try_collect().await
        })
        .await?;
        Ok(fetches.into_iter().find(|f| f.uid == Some(uid)).and_then(|f| f.body().map(|b| b.to_vec())))
    }

    /// Flags for a UID set; with CONDSTORE only messages changed since `modseq`.
    pub async fn fetch_flags(&mut self, path: &str, uids: &[u32], changed_since: Option<u64>) -> Result<Vec<(u32, MsgFlags)>> {
        self.ensure_selected(path).await?;
        let mut out = Vec::new();
        let sets = match changed_since {
            Some(_) => vec!["1:*".to_string()],
            None => uid_sets(uids),
        };
        for set in sets {
            let query = match changed_since {
                Some(m) => format!("(UID FLAGS) (CHANGEDSINCE {m})"),
                None => "(UID FLAGS)".to_string(),
            };
            let fetches: Vec<Fetch> =
                timed(async { self.session.uid_fetch(&set, &query).await?.try_collect().await }).await?;
            for f in fetches {
                if let Some(uid) = f.uid {
                    out.push((uid, MsgFlags::from_fetch(&f)));
                }
            }
        }
        Ok(out)
    }

    /// Which of `uids` still exist in the folder.
    pub async fn existing_uids(&mut self, path: &str, uids: &[u32]) -> Result<Vec<u32>> {
        self.ensure_selected(path).await?;
        let mut out = Vec::new();
        for set in uid_sets(uids) {
            let found = timed(self.session.uid_search(format!("UID {set}"))).await?;
            out.extend(found);
        }
        out.sort_unstable();
        Ok(out)
    }

    pub async fn store_flags(&mut self, path: &str, uids: &[u32], add: bool, flags: &str) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        self.ensure_selected(path).await?;
        let op = if add { "+FLAGS.SILENT" } else { "-FLAGS.SILENT" };
        for set in uid_sets(uids) {
            let _: Vec<Fetch> = timed(async {
                self.session.uid_store(&set, format!("{op} ({flags})")).await?.try_collect().await
            })
            .await?;
        }
        Ok(())
    }

    /// Move messages; falls back to COPY + \Deleted + expunge without MOVE.
    pub async fn move_uids(&mut self, path: &str, uids: &[u32], dest: &str) -> Result<()> {
        if uids.is_empty() || path == dest {
            return Ok(());
        }
        self.ensure_selected(path).await?;
        for set in uid_sets(uids) {
            if self.caps.r#move {
                timed(self.session.uid_mv(&set, dest)).await.with_context(|| format!("MOVE to {dest}"))?;
            } else {
                timed(self.session.uid_copy(&set, dest)).await.with_context(|| format!("COPY to {dest}"))?;
                let _: Vec<Fetch> = timed(async {
                    self.session.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").await?.try_collect().await
                })
                .await?;
                self.expunge_set(&set).await?;
            }
        }
        Ok(())
    }

    async fn expunge_set(&mut self, set: &str) -> Result<()> {
        if self.caps.uidplus {
            let _: Vec<_> = timed(async { self.session.uid_expunge(set).await?.try_collect().await }).await?;
        } else {
            let _: Vec<_> = timed(async { self.session.expunge().await?.try_collect().await }).await?;
        }
        Ok(())
    }

    /// Permanently delete messages (used for Trash/Spam and replaced drafts).
    pub async fn delete_uids(&mut self, path: &str, uids: &[u32]) -> Result<()> {
        if uids.is_empty() {
            return Ok(());
        }
        self.ensure_selected(path).await?;
        for set in uid_sets(uids) {
            let _: Vec<Fetch> = timed(async {
                self.session.uid_store(&set, "+FLAGS.SILENT (\\Deleted)").await?.try_collect().await
            })
            .await?;
            self.expunge_set(&set).await?;
        }
        Ok(())
    }

    pub async fn append(&mut self, path: &str, flags: &str, raw: &[u8]) -> Result<()> {
        timed(self.session.append(path, Some(flags), None, raw)).await.with_context(|| format!("APPEND to {path}"))?;
        // APPEND to the selected folder can change its UIDNEXT; force re-select.
        if self.selected.as_deref() == Some(path) {
            self.selected = None;
        }
        Ok(())
    }

    /// UIDs of messages by Message-ID (without `<>`): one SEARCH each in the
    /// folder as currently selected (SELECT it first so new arrivals are
    /// visible). The brackets are searched for too: HEADER matches substrings.
    pub async fn find_by_message_ids(&mut self, path: &str, message_ids: &[String]) -> Result<HashMap<String, u32>> {
        let mut found = HashMap::new();
        for mid in message_ids {
            if found.contains_key(mid) {
                continue;
            }
            let uids = self.search(path, &format!("HEADER Message-ID {}", quote(&format!("<{mid}>")))).await?;
            if let Some(uid) = uids.into_iter().max() {
                found.insert(mid.clone(), uid);
            }
        }
        Ok(found)
    }

    pub async fn create_folder(&mut self, path: &str) -> Result<()> {
        match timed(self.session.create(path)).await {
            Ok(()) => Ok(()),
            Err(e) if e.to_string().to_lowercase().contains("exist") => Ok(()),
            Err(e) => Err(e),
        }
    }

    /// Wait for new mail on a folder with IDLE. Returns when the server
    /// reports a change, after `max_wait`, or with an error. Consumes and
    /// returns the connection.
    pub async fn idle(mut self, path: &str, max_wait: Duration) -> (Result<bool>, Option<ImapConn>) {
        if let Err(e) = self.select(path).await {
            return (Err(e), Some(self));
        }
        let caps = self.caps.clone();
        let mut handle = self.session.idle();
        if let Err(e) = handle.init().await {
            return (Err(e.into()), None);
        }
        let (fut, _stop) = handle.wait_with_timeout(max_wait);
        let result = fut.await;
        let changed = matches!(result, Ok(async_imap::extensions::idle::IdleResponse::NewData(_)));
        match handle.done().await {
            Ok(session) => {
                let conn = ImapConn { session, caps, selected: Some(path.to_string()) };
                (result.map(|_| changed).map_err(Into::into), Some(conn))
            }
            Err(e) => (Err(e.into()), None),
        }
    }
}

pub fn is_auth_error(e: &anyhow::Error) -> bool {
    e.downcast_ref::<AuthError>().is_some() || e.downcast_ref::<crate::oauth::SignInRequired>().is_some()
}

/// The connection can't be reused: it dropped, or a timeout left it
/// between a command and its response (reading on would misparse replies).
pub fn is_connection_error(e: &anyhow::Error) -> bool {
    use async_imap::error::Error as E;
    e.chain().any(|c| {
        c.is::<TimedOut>() || c.is::<std::io::Error>() || matches!(c.downcast_ref::<E>(), Some(E::Io(_) | E::ConnectionLost | E::Parse(_)))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn folder(path: &str, special: Option<&'static str>) -> RemoteFolder {
        RemoteFolder { path: path.into(), display: decode_utf7(path), delimiter: Some("/".into()), special_use: special, selectable: true }
    }

    #[test]
    fn utf7_round_trip() {
        assert_eq!(decode_utf7("&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-"), "Отправленные");
        assert_eq!(decode_utf7("Caf&AOk-"), "Café");
        assert_eq!(decode_utf7("A&-B"), "A&B");
        assert_eq!(encode_utf7("Café"), "Caf&AOk-");
        assert_eq!(encode_utf7("A&B"), "A&-B");
        assert_eq!(decode_utf7(&encode_utf7("الأرشيف")), "الأرشيف");
    }

    #[test]
    fn folder_mapping() {
        let fs = vec![
            folder("INBOX", None),
            folder("[Gmail]/Sent Mail", Some("\\Sent")),
            folder("[Gmail]/All Mail", Some("\\All")),
            folder("Sent", None),
            folder("INBOX.Trash", None),
            folder("Archive/Inbox", None),
            folder("Projects", None),
            folder("&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-", None),
        ];
        let m = map_folders(&fs);
        let label = |p: &str| m.iter().find(|(f, _, _)| f.path == p).map(|(_, l, _)| l.clone()).unwrap();
        assert_eq!(label("INBOX"), "INBOX");
        assert_eq!(label("[Gmail]/Sent Mail"), "SENT");
        assert_eq!(label("[Gmail]/All Mail"), "ALL");
        assert_eq!(label("Sent"), "Sent", "second SENT claimant becomes a user folder");
        assert_eq!(label("INBOX.Trash"), "TRASH");
        assert_eq!(label("Archive/Inbox"), "Archive/Inbox");
        assert_eq!(label("Projects"), "Projects");
        assert_eq!(label("&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-"), "&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-");
    }

    #[test]
    fn uid_set_compression() {
        assert_eq!(uid_sets(&[5, 1, 2, 3, 9, 10]), vec!["1:3,5,9:10"]);
        let many: Vec<u32> = (1..5000).map(|i| i * 2).collect();
        let sets = uid_sets(&many);
        assert!(sets.len() > 1);
        assert!(sets.iter().all(|s| s.len() <= UID_SET_MAX_LEN));
        assert!(uid_sets(&[]).is_empty());
        assert_eq!(uid_sets(&[u32::MAX - 1, u32::MAX]), vec![format!("{}:{}", u32::MAX - 1, u32::MAX)]);
        assert_eq!(uid_sets(&[u32::MAX, 1]), vec![format!("1,{}", u32::MAX)]);
    }

    #[test]
    fn utf7_edge_cases() {
        // Unterminated or invalid shifts stay literal.
        assert_eq!(decode_utf7("&Jjo"), "&Jjo");
        assert_eq!(decode_utf7("&!!-"), "&!!-");
        assert_eq!(decode_utf7(&encode_utf7("Fotos 😀 & más")), "Fotos 😀 & más");
    }

    #[test]
    fn names_and_errors() {
        assert_eq!(label_from_name("INBOX.Sent"), Some(labels::SENT));
        assert_eq!(label_from_name("Archive/Inbox"), None);
        assert!(is_connection_error(&anyhow!(TimedOut).context("x")));
        assert!(!is_connection_error(&anyhow!("NO [OVERQUOTA] mailbox full")));
        assert!(is_auth_error(&AuthError("bad".into()).into()));
    }
}
