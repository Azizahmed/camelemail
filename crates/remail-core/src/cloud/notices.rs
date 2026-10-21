//! Change notices: a WebSocket to `/v1/notices` that says which section
//! changed (never what), so devices sync within seconds instead of at the
//! next poll. When it's down, the sync loop polls more often; it reconnects
//! with backoff.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use futures::{SinkExt, StreamExt};
use tokio::net::TcpStream;
use tokio_tungstenite::tungstenite::Message;

use remail_proto::api::{Notice, NoticeAuth};

use super::{CloudInner, SignedOutReason};
use crate::imap::MaybeTls;

const MIN_BACKOFF: Duration = Duration::from_secs(2);
const MAX_BACKOFF: Duration = Duration::from_secs(300);

pub(crate) async fn listen(c: Arc<CloudInner>) {
    let mut backoff = MIN_BACKOFF;
    loop {
        if let Err(e) = session(&c).await {
            tracing::debug!("change notices: {e:#}");
        }
        // A connection that was up (and then dropped) starts the backoff over.
        if c.ws_connected.swap(false, Ordering::SeqCst) {
            backoff = MIN_BACKOFF;
        }
        if !c.signed_in() {
            return;
        }
        tokio::select! {
            _ = tokio::time::sleep(backoff) => {}
            // The server answered again (after being away): reconnect now.
            _ = c.server_back.notified() => {}
        }
        backoff = (backoff * 2).min(MAX_BACKOFF);
    }
}

/// One connection, until it closes.
async fn session(c: &Arc<CloudInner>) -> Result<()> {
    let server = c.session_server()?;
    let token = c.access_token().await?;
    let url = reqwest::Url::parse(&server)?;
    let host = url.host_str().context("server has no host")?.trim_start_matches('[').trim_end_matches(']').to_string();
    let tls = url.scheme() == "https";
    let port = url.port_or_known_default().unwrap_or(if tls { 443 } else { 80 });
    let tcp = tokio::time::timeout(Duration::from_secs(10), TcpStream::connect((host.as_str(), port))).await.context("connect timed out")??;
    let stream = if tls { crate::imap::tls_wrap(tcp, &host, false).await? } else { MaybeTls::Plain(tcp) };
    let ws_url = format!("{}://{}{}/v1/notices", if tls { "wss" } else { "ws" }, url.authority(), url.path().trim_end_matches('/'));
    let (mut ws, _) = tokio_tungstenite::client_async(ws_url.as_str(), stream).await.context("WebSocket handshake")?;
    ws.send(Message::text(serde_json::to_string(&NoticeAuth { access_token: token })?)).await?;
    c.ws_connected.store(true, Ordering::SeqCst);
    // Catch up on anything missed while disconnected.
    c.wake.notify_one();
    while let Some(msg) = ws.next().await {
        let text = match msg? {
            Message::Text(t) => t.to_string(),
            Message::Close(_) => break,
            _ => continue,
        };
        let Ok(notice) = serde_json::from_str::<Notice>(&text) else { continue };
        match notice {
            // Also what the server sends when this socket missed notices: look again.
            Notice::Hello { server_time } => {
                c.observe(server_time);
                c.wake.notify_one();
            }
            Notice::Section { section, version } => {
                let have = c.cache.load(c.engine.store(), &section).ok().flatten().map(|s| s.version).unwrap_or(0);
                if version > have {
                    c.wake.notify_one();
                }
            }
            Notice::Org { .. } => {
                c.org_stale.store(true, Ordering::SeqCst);
                c.wake.notify_one();
            }
            Notice::SignedOut => {
                let me = c.clone();
                tokio::spawn(async move { me.end_session(SignedOutReason::Revoked).await });
                bail!("this device was signed out");
            }
        }
    }
    Ok(())
}
