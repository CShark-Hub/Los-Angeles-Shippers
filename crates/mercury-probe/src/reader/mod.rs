//! The code reader: `mercury-probe reader`, the fourth run mode. It takes one request from `serve`,
//! forwards it to its one upstream and returns what the model said and what the call cost. It reads
//! no file, writes nothing to disk, keeps nothing but the day's spend, and logs no body.
//!
//! Its only route out is the egress allowlist of its preset: the network enforces that, and
//! `check.sh` proves it. What this code adds is that it refuses to start with an upstream that
//! would need more than that.

pub mod client;
pub mod hosts;
pub mod upstream;
pub mod wire;

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant, SystemTime};

use axum::Json;
use axum::Router;
use axum::body::Body;
use axum::extract::State;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use chrono::{DateTime, NaiveDate, Utc};
use reqwest::Url;

use upstream::{Key, Upstream};
use wire::{ErrorBody, ErrorCode, MAX_BODY_BYTES, ReadReply, ReadRequest};

/// Where the reader listens when neither `--listen` nor `READER_LISTEN` is set.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:7100";

/// The one value of `READER_ALLOW_OUTSIDE` the reader accepts: the `demo` preset's, for the mock
/// repository.
pub const ALLOW_OUTSIDE: &str = "mock-repository";

/// A day's spend, in US dollars, when `READER_DAILY_USD` isn't set.
pub const DEFAULT_DAILY_USD: &str = "1.00";

/// A line of the reader's log goes here.
pub type Log = Arc<dyn Fn(&str) + Send + Sync>;

/// What the reader is told by its environment.
#[derive(Debug)]
pub struct ReaderConfig {
    pub listen: SocketAddr,
    pub upstream: Url,
    /// Whether the upstream is outside your organisation's network (only the `demo` preset's).
    pub outside: bool,
    pub model: String,
    key: Option<Key>,
    pub daily_usd: f64,
}

impl ReaderConfig {
    /// `READER_UPSTREAM_URL`, `READER_MODEL`, `READER_KEY_FILE`, `READER_DAILY_USD`,
    /// `READER_ALLOW_OUTSIDE`, `READER_LISTEN` (`listen`, when given, wins) and `HTTPS_PROXY`. An
    /// error names a variable and never a value.
    pub fn from_env(
        var: impl Fn(&str) -> Option<String>,
        listen: Option<SocketAddr>,
    ) -> Result<Self, String> {
        let set = |name: &str| var(name).map(|v| v.trim().to_owned()).filter(|v| !v.is_empty());
        let listen = match listen {
            Some(listen) => listen,
            None => crate::listen_addr(&set("READER_LISTEN").unwrap_or_else(|| DEFAULT_LISTEN.to_owned()))
                .map_err(|_| "READER_LISTEN must look like 127.0.0.1:7100".to_owned())?,
        };
        let upstream = set("READER_UPSTREAM_URL").ok_or("READER_UPSTREAM_URL is not set")?;
        let upstream = hosts::parse_url(&upstream)
            .map_err(|problem| format!("READER_UPSTREAM_URL isn't usable: {problem}"))?;
        let allow = set("READER_ALLOW_OUTSIDE");
        if allow.as_deref().is_some_and(|value| value != ALLOW_OUTSIDE) {
            return Err(format!("READER_ALLOW_OUTSIDE can only be {ALLOW_OUTSIDE}"));
        }
        let outside = !hosts::is_private(&upstream);
        if outside {
            if allow.is_none() {
                return Err(
                    "READER_UPSTREAM_URL names a host outside your network, and READER_ALLOW_OUTSIDE isn't set: only the demo preset may send code outside"
                        .to_owned(),
                );
            }
            if upstream.scheme() != "https" {
                return Err("READER_UPSTREAM_URL must be https when the host is outside".to_owned());
            }
            if !["HTTPS_PROXY", "https_proxy"]
                .iter()
                .any(|name| set(name).is_some())
            {
                return Err(
                    "HTTPS_PROXY is not set: an outside upstream is reached only through the reader's egress proxy, never directly"
                        .to_owned(),
                );
            }
        }
        let model = set("READER_MODEL").ok_or("READER_MODEL is not set")?;
        let key = match set("READER_KEY_FILE") {
            Some(path) => {
                let key = std::fs::read_to_string(path)
                    .map_err(|err| format!("cannot read READER_KEY_FILE: {err}"))?;
                let key = key.trim();
                if key.is_empty() {
                    return Err("READER_KEY_FILE is empty".to_owned());
                }
                Some(Key::new(key.to_owned()))
            }
            None if outside => {
                return Err("READER_KEY_FILE is not set: an outside upstream needs its key".to_owned());
            }
            None => None,
        };
        let daily = set("READER_DAILY_USD").unwrap_or_else(|| DEFAULT_DAILY_USD.to_owned());
        let daily_usd =
            decimal(&daily).ok_or("READER_DAILY_USD must be a non-negative decimal, like 1.00")?;
        Ok(Self {
            listen,
            upstream,
            outside,
            model,
            key,
            daily_usd,
        })
    }
}

/// A non-negative decimal written with digits and at most one point.
fn decimal(value: &str) -> Option<f64> {
    let digits = value.chars().filter(char::is_ascii_digit).count();
    let points = value.chars().filter(|c| *c == '.').count();
    if digits == 0 || points > 1 || value.chars().any(|c| !c.is_ascii_digit() && c != '.') {
        return None;
    }
    value.parse::<f64>().ok().filter(|v| v.is_finite())
}

/// The day's spend, per UTC day, in memory.
#[derive(Debug)]
struct Spend {
    day: NaiveDate,
    usd: f64,
}

