//! The limits the public routes mount as layers: a rate limit per token, and a cap on the request
//! body. Both answer with the API's error body.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use axum::body::Body;
use axum::extract::{Request, State};
use axum::http::header::{AUTHORIZATION, CONTENT_LENGTH};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use http_body_util::{BodyExt, LengthLimitError, Limited};

use crate::db::{self, Db};
use crate::error::ApiError;
use crate::identity::{self, Caller};

/// The largest request body, in bytes.
pub const MAX_BODY: usize = 64 * 1024;

/// How many requests a token may make: `burst` at once, then `per_hour` spread over the hour.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Rate {
    pub per_hour: u32,
    pub burst: u32,
}

impl Rate {
    /// The default for asking questions.
    pub const QUESTIONS: Rate = Rate {
        per_hour: 30,
        burst: 5,
    };
}

impl Default for Rate {
    fn default() -> Self {
        Self::QUESTIONS
    }
}

/// When a key's memory is pruned of keys that are back to a full allowance.
const PRUNE_ABOVE: usize = 1024;

/// A rate limit per key (a token's public id), as a generic cell rate: a request is allowed
/// while the key's next slot lies at most `burst - 1` intervals ahead of now.
#[derive(Debug, Clone)]
pub struct RateLimiter {
    interval: Duration,
    tolerance: Duration,
    next_slot: Arc<Mutex<HashMap<String, Instant>>>,
}

impl RateLimiter {
    pub fn new(rate: Rate) -> Self {
        let interval = Duration::from_secs(3600) / rate.per_hour.max(1);
        Self {
            interval,
            tolerance: interval * (rate.burst.max(1) - 1),
            next_slot: Arc::default(),
        }
    }

    /// Spends one request of `key` at `now`. `Err` says how long until the next one is allowed.
    pub fn check(&self, key: &str, now: Instant) -> Result<(), Duration> {
        let mut next_slot = self.next_slot.lock().unwrap_or_else(PoisonError::into_inner);
        let slot = next_slot.get(key).copied().unwrap_or(now).max(now);
        let ahead = slot.duration_since(now);
        if ahead > self.tolerance {
            return Err(ahead - self.tolerance);
        }
        if next_slot.len() > PRUNE_ABOVE {
            next_slot.retain(|_, slot| *slot > now);
        }
        next_slot.insert(key.to_owned(), slot + self.interval);
        Ok(())
    }
}

/// The state of the [`per_token`] layer.
#[derive(Debug, Clone)]
pub struct PerToken {
    pub db: Db,
    pub limiter: RateLimiter,
}

/// Authenticates the caller and spends one request of their token, whether it came as a Bearer
/// token or as the web client's session. Over the limit, `429 rate_limited` with `Retry-After`; a
/// request without a valid token never reaches the limiter. The caller then rides in the request's
/// extensions, where the `Caller` extractor takes it. A caller the session layer has already put
/// there is not looked up again.
pub async fn per_token(State(state): State<PerToken>, mut request: Request, next: Next) -> Response {
    let caller = match request.extensions().get::<Caller>().cloned() {
        Some(caller) => caller,
        None => {
            let header = request.headers().get(AUTHORIZATION);
            match identity::authenticate(&state.db, header, db::now()).await {
                Ok(caller) => caller,
                Err(err) => return ApiError::from(err).into_response(),
            }
        }
    };
    if let Err(wait) = state.limiter.check(&caller.token.public_id, Instant::now()) {
        return ApiError::rate_limited(wait).into_response();
    }
    request.extensions_mut().insert(caller);
    next.run(request).await
}

/// Refuses a body over [`MAX_BODY`] with `413 invalid_request` before a handler reads it: by its
/// `Content-Length` when it declares one, else by reading at most the limit.
pub async fn body_cap(request: Request, next: Next) -> Response {
    let declared = request
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > MAX_BODY as u64) {
        return ApiError::body_too_large().into_response();
    }
    let (parts, body) = request.into_parts();
    let bytes = match Limited::new(body, MAX_BODY).collect().await {
        Ok(collected) => collected.to_bytes(),
        Err(err) if err.downcast_ref::<LengthLimitError>().is_some() => {
            return ApiError::body_too_large().into_response();
        }
        Err(_) => return ApiError::unreadable_body().into_response(),
    };
    next.run(Request::from_parts(parts, Body::from(bytes))).await
}

#[cfg(test)]
mod tests {
    use axum::Router;
    use axum::http::header::RETRY_AFTER;
    use axum::http::{Method, StatusCode};
    use axum::middleware;
    use axum::routing::{get, post};
    use tower::ServiceExt;

    use super::*;
    use crate::db::testing;
    use crate::identity::{Caller, role_name};

    const HOUR: Duration = Duration::from_secs(3600);

