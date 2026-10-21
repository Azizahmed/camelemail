//! The audit log: every security event, admin action and decrypt path.
//!
//! Rows carry IDs, actions and small metadata only, never secrets, tokens or
//! section content. Writing a row never fails the request that caused it.

use std::net::IpAddr;

use serde_json::{json, Value};
use sqlx::{PgExecutor, PgPool};

#[derive(Debug, Clone, Copy)]
pub enum Actor<'a> {
    User(&'a str),
    Admin(&'a str),
    System,
}

impl Actor<'_> {
    fn parts(&self) -> (&'static str, Option<&str>) {
        match self {
            Actor::User(id) => ("user", Some(id)),
            Actor::Admin(id) => ("admin", Some(id)),
            Actor::System => ("system", None),
        }
    }
}

pub struct Event<'a> {
    pub org_id: Option<&'a str>,
    pub actor: Actor<'a>,
    pub action: &'a str,
    pub target: Option<&'a str>,
    pub ip: Option<IpAddr>,
    pub meta: Value,
}

impl<'a> Event<'a> {
    pub fn new(action: &'a str, actor: Actor<'a>) -> Self {
        Event { org_id: None, actor, action, target: None, ip: None, meta: json!({}) }
    }
    pub fn org(mut self, org_id: &'a str) -> Self {
        self.org_id = Some(org_id);
        self
    }
    pub fn target(mut self, target: &'a str) -> Self {
        self.target = Some(target);
        self
    }
    pub fn ip(mut self, ip: Option<IpAddr>) -> Self {
        self.ip = ip;
        self
    }
    pub fn meta(mut self, meta: Value) -> Self {
        self.meta = meta;
        self
    }
}

/// Writes an audit row; failures are logged, not returned.
pub async fn log(db: impl PgExecutor<'_>, e: Event<'_>) {
    let (kind, actor_id) = e.actor.parts();
    let res = sqlx::query(
        "insert into audit_log (org_id, actor_kind, actor_id, action, target, ip, meta)
         values ($1, $2, $3, $4, $5, $6::text::inet, $7)",
    )
    .bind(e.org_id)
    .bind(kind)
    .bind(actor_id)
    .bind(e.action)
    .bind(e.target)
    .bind(e.ip.map(|i| i.to_string()))
    .bind(&e.meta)
    .execute(db)
    .await;
    if let Err(err) = res {
        tracing::warn!(action = e.action, error = %err, "audit row not written");
    }
}

/// Window over which decrypt-path reads by one device collapse into one row.
pub const DECRYPT_WINDOW_MINUTES: i32 = 10;

/// `decrypt.<path>` aggregated: one row per device per 10 minutes, with a
/// `count` in its metadata.
pub async fn decrypt(db: &PgPool, path: &str, org_id: &str, user_id: &str, device_id: &str) {
    let action = format!("decrypt.{path}");
    let bumped = sqlx::query(
        "update audit_log set meta = jsonb_set(meta, '{count}', to_jsonb(coalesce((meta->>'count')::bigint, 0) + 1))
         where id = (select id from audit_log
                     where action = $1 and target = $2 and at > now() - make_interval(mins => $3)
                     order by id desc limit 1)",
    )
    .bind(&action)
    .bind(device_id)
    .bind(DECRYPT_WINDOW_MINUTES)
    .execute(db)
    .await;
    match bumped {
        Ok(r) if r.rows_affected() > 0 => {}
        Ok(_) => {
            log(
                db,
                Event::new(&action, Actor::User(user_id)).org(org_id).target(device_id).meta(json!({"count": 1})),
            )
            .await
        }
        Err(err) => tracing::warn!(action, error = %err, "decrypt audit not written"),
    }
}