/// The reader's state: its upstream, its day's spend and its log.
pub struct Reader {
    model: String,
    daily_usd: f64,
    upstream: Upstream,
    spend: Mutex<Spend>,
    today: Box<dyn Fn() -> NaiveDate + Send + Sync>,
    log: Log,
}

impl Reader {
    pub fn new(config: ReaderConfig, log: Log) -> Result<Self, String> {
        Self::with_timeout(config, upstream::UPSTREAM_TIMEOUT, log)
    }

    /// The same with another wait for the upstream, for the tests.
    pub fn with_timeout(config: ReaderConfig, timeout: Duration, log: Log) -> Result<Self, String> {
        let upstream = Upstream::new(config.upstream, config.outside, config.key, timeout)?;
        Ok(Self {
            model: config.model,
            daily_usd: config.daily_usd,
            upstream,
            spend: Mutex::new(Spend {
                day: utc_today(),
                usd: 0.0,
            }),
            today: Box::new(utc_today),
            log,
        })
    }

    /// The same with another clock, for the tests.
    #[must_use]
    pub fn with_clock(mut self, today: impl Fn() -> NaiveDate + Send + Sync + 'static) -> Self {
        self.today = Box::new(today);
        self
    }

    fn spend_today(&self) -> f64 {
        let mut spend = self.spend.lock().unwrap_or_else(PoisonError::into_inner);
        roll(&mut spend, (self.today)());
        spend.usd
    }

    /// Adds an answered request's cost to the day's and gives the day's total.
    fn charge(&self, cost: f64) -> f64 {
        let mut spend = self.spend.lock().unwrap_or_else(PoisonError::into_inner);
        roll(&mut spend, (self.today)());
        spend.usd += cost;
        spend.usd
    }

    fn say(&self, line: &str) {
        (self.log)(line);
    }
}

fn roll(spend: &mut Spend, today: NaiveDate) {
    if spend.day != today {
        spend.day = today;
        spend.usd = 0.0;
    }
}

fn utc_today() -> NaiveDate {
    DateTime::<Utc>::from(SystemTime::now()).date_naive()
}

/// `GET /health` and `POST /read`.
pub fn app(reader: Arc<Reader>) -> Router {
    crate::router().merge(Router::new().route("/read", post(read)).with_state(reader))
}

fn failure(code: ErrorCode) -> Response {
    let status = StatusCode::from_u16(code.status()).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    (status, Json(ErrorBody { error: code })).into_response()
}

async fn read(State(reader): State<Arc<Reader>>, headers: HeaderMap, body: Body) -> Response {
    let started = Instant::now();
    let announced = headers
        .get(header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<usize>().ok());
    // over the cap: refused before a byte of it is read
    let bytes = if announced.is_some_and(|length| length > MAX_BODY_BYTES) {
        None
    } else {
        axum::body::to_bytes(body, MAX_BODY_BYTES).await.ok()
    };
    let Some(bytes) = bytes else {
        return finish(
            &reader,
            started,
            ErrorCode::TooLarge,
            announced.unwrap_or(0),
            0,
            None,
            "over the cap",
        );
    };
    let size_in = bytes.len();
    let Some(request) = serde_json::from_slice::<ReadRequest>(&bytes)
        .ok()
        .filter(|request| request.is_well_formed() && request.model == reader.model)
    else {
        return finish(
            &reader,
            started,
            ErrorCode::InvalidRequest,
            size_in,
            0,
            None,
            "refused",
        );
    };
    if reader.spend_today() >= reader.daily_usd {
        return finish(
            &reader,
            started,
            ErrorCode::DailyCap,
            size_in,
            0,
            Some(reader.spend_today()),
            "the day's cap is spent",
        );
    }
    match reader.upstream.ask(&request).await {
        Ok(answer) => {
            let total = reader.charge(answer.cost_usd);
            let reply = ReadReply {
                content: answer.content,
                cost_usd: answer.cost_usd,
            };
            let out = serde_json::to_vec(&reply).map_or(0, |bytes| bytes.len());
            reader.say(&format!(
                "read: 200, {size_in} bytes in, {out} bytes out, ${:.4}, {:.1} s, the day's total ${total:.4}",
                answer.cost_usd,
                started.elapsed().as_secs_f64(),
            ));
            Json(reply).into_response()
        }
        Err(failed) => finish(
            &reader,
            started,
            failed.code,
            size_in,
            0,
            Some(reader.spend_today()),
            &failed.kind,
        ),
    }
}

/// Logs one line for a request that gave no answer, and answers it. The line holds the status, the
/// code and a few words of the kind; never a body.
fn finish(
    reader: &Reader,
    started: Instant,
    code: ErrorCode,
    size_in: usize,
    size_out: usize,
    total: Option<f64>,
    kind: &str,
) -> Response {
    let day = total.map_or_else(String::new, |total| format!(", the day's total ${total:.4}"));
    reader.say(&format!(
        "read: {} {} ({kind}), {size_in} bytes in, {size_out} bytes out, {:.1} s{day}",
        code.status(),
        code.as_str(),
        started.elapsed().as_secs_f64(),
    ));
    failure(code)
}

/// What `reader` says when it starts: the address, the upstream's host (never its path or query),
/// whether it is private or outside, the model and the cap.
pub fn start_line(config: &ReaderConfig) -> String {
    format!(
        "mercury-probe {} (reader): POST /read and the health check, on http://{}; upstream {} ({}), model {}, a cap of ${:.2} a day, spent in memory only",
        env!("CARGO_PKG_VERSION"),
        config.listen,
        config.upstream.host_str().unwrap_or("?"),
        if config.outside { "outside" } else { "private" },
        config.model,
        config.daily_usd,
    )
}