    #[test]
    fn a_burst_of_five_then_one_every_two_minutes() {
        let limiter = RateLimiter::new(Rate::QUESTIONS);
        let start = Instant::now();
        for n in 1..=5 {
            assert_eq!(limiter.check("k3Xq9AbCdEfG", start), Ok(()), "request {n}");
        }
        assert_eq!(
            limiter.check("k3Xq9AbCdEfG", start),
            Err(Duration::from_secs(120)),
            "the sixth waits for the next slot"
        );
        let later = start + Duration::from_secs(119);
        assert_eq!(limiter.check("k3Xq9AbCdEfG", later), Err(Duration::from_secs(1)));
        let slot = start + Duration::from_secs(120);
        assert_eq!(limiter.check("k3Xq9AbCdEfG", slot), Ok(()));
        assert!(limiter.check("k3Xq9AbCdEfG", slot).is_err());
    }

    #[test]
    fn thirty_an_hour_at_a_steady_pace() {
        let limiter = RateLimiter::new(Rate::QUESTIONS);
        let start = Instant::now();
        let allowed = (0..120)
            .map(|minute| start + Duration::from_secs(60) * minute)
            .filter(|&at| limiter.check("k3Xq9AbCdEfG", at).is_ok())
            .count();
        // two hours at one try a minute: the burst, then one every two minutes
        assert_eq!(allowed, 5 + 59);
        let after_a_quiet_hour = start + 3 * HOUR;
        for _ in 0..5 {
            assert!(limiter.check("k3Xq9AbCdEfG", after_a_quiet_hour).is_ok());
        }
    }

    #[test]
    fn each_token_has_its_own_allowance() {
        let limiter = RateLimiter::new(Rate {
            per_hour: 60,
            burst: 1,
        });
        let now = Instant::now();
        assert!(limiter.check("aaaaaaaaaaaa", now).is_ok());
        assert_eq!(limiter.check("aaaaaaaaaaaa", now), Err(Duration::from_secs(60)));
        assert!(limiter.check("bbbbbbbbbbbb", now).is_ok());
    }

    async fn whoami(caller: Caller) -> String {
        role_name(caller.role).to_owned()
    }

    fn limited(db: &Db) -> Router {
        let state = PerToken {
            db: db.clone(),
            limiter: RateLimiter::new(Rate::QUESTIONS),
        };
        Router::new()
            .route("/ask", get(whoami))
            .route_layer(middleware::from_fn_with_state(state, per_token))
            .with_state(db.clone())
    }

    async fn ask(app: &Router, token: &str) -> Response {
        let request = Request::builder()
            .uri("/ask")
            .header(AUTHORIZATION, format!("Bearer {token}"))
            .body(Body::empty())
            .unwrap();
        app.clone().oneshot(request).await.unwrap()
    }

    async fn code(response: Response) -> String {
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let json: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
        json["error"]["code"].as_str().unwrap().to_owned()
    }

    #[tokio::test]
    async fn the_sixth_request_in_a_burst_gets_429_with_retry_after() {
        let (_dir, db) = testing::db().await;
        let soon = db::now() + chrono::TimeDelta::days(1);
        let token = crate::identity::testing::issue(&db, "pm@example.com", soon).await;
        let other = crate::identity::testing::issue(&db, "lead@example.com", soon).await;
        let app = limited(&db);
        for n in 1..=5 {
            assert_eq!(ask(&app, &token).await.status(), StatusCode::OK, "request {n}");
        }
        let refused = ask(&app, &token).await;
        assert_eq!(refused.status(), StatusCode::TOO_MANY_REQUESTS);
        let retry_after: u64 = refused.headers()[RETRY_AFTER].to_str().unwrap().parse().unwrap();
        assert!((119..=120).contains(&retry_after), "{retry_after}");
        assert_eq!(code(refused).await, "rate_limited");
        assert_eq!(ask(&app, &other).await.status(), StatusCode::OK, "another token");
    }

    #[tokio::test]
    async fn a_request_without_a_valid_token_is_refused_before_the_limiter() {
        let (_dir, db) = testing::db().await;
        let app = limited(&db);
        for _ in 0..7 {
            let response = ask(&app, "mcy_nothing").await;
            assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
            assert_eq!(code(response).await, "unauthenticated");
        }
    }

    async fn length(body: axum::body::Bytes) -> String {
        body.len().to_string()
    }

    async fn send(body: Body, declared: Option<u64>) -> Response {
        let app = Router::new()
            .route("/echo", post(length))
            .layer(middleware::from_fn(body_cap));
        let mut request = Request::builder().method(Method::POST).uri("/echo");
        if let Some(declared) = declared {
            request = request.header(CONTENT_LENGTH, declared);
        }
        app.oneshot(request.body(body).unwrap()).await.unwrap()
    }

    #[tokio::test]
    async fn a_body_of_64_kb_passes_and_one_byte_more_is_refused() {
        let full = send(Body::from(vec![b'a'; MAX_BODY]), Some(MAX_BODY as u64)).await;
        assert_eq!(full.status(), StatusCode::OK);
        let bytes = full.into_body().collect().await.unwrap().to_bytes();
        assert_eq!(bytes, MAX_BODY.to_string().as_bytes());

        let over = send(Body::from(vec![b'a'; MAX_BODY + 1]), None).await;
        assert_eq!(over.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(code(over).await, "invalid_request");
    }

    #[tokio::test]
    async fn a_declared_length_over_the_cap_is_refused_unread() {
        let response = send(Body::empty(), Some(10 * 1024 * 1024)).await;
        assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
        assert_eq!(code(response).await, "invalid_request");
    }
}
