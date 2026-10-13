//! `POST /internal/probes/register`: a probe's registration and heartbeat, on the internal
//! listener behind its source check. The probe's token is checked before its body is read; the
//! answers carry no body, since the probe logs only their status.

use axum::Router;
use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::AUTHORIZATION;
use axum::http::{HeaderMap, StatusCode};
use axum::middleware;
use axum::routing::post;
use mercury_schema::api::ProbeRegistration;

use super::{Probes, Recorded};
use crate::db;
use crate::limits;

/// The registration route, for [`crate::internal::router_with`].
pub fn routes(probes: Probes) -> Router {
    Router::new()
        .route("/internal/probes/register", post(register))
        .layer(middleware::from_fn(limits::body_cap))
        .with_state(probes)
}

fn bearer(headers: &HeaderMap) -> Option<&str> {
    let (scheme, token) = headers.get(AUTHORIZATION)?.to_str().ok()?.split_once(' ')?;
    scheme.eq_ignore_ascii_case("bearer").then(|| token.trim())
}

/// `202` when the probe is recorded and its tools are checked next; `409` when it is quarantined;
/// `401` without a known probe token; `403` for another project's registration; `400` for a body
/// that isn't a registration.
async fn register(State(probes): State<Probes>, headers: HeaderMap, body: Bytes) -> StatusCode {
    let Some(project) = bearer(&headers).and_then(|token| probes.project_of(token)) else {
        return StatusCode::UNAUTHORIZED;
    };
    let Ok(registration) = serde_json::from_slice::<ProbeRegistration>(&body) else {
        return StatusCode::BAD_REQUEST;
    };
    if registration.project.as_str() != project {
        return StatusCode::FORBIDDEN;
    }
    match probes.record(&registration, db::now()).await {
        Ok(Recorded::ToCheck) => {
            let (probes, project) = (probes.clone(), project.to_owned());
            // the check calls the probe back; the probe's request needn't wait for it
            tokio::spawn(async move {
                if let Err(err) = probes.check(&project).await {
                    tracing::warn!(project, error = %err, "a probe's check failed");
                }
            });
            StatusCode::ACCEPTED
        }
        Ok(Recorded::Quarantined(_)) => StatusCode::CONFLICT,
        Err(err) => {
            tracing::error!(project, error = %err, "cannot record a registration");
            StatusCode::INTERNAL_SERVER_ERROR
        }
    }
}
