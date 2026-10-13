//! Structured JSON logs on stderr. A request is logged by its listener, method, route pattern,
//! status and duration only: never a header, a body, a query or the raw path, so tokens,
//! question text, facts and answers have no way into a log line. The MCP library's own lines stay
//! at `warn` and above whatever `RUST_LOG` says, since below it they carry what a request held.

use std::time::Instant;

use axum::extract::{MatchedPath, Request, State};
use axum::middleware::Next;
use axum::response::Response;
use tracing::{Level, Metadata, Subscriber};
use tracing_subscriber::EnvFilter;
use tracing_subscriber::filter::{FilterFn, filter_fn};
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;

/// The filter when `RUST_LOG` is unset: `info`, without the MCP library's own lines about each
/// request at `/mcp`, so a request stays one line.
pub const DEFAULT_FILTER: &str = "rmcp=warn,info";

/// The MCP library, whose lines below [`LIBRARY_LEVEL`] are never written: below it, the library
/// logs the messages it reads and writes, a tool call's arguments and its result among them.
const LIBRARY: &str = "rmcp";
const LIBRARY_LEVEL: Level = Level::WARN;

/// The filter from `RUST_LOG`'s value, and whether that value was refused. Unset or refused, it is
/// [`DEFAULT_FILTER`]: a typo never turns the logs off.
pub fn filter(value: Option<&str>) -> (EnvFilter, bool) {
    match value.filter(|value| !value.trim().is_empty()) {
        None => (EnvFilter::new(DEFAULT_FILTER), false),
        Some(value) => match EnvFilter::try_new(value) {
            Ok(filter) => (filter, false),
            Err(_) => (EnvFilter::new(DEFAULT_FILTER), true),
        },
    }
}

/// The subscriber the core runs with, writing to `writer`. `RUST_LOG` overrides the filter.
pub fn subscriber<W>(writer: W) -> impl Subscriber + Send + Sync
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    with_filter(writer, filter(std::env::var("RUST_LOG").ok().as_deref()).0)
}

/// The subscriber with `filter`, under the cap on the MCP library's lines, which no filter lifts.
fn with_filter<W>(writer: W, filter: EnvFilter) -> impl Subscriber + Send + Sync
where
    W: for<'a> MakeWriter<'a> + Send + Sync + 'static,
{
    tracing_subscriber::fmt()
        .json()
        .with_env_filter(filter)
        .with_writer(writer)
        .finish()
        .with(library_cap())
}

/// Turns off the MCP library's lines below [`LIBRARY_LEVEL`], before any filter is asked: a
/// `RUST_LOG` of `debug`, `rmcp=trace` or a directive for one of its modules can't bring them back.
fn library_cap() -> FilterFn<impl Fn(&Metadata<'_>) -> bool> {
    filter_fn(|metadata| !is_library(metadata.target()) || *metadata.level() <= LIBRARY_LEVEL)
}

/// Whether `target` is the MCP library or one of its modules.
fn is_library(target: &str) -> bool {
    target
        .strip_prefix(LIBRARY)
        .is_some_and(|rest| rest.is_empty() || rest.starts_with("::"))
}

/// Installs the subscriber for the whole process, on stderr. A `RUST_LOG` that isn't a filter
/// falls back to [`DEFAULT_FILTER`], with a warning as the first line.
pub fn init() -> Result<(), String> {
    let (filter, refused) = filter(std::env::var("RUST_LOG").ok().as_deref());
    tracing::subscriber::set_global_default(with_filter(std::io::stderr, filter))
        .map_err(|err| format!("cannot set up logging: {err}"))?;
    if refused {
        tracing::warn!("RUST_LOG is not a valid filter; logging at {DEFAULT_FILTER}");
    }
    Ok(())
}

/// Logs one line per request, after the response. An unrouted path is logged as `unmatched`.
pub async fn log_request(State(listener): State<&'static str>, request: Request, next: Next) -> Response {
    let method = request.method().clone();
    let route = request
        .extensions()
        .get::<MatchedPath>()
        .map_or_else(|| "unmatched".to_owned(), |path| path.as_str().to_owned());
    let started = Instant::now();
    let response = next.run(request).await;
    tracing::info!(
        listener,
        method = %method,
        route,
        status = response.status().as_u16(),
        ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        "request"
    );
    response
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_bad_rust_log_falls_back_to_info() {
        let (bad, refused) = filter(Some("core=loud"));
        assert!(refused);
        assert_eq!(bad.to_string(), DEFAULT_FILTER);
        let (good, refused) = filter(Some("mercury_core=debug"));
        assert!(!refused);
        assert_eq!(good.to_string(), "mercury_core=debug");
        assert!(!filter(None).1 && !filter(Some(" ")).1, "unset is not refused");
    }

    /// Whatever `RUST_LOG` asks for, the MCP library logs nothing below `warn`, its modules and its
    /// spans included, while the core's own lines follow the filter.
    #[test]
    fn the_mcp_librarys_lines_stay_at_warn_whatever_rust_log_says() {
        for rust_log in [
            None,
            Some("debug"),
            Some("trace"),
            Some("rmcp=trace,debug"),
            Some("rmcp::transport::streamable_http_server=trace,debug"),
            Some("[streamable_http_session]=trace,debug"),
        ] {
            let (filter, refused) = filter(rust_log);
            assert!(!refused, "{rust_log:?}");
            let subscriber = with_filter(std::io::sink, filter);
            tracing::subscriber::with_default(subscriber, || {
                assert!(!tracing::enabled!(target: "rmcp", Level::INFO), "{rust_log:?}");
                assert!(
                    !tracing::enabled!(target: "rmcp::service", Level::DEBUG),
                    "{rust_log:?}"
                );
                assert!(
                    !tracing::enabled!(
                        target: "rmcp::transport::streamable_http_server::tower",
                        Level::TRACE
                    ),
                    "{rust_log:?}"
                );
                assert!(
                    !tracing::span_enabled!(target: "rmcp::service", Level::INFO),
                    "{rust_log:?}"
                );
                assert!(
                    tracing::enabled!(target: "rmcp::service", Level::WARN),
                    "{rust_log:?}"
                );
                assert!(tracing::enabled!(target: "rmcp", Level::ERROR), "{rust_log:?}");
                // the core follows the filter: debug when asked, info by default
                let debug = rust_log.is_some();
                assert_eq!(
                    tracing::enabled!(target: "mercury_core::mcp", Level::DEBUG),
                    debug,
                    "{rust_log:?}"
                );
                assert!(
                    tracing::enabled!(target: "mercury_core::mcp", Level::INFO),
                    "{rust_log:?}"
                );
                // a crate whose name only begins like the library's is not the library
                assert_eq!(
                    tracing::enabled!(target: "rmcp_like", Level::DEBUG),
                    debug,
                    "{rust_log:?}"
                );
            });
        }
    }
}
