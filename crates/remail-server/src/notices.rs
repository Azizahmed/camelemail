//! Change notices: `GET /v1/notices` (WebSocket).
//!
//! The client's first message is [`NoticeAuth`]; the server answers
//! [`Notice::Hello`] and then says which section or org config changed, never
//! what it contains. Events go through an in-process broadcast channel and
//! Postgres `NOTIFY`, so every server process (and the CLI) reaches every
//! connected device.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{CloseFrame, Message, WebSocket, WebSocketUpgrade};
use axum::extract::State;
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;
use remail_proto::api::{Notice, NoticeAuth};
use serde::{Deserialize, Serialize};
use sqlx::postgres::PgListener;
use sqlx::PgPool;
use tokio::sync::broadcast;

use crate::app::AppState;
use crate::auth::AuthCtx;

pub const CHANNEL: &str = "remail_notices";

/// What changed. IDs and versions only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "k", rename_all = "camelCase")]
pub enum HubEvent {
    Section { user: String, section: String, version: i64 },
    Org { org: String, version: i64 },
    /// The device was signed out (revoked, logout, reset, password change).
    Device { device: String },
}

#[derive(Serialize, Deserialize)]
struct Wire {
    /// Sending process, so it doesn't deliver its own events twice.
    o: String,
    e: HubEvent,
}

pub struct Hub {
    tx: broadcast::Sender<HubEvent>,
    instance: String,
}

impl Default for Hub {
    fn default() -> Self {
        Self::new()
    }
}

impl Hub {
    pub fn new() -> Self {
        let (tx, _) = broadcast::channel(1024);
        Hub { tx, instance: remail_proto::new_id("srv") }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<HubEvent> {
        self.tx.subscribe()
    }

    /// Delivers locally right away and to other processes through `NOTIFY`.
    pub async fn publish(&self, db: &PgPool, e: HubEvent) {
        let _ = self.tx.send(e.clone());
        notify(db, &self.instance, e).await;
    }

    /// Relays `NOTIFY`s from other processes; reconnects when the
    /// connection drops.
    pub fn spawn_listener(self: Arc<Self>, db: PgPool) {
        tokio::spawn(async move {
            loop {
                match PgListener::connect_with(&db).await {
                    Ok(mut l) => {
                        if l.listen(CHANNEL).await.is_ok() {
                            while let Ok(n) = l.recv().await {
                                if let Ok(w) = serde_json::from_str::<Wire>(n.payload()) {
                                    if w.o != self.instance {
                                        let _ = self.tx.send(w.e);
                                    }
                                }
                            }
                        }
                    }
                    Err(e) => tracing::debug!(error = %e, "notice listener can't connect"),
                }
                if db.is_closed() {
                    return;
                }
                tokio::time::sleep(Duration::from_secs(2)).await;
            }
        });
    }
}

/// `NOTIFY` from anywhere (the CLI uses this with its own instance id).
pub async fn notify(db: &PgPool, instance: &str, e: HubEvent) {
    let Ok(payload) = serde_json::to_string(&Wire { o: instance.to_string(), e }) else { return };
    if let Err(err) = sqlx::query("select pg_notify($1, $2)").bind(CHANNEL).bind(payload).execute(db).await {
        tracing::warn!(error = %err, "notice not sent to other processes");
    }
}

pub fn routes() -> Router<AppState> {
    Router::new().route("/notices", get(upgrade))
}

async fn upgrade(
    State(s): State<AppState>,
    ws: Result<WebSocketUpgrade, axum::extract::ws::rejection::WebSocketUpgradeRejection>,
) -> Response {
    match ws {
        Ok(ws) => ws.on_upgrade(move |socket| run(s, socket)),
        Err(_) => crate::error::AppError::bad_request("Connect with a WebSocket.").into_response(),
    }
}

async fn send(socket: &mut WebSocket, notice: &Notice) -> bool {
    match serde_json::to_string(notice) {
        Ok(text) => socket.send(Message::Text(text.into())).await.is_ok(),
        Err(_) => false,
    }
}

async fn close(mut socket: WebSocket, code: u16, reason: &'static str) {
    let _ = socket.send(Message::Close(Some(CloseFrame { code, reason: reason.into() }))).await;
}

async fn device_live(s: &AppState, device_id: &str) -> bool {
    sqlx::query_scalar::<_, bool>("select revoked_at is null from devices where id = $1")
        .bind(device_id)
        .fetch_optional(&s.db)
        .await
        .ok()
        .flatten()
        .unwrap_or(false)
}

async fn run(s: AppState, mut socket: WebSocket) {
    // Authenticate with the first message (tokens never go in the URL).
    let first = tokio::time::timeout(Duration::from_secs(10), socket.recv()).await;
    let token = match first {
        Ok(Some(Ok(Message::Text(t)))) => serde_json::from_str::<NoticeAuth>(&t).ok().map(|a| a.access_token),
        _ => None,
    };
    let ctx = match token {
        Some(t) => AuthCtx::from_access(&s, &t).await.ok(),
        None => None,
    };
    let Some(ctx) = ctx else {
        return close(socket, 4401, "unauthorized").await;
    };
    let mut rx = s.hub.subscribe();
    if !send(&mut socket, &Notice::Hello { server_time: remail_proto::now_ms() }).await {
        return;
    }
    let mut tick = tokio::time::interval(Duration::from_secs(30));
    tick.tick().await;
    loop {
        tokio::select! {
            ev = rx.recv() => {
                let notice = match ev {
                    Ok(HubEvent::Section { user, section, version }) if user == ctx.user_id => {
                        Notice::Section { section, version }
                    }
                    Ok(HubEvent::Org { org, version }) if org == ctx.org_id => Notice::Org { version },
                    Ok(HubEvent::Device { device }) if device == ctx.device_id => {
                        let _ = send(&mut socket, &Notice::SignedOut).await;
                        return close(socket, 4403, "signed out").await;
                    }
                    Ok(_) => continue,
                    // Missed events: the client re-reads the index on Hello.
                    Err(broadcast::error::RecvError::Lagged(_)) => Notice::Hello { server_time: remail_proto::now_ms() },
                    Err(broadcast::error::RecvError::Closed) => return,
                };
                if !send(&mut socket, &notice).await {
                    return;
                }
            }
            msg = socket.recv() => match msg {
                Some(Ok(Message::Close(_))) | None | Some(Err(_)) => return,
                _ => {}
            },
            _ = tick.tick() => {
                // Catches a revocation whose event this process missed.
                if !device_live(&s, &ctx.device_id).await {
                    let _ = send(&mut socket, &Notice::SignedOut).await;
                    return close(socket, 4403, "signed out").await;
                }
                if socket.send(Message::Ping(Vec::new().into())).await.is_err() {
                    return;
                }
            }
        }
    }
}
