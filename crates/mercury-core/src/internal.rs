//! The internal listener (`/internal/*`, for probes). Before anything else looks at a request,
//! even at its token, the source address must be in `MERCURY_INTERNAL_ALLOW_CIDRS`; any other
//! source gets a bare 403.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;

use axum::Router;
use axum::extract::{ConnectInfo, Request, State};
use axum::http::StatusCode;
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use ipnet::IpNet;

use crate::logging;

/// The source ranges the internal listener serves. Empty refuses every source.
#[derive(Debug, Clone, Default)]
pub struct AllowList(Arc<[IpNet]>);

impl AllowList {
    /// A comma-separated list of CIDR ranges, e.g. `172.30.10.0/24,100.64.0.0/10`.
    pub fn parse(value: Option<&str>) -> Result<Self, String> {
        Self::parse_for("MERCURY_INTERNAL_ALLOW_CIDRS", value)
    }

    /// The same list, read from the variable `variable`, which an error names.
    pub fn parse_for(variable: &str, value: Option<&str>) -> Result<Self, String> {
        let ranges = value
            .unwrap_or_default()
            .split(',')
            .map(str::trim)
            .filter(|range| !range.is_empty())
            .map(|range| {
                range
                    .parse::<IpNet>()
                    .map_err(|_| format!("{variable}: {range:?} is not a range like 172.30.10.0/24"))
            })
            .collect::<Result<Vec<_>, _>>()?;
        Ok(Self(ranges.into()))
    }

    pub fn allows(&self, ip: IpAddr) -> bool {
        // an IPv4 peer of a dual-stack listener arrives as ::ffff:a.b.c.d
        let ip = match ip {
            IpAddr::V6(v6) => v6.to_ipv4_mapped().map_or(ip, IpAddr::V4),
            v4 => v4,
        };
        self.0.iter().any(|range| range.contains(&ip))
    }

    pub fn is_empty(&self) -> bool {
        self.0.is_empty()
    }

    pub fn len(&self) -> usize {
        self.0.len()
    }
}

async fn check_source(State(allow): State<AllowList>, request: Request, next: Next) -> Response {
    let source = request
        .extensions()
        .get::<ConnectInfo<SocketAddr>>()
        .map(|info| info.0.ip());
    match source {
        Some(ip) if allow.allows(ip) => next.run(request).await,
        _ => StatusCode::FORBIDDEN.into_response(),
    }
}

/// The internal API without routes; the source check covers every path, the unknown ones too.
/// Serve it with `into_make_service_with_connect_info::<SocketAddr>()`, or every request is refused.
pub fn router(allow: AllowList) -> Router {
    router_with(allow, Router::new())
}

/// The internal API with `routes`, all behind the source check, which runs before anything else
/// looks at a request.
pub fn router_with(allow: AllowList, routes: Router) -> Router {
    routes
        .fallback(|| async { StatusCode::NOT_FOUND })
        .layer(middleware::from_fn_with_state(allow, check_source))
        .layer(middleware::from_fn_with_state("internal", logging::log_request))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::header::AUTHORIZATION;
    use tower::ServiceExt;

    use super::*;

    fn allow() -> AllowList {
        AllowList::parse(Some("172.30.10.0/24, 100.64.0.0/10")).unwrap()
    }

    async fn call_from(source: Option<&str>, uri: &str, token: Option<&str>) -> StatusCode {
        let mut request = Request::builder().method("POST").uri(uri);
        if let Some(token) = token {
            request = request.header(AUTHORIZATION, format!("Bearer {token}"));
        }
        let mut request = request.body(Body::empty()).unwrap();
        if let Some(source) = source {
            let addr: SocketAddr = source.parse().unwrap();
            request.extensions_mut().insert(ConnectInfo(addr));
        }
        router(allow()).oneshot(request).await.unwrap().status()
    }

    #[tokio::test]
    async fn a_source_outside_the_ranges_gets_403_even_with_a_probe_token() {
        let token = Some("mcy_probedelta_0123456789abcdef0123456789abcdef");
        for source in [
            "172.30.20.5:40000",
            "127.0.0.1:40000",
            "[::1]:40000",
            "10.0.0.7:40000",
        ] {
            let status = call_from(Some(source), "/internal/probes/register", token).await;
            assert_eq!(status, StatusCode::FORBIDDEN, "{source}");
        }
    }

    #[tokio::test]
    async fn an_unknown_source_gets_403() {
        assert_eq!(
            call_from(None, "/internal/probes/register", None).await,
            StatusCode::FORBIDDEN
        );
    }

    #[tokio::test]
    async fn a_source_in_the_ranges_gets_past_the_check() {
        for source in [
            "172.30.10.5:40000",
            "100.100.1.2:40000",
            "[::ffff:172.30.10.9]:40000",
        ] {
            let status = call_from(Some(source), "/internal/probes/register", None).await;
            assert_eq!(
                status,
                StatusCode::NOT_FOUND,
                "{source}: past the check, no route yet"
            );
        }
    }

    #[test]
    fn the_allow_list_parses_comma_separated_ranges() {
        assert_eq!(allow().len(), 2);
        assert!(AllowList::parse(None).unwrap().is_empty());
        assert!(AllowList::parse(Some(" , ")).unwrap().is_empty());
        assert!(AllowList::parse(Some("172.30.10.0/24,everyone")).is_err());
        assert!(AllowList::parse(Some("172.30.10.0")).is_err());
    }

    #[test]
    fn an_empty_list_refuses_every_source() {
        let empty = AllowList::default();
        assert!(!empty.allows("172.30.10.5".parse().unwrap()));
        assert!(!empty.allows("127.0.0.1".parse().unwrap()));
    }
}
