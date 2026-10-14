//! Browser sessions: the web client trades its token once for a cookie it can't read, and the
//! cookie stands for the token from then on. `POST /v1/session` signs in, `DELETE /v1/session`
//! signs out, and [`resolve`] is the layer that turns the cookie into a [`Caller`] on the routes
//! the web client may use; a route without it, `/mcp`, takes only Bearer.
//!
//! The session id is 256 random bits and only its SHA-256 is stored. A request that changes state
//! with the cookie, and the sign-in itself, must come from the web client's own page: `Origin` is
//! `MERCURY_PUBLIC_ORIGIN` and `X-Mercury-Client` is `web`, since SameSite=Strict and the absence of
//! CORS headers alone are a browser's promise, not the core's.

use std::collections::HashMap;
use std::net::{IpAddr, Ipv6Addr, SocketAddr};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::extract::rejection::JsonRejection;
use axum::extract::{ConnectInfo, Extension, MatchedPath, Request, State};
use axum::handler::Handler;
use axum::http::header::{AUTHORIZATION, CACHE_CONTROL, COOKIE, ORIGIN, SET_COOKIE};
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use base64::Engine;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use chrono::{DateTime, TimeDelta, Utc};
use mercury_schema::api::{ErrorCode, SessionRequest};
use serde_json::json;
use sha2::{Digest, Sha256};
use sqlx::{Sqlite, Transaction};

use crate::audit::{self, AuditError};
use crate::db::{self, Db};
use crate::error::ApiError;
use crate::identity::{self, AuthError, Caller};
use crate::internal::AllowList;
use crate::limits::{self, Rate, RateLimiter};

/// The session cookie. The `__Host-` prefix makes a browser refuse it unless it is `Secure`, has
/// `Path=/` and names no domain, so another subdomain can't plant one.
pub const COOKIE_NAME: &str = "__Host-mercury_session";

/// The header the web client adds to everything it sends, and its only value.
pub const CLIENT_HEADER: &str = "x-mercury-client";
pub const CLIENT_VALUE: &str = "web";

/// How long a session lasts unless `MERCURY_SESSION_TTL` says otherwise.
pub const DEFAULT_TTL: TimeDelta = TimeDelta::hours(12);

/// Sign-in attempts per source address: 10 a minute, a burst of 3.
pub const SIGNIN_RATE: Rate = Rate {
    per_hour: 600,
    burst: 3,
};

/// An attempt the limit refuses is audited at most this often per source.
const REFUSED_AUDIT_EVERY: Duration = Duration::from_secs(60);

/// When a map of sources is pruned of the ones that have gone quiet.
const PRUNE_ABOVE: usize = 1024;

/// What the sessions are set up with, from the environment.
#[derive(Debug, Clone)]
pub struct SessionConfig {
    pub ttl: TimeDelta,
    /// The web client's own origin, `MERCURY_PUBLIC_ORIGIN`. Without it no request can pass the
    /// check on where it came from, so none is let through: signing in and every change made with
    /// the cookie are refused.
    pub public_origin: Option<String>,
    /// The peers whose `X-Forwarded-For` is believed: Caddy on the `edge` network.
    pub trusted_proxies: AllowList,
}

impl Default for SessionConfig {
    fn default() -> Self {
        Self {
            ttl: DEFAULT_TTL,
            public_origin: None,
            trusted_proxies: AllowList::default(),
        }
    }
}

impl SessionConfig {
    /// The settings from the values of `MERCURY_PUBLIC_ORIGIN`, `MERCURY_SESSION_TTL` and
    /// `MERCURY_TRUSTED_PROXY_CIDRS`. A value that can't be read is an error, for the core's start.
    pub fn parse(
        origin: Option<&str>,
        ttl: Option<&str>,
        trusted_proxies: Option<&str>,
    ) -> Result<Self, String> {
        let public_origin = origin
            .map(str::trim)
            .filter(|origin| !origin.is_empty())
            .map(origin_of)
            .transpose()?;
        let ttl = match ttl.map(str::trim).filter(|ttl| !ttl.is_empty()) {
            None => DEFAULT_TTL,
            Some(text) => {
                crate::cli::parse_validity(text).map_err(|err| format!("MERCURY_SESSION_TTL: {err}"))?
            }
        };
        Ok(Self {
            ttl,
            public_origin,
            trusted_proxies: AllowList::parse_for("MERCURY_TRUSTED_PROXY_CIDRS", trusted_proxies)?,
        })
    }
}

