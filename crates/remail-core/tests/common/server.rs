//! A small stand-in for the Remail server: sign-in with rotating refresh
//! tokens, sections with versions and `If-Match`, org config and providers,
//! and Google's server-side code exchange. Enough for the client's sync.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};

use axum::extract::{Path, State};
use axum::http::{HeaderMap, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use remail_core::proto::api::*;
use remail_core::proto::org::{OrgConfig, Policies};
use remail_core::proto::sync::SectionDoc;
use remail_core::proto::{ApiError, ErrorCode};

#[derive(Default)]
pub struct Mock {
    /// section → (version, document)
    pub sections: HashMap<String, (i64, SectionDoc)>,
    /// refresh token → device
    pub refresh: HashMap<String, String>,
    /// access token → device
    pub access: HashMap<String, String>,
    pub revoked: HashSet<String>,
    pub org: OrgConfig,
    pub google_server_exchange: bool,
    /// Before the next write of a section, this document lands first (another device's write).
    pub race: HashMap<String, SectionDoc>,
    /// (device, section, If-Match version, answer)
    pub puts: Vec<(String, String, i64, u16)>,
    /// Refresh requests (device), to see rotation.
    pub refreshes: Vec<String>,
    pub google_exchanges: Vec<String>,
    /// Refresh tokens sent to `/v1/providers/google/refresh`.
    pub google_refreshes: Vec<String>,
    counter: u64,
}

pub type SharedMock = Arc<Mutex<Mock>>;

fn err(code: ErrorCode, msg: &str) -> Response {
    let status = StatusCode::from_u16(code.status()).unwrap();
    (status, Json(remail_core::proto::error::ErrorBody { error: ApiError::new(code, msg) })).into_response()
}

pub fn me() -> Me {
    Me {
        id: "usr_1".into(),
        name: "Aziz".into(),
        email: "aziz@example.com".into(),
        role: "user".into(),
        org_id: "org_1".into(),
        org_name: "Acme".into(),
        totp: false,
        policies: Policies::default(),
    }
}

impl Mock {
    fn tokens(&mut self, device: &str) -> Tokens {
        self.counter += 1;
        let (access, refresh) = (format!("at-{}", self.counter), format!("rt-{}", self.counter));
        self.access.insert(access.clone(), device.to_string());
        self.refresh.insert(refresh.clone(), device.to_string());
        Tokens { access_token: access, access_expires_at: remail_core::store::now_ms() + 900_000, refresh_token: refresh, device_id: device.into(), user: me() }
    }

    fn device(&self, headers: &HeaderMap) -> Option<String> {
        let token = headers.get("authorization")?.to_str().ok()?.strip_prefix("Bearer ")?;
        let device = self.access.get(token)?;
        (!self.revoked.contains(device)).then(|| device.clone())
    }

    /// Sign this device out on the server: its tokens stop working.
    pub fn revoke(&mut self, device: &str) {
        self.revoked.insert(device.to_string());
        self.access.retain(|_, d| d != device);
    }

    pub fn doc(&self, section: &str) -> SectionDoc {
        self.sections.get(section).map(|s| s.1.clone()).unwrap_or_default()
    }
}

macro_rules! authed {
    ($s:expr, $h:expr) => {
        match $s.device(&$h) {
            Some(d) => d,
            None => return err(ErrorCode::Unauthorized, "sign in"),
        }
    };
}

async fn meta() -> Json<Meta> {
    Json(Meta {
        mode: "hosted".into(),
        version: "test".into(),
        min_client: "0".into(),
        org_name: Some("Acme".into()),
        signup: "open".into(),
        verify_email: false,
        licence: LicenceState::None,
    })
}

async fn login(State(s): State<SharedMock>, Json(req): Json<LoginRequest>) -> Response {
    if req.password != "correct horse" {
        return err(ErrorCode::InvalidCredentials, "wrong email or password");
    }
    let mut m = s.lock().unwrap();
    let device = format!("dev_{}", m.counter + 1);
    Json(m.tokens(&device)).into_response()
}

async fn refresh(State(s): State<SharedMock>, Json(req): Json<RefreshRequest>) -> Response {
    let mut m = s.lock().unwrap();
    let Some(device) = m.refresh.remove(&req.refresh_token) else { return err(ErrorCode::InvalidToken, "refresh token unknown") };
    if m.revoked.contains(&device) {
        return err(ErrorCode::InvalidToken, "device signed out");
    }
    m.refreshes.push(device.clone());
    Json(m.tokens(&device)).into_response()
}

async fn logout(State(s): State<SharedMock>, headers: HeaderMap) -> Response {
    let mut m = s.lock().unwrap();
    let device = authed!(m, headers);
    m.revoke(&device);
    StatusCode::NO_CONTENT.into_response()
}

async fn get_me(State(s): State<SharedMock>, headers: HeaderMap) -> Response {
    let m = s.lock().unwrap();
    authed!(m, headers);
    Json(me()).into_response()
}

async fn org_config(State(s): State<SharedMock>, headers: HeaderMap) -> Response {
    let m = s.lock().unwrap();
    authed!(m, headers);
    Json(OrgConfigResponse { version: 1, config: m.org.clone() }).into_response()
}

async fn providers(State(s): State<SharedMock>, headers: HeaderMap) -> Response {
    let m = s.lock().unwrap();
    authed!(m, headers);
    Json(providers_for(&m.org, m.google_server_exchange)).into_response()
}

async fn index(State(s): State<SharedMock>, headers: HeaderMap) -> Response {
    let m = s.lock().unwrap();
    authed!(m, headers);
    let sections = m.sections.iter().map(|(id, (v, _))| SectionInfo { id: id.clone(), version: *v, updated_at: 0 }).collect();
    Json(SectionIndex { sections, server_time: remail_core::store::now_ms() }).into_response()
}

async fn get_section(State(s): State<SharedMock>, headers: HeaderMap, Path(id): Path<String>) -> Response {
    let m = s.lock().unwrap();
    authed!(m, headers);
    let (version, doc) = m.sections.get(&id).cloned().unwrap_or_default();
    Json(Section { id, version, doc }).into_response()
}

async fn put_section(State(s): State<SharedMock>, headers: HeaderMap, Path(id): Path<String>, Json(body): Json<PutSection>) -> Response {
    let mut m = s.lock().unwrap();
    let device = authed!(m, headers);
    if let Some(other) = m.race.remove(&id) {
        let e = m.sections.entry(id.clone()).or_default();
        e.0 += 1;
        e.1.merge(&other);
    }
    let want: i64 = headers.get("if-match").and_then(|v| v.to_str().ok()).and_then(|v| v.trim_matches('"').strip_prefix('v')).and_then(|v| v.parse().ok()).unwrap_or(-1);
    let (version, doc) = m.sections.get(&id).cloned().unwrap_or_default();
    if want != version {
        m.puts.push((device, id.clone(), want, 412));
        let body = VersionMismatch { error: ApiError::new(ErrorCode::VersionMismatch, "changed"), current: Section { id, version, doc } };
        return (StatusCode::PRECONDITION_FAILED, Json(body)).into_response();
    }
    m.puts.push((device, id.clone(), want, 200));
    m.sections.insert(id, (version + 1, body.doc));
    Json(PutSectionResponse { version: version + 1 }).into_response()
}

async fn google_token(State(s): State<SharedMock>, headers: HeaderMap, Json(req): Json<GoogleExchange>) -> Response {
    let mut m = s.lock().unwrap();
    authed!(m, headers);
    m.google_exchanges.push(req.code.clone());
    Json(GoogleTokens {
        access_token: "gat-1".into(),
        expires_in: 3600,
        refresh_token: Some("grt-1".into()),
        id_token: Some(super::google_id_token("me@gmail.com", "sub-me")),
    })
    .into_response()
}

async fn google_refresh(State(s): State<SharedMock>, headers: HeaderMap, Json(req): Json<GoogleRefresh>) -> Response {
    let mut m = s.lock().unwrap();
    authed!(m, headers);
    m.google_refreshes.push(req.refresh_token.clone());
    if req.refresh_token != "grt-1" {
        return err(ErrorCode::InvalidToken, "Google refused the sign-in (invalid_grant).");
    }
    Json(GoogleTokens { access_token: "gat-2".into(), expires_in: 3600, refresh_token: None, id_token: None }).into_response()
}

pub fn router(state: SharedMock) -> Router {
    Router::new()
        .route("/v1/meta", get(meta))
        .route("/v1/auth/login", post(login))
        .route("/v1/auth/refresh", post(refresh))
        .route("/v1/auth/logout", post(logout))
        .route("/v1/me", get(get_me))
        .route("/v1/org/config", get(org_config))
        .route("/v1/providers", get(providers))
        .route("/v1/providers/google/token", post(google_token))
        .route("/v1/providers/google/refresh", post(google_refresh))
        .route("/v1/sections", get(index))
        .route("/v1/sections/{id}", get(get_section).put(put_section))
        .with_state(state)
}