/// The origin a browser sends for a page at `text`: scheme, host and a port other than the
/// default, written as the browser writes it.
fn origin_of(text: &str) -> Result<String, String> {
    let wrong =
        || "MERCURY_PUBLIC_ORIGIN must be a page's origin, like https://mercury.example.com".to_owned();
    let url = reqwest::Url::parse(text).map_err(|_| wrong())?;
    let plain = matches!(url.scheme(), "http" | "https")
        && url.host().is_some()
        && matches!(url.path(), "" | "/")
        && url.query().is_none()
        && url.fragment().is_none()
        && url.username().is_empty()
        && url.password().is_none();
    if plain {
        Ok(url.origin().ascii_serialization())
    } else {
        Err(wrong())
    }
}

/// The sessions' state: the database, the settings and the sign-in limit.
#[derive(Debug, Clone)]
pub struct Sessions {
    db: Db,
    config: Arc<SessionConfig>,
    signins: RateLimiter,
    /// When a source's refused sign-in last reached the audit.
    refused: Arc<Mutex<HashMap<String, Instant>>>,
}

#[derive(Debug, thiserror::Error)]
enum CreateError {
    #[error("cannot draw random bytes: {0}")]
    Random(#[from] getrandom::Error),
    #[error("the database failed: {0}")]
    Db(#[from] sqlx::Error),
    #[error("{0}")]
    Audit(#[from] AuditError),
}

impl Sessions {
    pub fn new(db: Db, config: SessionConfig) -> Self {
        Self {
            db,
            config: Arc::new(config),
            signins: RateLimiter::new(SIGNIN_RATE),
            refused: Arc::default(),
        }
    }

    pub fn config(&self) -> &SessionConfig {
        &self.config
    }

    /// Where a request comes from, as the sign-in limit counts it: the connection's peer, or, for
    /// a peer that is a trusted proxy, the first address from the right of `X-Forwarded-For` that
    /// isn't one. An IPv6 address counts as its /64, the smallest network one holder has.
    fn source(&self, peer: Option<IpAddr>, headers: &HeaderMap) -> String {
        let Some(peer) = peer else {
            return "unknown".to_owned();
        };
        let trusted = &self.config.trusted_proxies;
        if !trusted.allows(peer) {
            return normalize(peer);
        }
        let forwarded: Vec<&str> = headers
            .get_all("x-forwarded-for")
            .iter()
            .filter_map(|value| value.to_str().ok())
            .flat_map(|value| value.split(','))
            .collect();
        for address in forwarded.into_iter().rev() {
            // an entry that isn't an address ends the walk: nothing past it is believed
            let Ok(address) = address.trim().parse::<IpAddr>() else {
                break;
            };
            if !trusted.allows(address) {
                return normalize(address);
            }
        }
        normalize(peer)
    }

    /// One `session_signin` row: what became of an attempt, from where, and with which token when
    /// it had the shape of one. A failed write is logged, never the reason an answer changes.
    async fn record_attempt(&self, outcome: &str, source: &str, token: Option<&str>, now: DateTime<Utc>) {
        let payload = json!({ "outcome": outcome, "source": source, "token": token });
        if let Err(err) = audit::action(&self.db, "session_signin", None, &payload, now).await {
            tracing::error!(error = %err, "a sign-in attempt can't be audited");
        }
    }

    /// Whether this is the first refused attempt of `source` in the last minute.
    fn first_refusal(&self, source: &str, now: Instant) -> bool {
        let mut refused = self.refused.lock().unwrap_or_else(PoisonError::into_inner);
        if refused.len() > PRUNE_ABOVE {
            refused.retain(|_, at| now.duration_since(*at) < REFUSED_AUDIT_EVERY);
        }
        match refused.get(source) {
            Some(at) if now.duration_since(*at) < REFUSED_AUDIT_EVERY => false,
            _ => {
                refused.insert(source.to_owned(), now);
                true
            }
        }
    }

    /// One `csrf_refused` row for a cookie request the core turned away: whose session, which
    /// route and which half of the check failed, never what `Origin` said.
    async fn record_csrf(&self, caller: &Caller, method: &Method, route: &str, why: &str) {
        let payload = json!({
            "token": caller.token.public_id,
            "method": method.as_str(),
            "route": route,
            "missing": why,
        });
        if let Err(err) = audit::action(&self.db, "csrf_refused", None, &payload, db::now()).await {
            tracing::error!(error = %err, "a refused cookie request can't be audited");
        }
    }

    /// A session for `caller`, and its cookie. The row and the audit row are one transaction: no
    /// session without its trail.
    async fn create(&self, caller: &Caller, source: &str, now: DateTime<Utc>) -> Result<String, CreateError> {
        let mut id = [0_u8; 32];
        getrandom::fill(&mut id)?;
        let digest = Sha256::digest(id).to_vec();
        // never beyond the token's own expiry
        let expires_at = (now + self.config.ttl).min(caller.token.expires_at);
        let (created, expires) = (db::time(now), db::time(expires_at));
        let mut tx = self.db.pool().begin_with("BEGIN IMMEDIATE").await?;
        sqlx::query!(
            "INSERT INTO sessions (id_sha256, token_public_id, user_id, created_at, expires_at)
             VALUES (?, ?, ?, ?, ?)",
            digest,
            caller.token.public_id,
            caller.user_id,
            created,
            expires
        )
        .execute(&mut *tx)
        .await?;
        let payload = json!({
            "outcome": "created",
            "source": source,
            "token": caller.token.public_id,
            "expires_at": expires,
        });
        audit::action_in(&mut tx, "session_signin", None, &payload, now).await?;
        tx.commit().await?;
        let seconds = (expires_at - now).num_seconds().max(1);
        Ok(set_cookie(&URL_SAFE_NO_PAD.encode(id), seconds))
    }
}

/// An address as the sign-in limit counts it.
fn normalize(ip: IpAddr) -> String {
    match ip {
        IpAddr::V4(v4) => v4.to_string(),
        IpAddr::V6(v6) => match v6.to_ipv4_mapped() {
            Some(v4) => v4.to_string(),
            None => format!("{}/64", Ipv6Addr::from(u128::from(v6) & (u128::MAX << 64))),
        },
    }
}

/// A `Set-Cookie` value for the session cookie; `max_age` 0 clears it.
fn set_cookie(value: &str, max_age: i64) -> String {
    format!("{COOKIE_NAME}={value}; Path=/; Secure; HttpOnly; SameSite=Strict; Max-Age={max_age}")
}

/// The SHA-256 of the session id a request's cookie carries, if it carries a well-formed one. The
/// first cookie of the name decides.
fn cookie_id(headers: &HeaderMap) -> Option<[u8; 32]> {
    let found = headers
        .get_all(COOKIE)
        .iter()
        .filter_map(|value| value.to_str().ok())
        .flat_map(|value| value.split(';'))
        .find_map(|pair| pair.trim().strip_prefix(COOKIE_NAME)?.strip_prefix('='))?;
    let id: [u8; 32] = URL_SAFE_NO_PAD.decode(found.trim()).ok()?.try_into().ok()?;
    Some(Sha256::digest(id).into())
}

/// Whether a request comes from the web client's own page: `Origin` is the configured origin and
/// the client's header is there. Err says which half failed.
fn from_the_web_client(config: &SessionConfig, headers: &HeaderMap) -> Result<(), &'static str> {
    let origin = headers.get(ORIGIN).and_then(|value| value.to_str().ok());
    match (&config.public_origin, origin) {
        (Some(expected), Some(sent)) if sent == expected => {}
        _ => return Err("origin"),
    }
    match headers.get(CLIENT_HEADER).and_then(|value| value.to_str().ok()) {
        Some(CLIENT_VALUE) => Ok(()),
        _ => Err("client"),
    }
}

fn changes_state(method: &Method) -> bool {
    matches!(
        *method,
        Method::POST | Method::PUT | Method::PATCH | Method::DELETE
    )
}

/// Turns the session cookie into the request's [`Caller`], for the routes behind it. A request
/// with an `Authorization` header is a Bearer request and passes untouched, whatever cookie it
/// carries; one whose cookie is no session passes too, as if it had none, and the route's own
/// check refuses it with `401` where it needs a caller.
/// A cookie that changes state without the web client's `Origin` and header is turned away with
/// `403`, and the attempt is audited.
pub async fn resolve(State(sessions): State<Sessions>, mut request: Request, next: Next) -> Response {
    if request.headers().contains_key(AUTHORIZATION) {
        return next.run(request).await;
    }
    let Some(id) = cookie_id(request.headers()) else {
        return next.run(request).await;
    };
    let caller = match identity::authenticate_session(&sessions.db, &id, db::now()).await {
        Ok(caller) => caller,
        // a cookie that is no session is as no cookie: a route that needs a caller says 401 itself,
        // and the public ones still answer a browser that holds a stale cookie
        Err(AuthError::Unauthenticated | AuthError::Expired) => return next.run(request).await,
        Err(err) => return ApiError::from(err).into_response(),
    };
    if changes_state(request.method())
        && let Err(why) = from_the_web_client(&sessions.config, request.headers())
    {
        let route = request
            .extensions()
            .get::<MatchedPath>()
            .map_or("unmatched", MatchedPath::as_str)
            .to_owned();
        sessions.record_csrf(&caller, request.method(), &route, why).await;
        return ApiError::not_from_the_web_client().into_response();
    }
    request.extensions_mut().insert(caller);
    next.run(request).await
}

/// Where a sign-in attempt came from, for its handler, once the gate has counted it.
#[derive(Debug, Clone)]
struct Source(String);

/// Before a sign-in is read: the limit per source address, then the check on where it came from.
/// Both refusals are audited; the limit's at most once a minute per source, so that a flood from
/// one address can't fill the disk and stop the audit.
async fn sign_in_gate(State(sessions): State<Sessions>, mut request: Request, next: Next) -> Response {
    let now = db::now();
    let peer = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    let source = sessions.source(peer, request.headers());
    if let Err(wait) = sessions.signins.check(&source, Instant::now()) {
        if sessions.first_refusal(&source, Instant::now()) {
            sessions.record_attempt("rate_limited", &source, None, now).await;
        }
        return ApiError::rate_limited(wait).into_response();
    }
    if from_the_web_client(&sessions.config, request.headers()).is_err() {
        sessions
            .record_attempt("not_from_the_web_client", &source, None, now)
            .await;
        return ApiError::not_from_the_web_client().into_response();
    }
    request.extensions_mut().insert(Source(source));
    next.run(request).await
}

/// `POST /v1/session`: the token is checked like any request, and a session is made for it.
async fn sign_in(
    State(sessions): State<Sessions>,
    Extension(Source(source)): Extension<Source>,
    body: Result<Json<SessionRequest>, JsonRejection>,
) -> Response {
    let now = db::now();
    let Ok(Json(request)) = body else {
        sessions
            .record_attempt("invalid_request", &source, None, now)
            .await;
        return ApiError {
            status: StatusCode::BAD_REQUEST,
            code: ErrorCode::InvalidRequest,
            message: "The request body isn't the JSON this route takes.".to_owned(),
            hint: "Send {\"token\": \"mcy_…\"}.".to_owned(),
            retry_after: None,
        }
        .into_response();
    };
    let text = request.token.expose();
    let claimed = identity::claimed_public_id(text);
    let caller = match identity::authenticate_token(&sessions.db, text, now).await {
        Ok(caller) => caller,
        Err(err) => {
            let outcome = match err {
                AuthError::Expired => "token_expired",
                AuthError::Unauthenticated => "unauthenticated",
                AuthError::Corrupt | AuthError::Db(_) => "error",
            };
            sessions
                .record_attempt(outcome, &source, claimed.as_deref(), now)
                .await;
            return ApiError::from(err).into_response();
        }
    };
    match sessions.create(&caller, &source, now).await {
        Ok(cookie) => no_content_with(&cookie),
        Err(err) => {
            tracing::error!(error = %err, "cannot start a session");
            ApiError::internal().into_response()
        }
    }
}

/// `DELETE /v1/session`: ends the session the cookie names. It changes state with the cookie, so
/// it must come from the web client's own page too.
async fn sign_out(State(sessions): State<Sessions>, headers: HeaderMap) -> Response {
    let now = db::now();
    let Some(id) = cookie_id(&headers) else {
        return ApiError::unauthenticated().into_response();
    };
    let caller = match identity::authenticate_session(&sessions.db, &id, now).await {
        Ok(caller) => caller,
        Err(err) => return ApiError::from(err).into_response(),
    };
    if let Err(why) = from_the_web_client(&sessions.config, &headers) {
        sessions
            .record_csrf(&caller, &Method::DELETE, "/v1/session", why)
            .await;
        return ApiError::not_from_the_web_client().into_response();
    }
    match end(&sessions.db, &id, &caller, now).await {
        Ok(()) => no_content_with(&set_cookie("", 0)),
        Err(err) => {
            tracing::error!(error = %err, "cannot end a session");
            ApiError::internal().into_response()
        }
    }
}

/// Ends one session and writes its `session_ended` row (the token's public id, never the session
/// id) in one transaction: a sign-out that can't be audited ends nothing. A session another request
/// ended in the meantime leaves no second row.
async fn end(db: &Db, id: &[u8; 32], caller: &Caller, now: DateTime<Utc>) -> Result<(), AuditError> {
    let (ended_at, id) = (db::time(now), id.as_slice());
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let ended = sqlx::query!(
        "UPDATE sessions SET revoked_at = ? WHERE id_sha256 = ? AND revoked_at IS NULL",
        ended_at,
        id
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    if ended == 0 {
        return Ok(());
    }
    let payload = json!({ "token": caller.token.public_id });
    audit::action_in(&mut tx, "session_ended", None, &payload, now).await?;
    tx.commit().await?;
    Ok(())
}

fn no_content_with(cookie: &str) -> Response {
    // the value is made of the cookie's own fixed words and base64url: always a valid header
    let cookie = HeaderValue::from_str(cookie).unwrap_or_else(|_| HeaderValue::from_static(""));
    (
        StatusCode::NO_CONTENT,
        [
            (SET_COOKIE, cookie),
            (CACHE_CONTROL, HeaderValue::from_static("no-store")),
        ],
    )
        .into_response()
}

/// `POST` and `DELETE /v1/session`. Serve it with
/// `into_make_service_with_connect_info::<SocketAddr>()`, or every sign-in counts as one source.
pub fn routes(sessions: Sessions) -> Router {
    let sign_in = sign_in
        .layer(middleware::from_fn(limits::body_cap))
        .layer(middleware::from_fn_with_state(sessions.clone(), sign_in_gate));
    Router::new()
        .route("/v1/session", post(sign_in).delete(sign_out))
        .with_state(sessions)
}

/// Ends every session of a token, inside `tx`: revoking the token and its sessions are one step.
pub(crate) async fn end_for_token(
    tx: &mut Transaction<'_, Sqlite>,
    public_id: &str,
    now: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let ended_at = db::time(now);
    Ok(sqlx::query!(
        "UPDATE sessions SET revoked_at = ? WHERE token_public_id = ? AND revoked_at IS NULL",
        ended_at,
        public_id
    )
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

/// Ends every session of a person, inside `tx`: disabling them and their sessions are one step.
pub(crate) async fn end_for_user(
    tx: &mut Transaction<'_, Sqlite>,
    user_id: &str,
    now: DateTime<Utc>,
) -> Result<u64, sqlx::Error> {
    let ended_at = db::time(now);
    Ok(sqlx::query!(
        "UPDATE sessions SET revoked_at = ? WHERE user_id = ? AND revoked_at IS NULL",
        ended_at,
        user_id
    )
    .execute(&mut **tx)
    .await?
    .rows_affected())
}

/// Deletes the sessions that have expired; how many. The core does it once a day.
pub async fn purge_expired(db: &Db, now: DateTime<Utc>) -> Result<u64, sqlx::Error> {
    let at = db::time(now);
    Ok(sqlx::query!("DELETE FROM sessions WHERE expires_at <= ?", at)
        .execute(db.pool())
        .await?
        .rows_affected())
}
