//! The probe next to a repository. `sync` reads the repository through its host's API and writes
//! `/meta`; `serve` indexes what `sync` wrote and answers MCP tools with facts; `dev` does both on a
//! local folder with its metadata beside it. `healthcheck`, `reach` and `egress` are the network
//! tools its images run (`net`).

pub mod access;
pub mod aggregates;
pub mod ask_code;
pub mod conformance;
pub mod dev;
pub mod facts;
pub mod index;
pub mod judging;
pub mod mcp;
pub mod meta;
pub mod net;
pub mod probe;
pub mod reader;
pub mod registration;
pub mod rules;
pub mod serve;
pub mod sync;
pub mod tools;

use std::ffi::OsString;
use std::net::SocketAddr;
use std::path::PathBuf;

use axum::{Json, Router, routing::get};
use mercury_schema::AccessMode;
use serde::Serialize;

/// Where the probe listens when neither `--listen` nor `PROBE_LISTEN` is set.
pub const DEFAULT_LISTEN: &str = "127.0.0.1:7000";

pub const USAGE: &str =
    "usage: mercury-probe dev --repo DIR --meta DIR [--listen ADDR] [--access code|metadata]
       mercury-probe serve|sync|reader [--listen ADDR]
       mercury-probe healthcheck PORT
       mercury-probe reach http://HOST[:PORT][/PATH]|https://HOST[:PORT]|HOST:PORT [--timeout SECS]
       mercury-probe egress --listen ADDR --allow FILE [--port N]...";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Dev,
    Serve,
    Sync,
    /// The code reader: forwards one request to its one upstream (`code_model`).
    Reader,
}

impl Mode {
    pub fn as_str(self) -> &'static str {
        match self {
            Mode::Dev => "dev",
            Mode::Serve => "serve",
            Mode::Sync => "sync",
            Mode::Reader => "reader",
        }
    }
}

/// The command line, after the program name.
#[derive(Debug, PartialEq, Eq)]
pub struct Args {
    pub mode: Mode,
    pub listen: Option<SocketAddr>,
    pub repo: Option<PathBuf>,
    pub meta: Option<PathBuf>,
    /// `dev` only: the access mode the git host token would allow.
    pub access: Option<AccessMode>,
}

/// What the command line asks for: one of the probe's modes, or one of its network tools.
#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    /// `dev`, `serve`, `sync` or `reader`.
    Run(Args),
    /// `healthcheck PORT`: the port to ask for `/health` on the loopback.
    Healthcheck(u16),
    /// `reach TARGET [--timeout SECS]`.
    Reach(net::reach::ReachArgs),
    /// `egress --listen ADDR --allow FILE [--port N]...`.
    Egress(net::egress::EgressArgs),
}

/// The command line as the system gives it: an argument that isn't UTF-8 is refused, not a panic.
pub fn parse_os_command(args: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    parse_command(utf8(args)?)
}

pub fn parse_command(args: impl IntoIterator<Item = String>) -> Result<Command, String> {
    let mut args = args.into_iter().peekable();
    // the tools first; anything else is a mode, or no mode at all
    match args.peek().map(String::as_str) {
        Some("healthcheck") => net::healthcheck::parse_args(args.skip(1)).map(Command::Healthcheck),
        Some("reach") => net::reach::parse_args(args.skip(1)).map(Command::Reach),
        Some("egress") => net::egress::parse_args(args.skip(1)).map(Command::Egress),
        _ => parse_args(args).map(Command::Run),
    }
}

/// One of the probe's modes, from the command line as the system gives it.
pub fn parse_os_args(args: impl IntoIterator<Item = OsString>) -> Result<Args, String> {
    parse_args(utf8(args)?)
}

fn utf8(args: impl IntoIterator<Item = OsString>) -> Result<Vec<String>, String> {
    args.into_iter()
        .map(|arg| {
            arg.into_string()
                .map_err(|_| "an argument isn't valid UTF-8".to_owned())
        })
        .collect()
}

pub fn parse_args(args: impl IntoIterator<Item = String>) -> Result<Args, String> {
    let mut args = args.into_iter();
    let mode = match args.next().as_deref() {
        Some("dev") => Mode::Dev,
        Some("serve") => Mode::Serve,
        Some("sync") => Mode::Sync,
        Some("reader") => Mode::Reader,
        Some(other) => return Err(format!("unknown mode {other:?}")),
        None => return Err("no mode given".to_owned()),
    };
    let mut parsed = Args {
        mode,
        listen: None,
        repo: None,
        meta: None,
        access: None,
    };
    while let Some(flag) = args.next() {
        let known = matches!(flag.as_str(), "--listen" | "--repo" | "--meta" | "--access");
        let value = match (known, args.next()) {
            (false, _) => return Err(format!("unknown option {flag:?}")),
            (true, None) => return Err(format!("{flag} needs a value")),
            (true, Some(value)) => value,
        };
        match flag.as_str() {
            "--listen" => parsed.listen = Some(listen_addr(&value)?),
            "--repo" => parsed.repo = Some(value.into()),
            "--meta" => parsed.meta = Some(value.into()),
            _ => parsed.access = Some(access_mode(&value)?),
        }
    }
    match mode {
        Mode::Dev if parsed.repo.is_none() || parsed.meta.is_none() => {
            Err("dev needs --repo and --meta".to_owned())
        }
        Mode::Reader if parsed.access.is_some() || parsed.repo.is_some() || parsed.meta.is_some() => {
            Err("reader takes --listen alone: it reads no folder and has no access mode".to_owned())
        }
        Mode::Serve | Mode::Sync if parsed.access.is_some() => {
            Err("--access is for dev only; serve and sync work the mode out themselves".to_owned())
        }
        Mode::Serve | Mode::Sync if parsed.repo.is_some() || parsed.meta.is_some() => {
            Err("--repo and --meta are for dev only; serve and sync use /repo and /meta".to_owned())
        }
        _ => Ok(parsed),
    }
}

fn access_mode(value: &str) -> Result<AccessMode, String> {
    match value {
        "code" => Ok(AccessMode::Code),
        "metadata" => Ok(AccessMode::Metadata),
        _ => Err(format!("--access must be code or metadata, not {value:?}")),
    }
}

pub fn listen_addr(value: &str) -> Result<SocketAddr, String> {
    value
        .parse()
        .map_err(|_| format!("the listen address must look like {DEFAULT_LISTEN}, not {value:?}"))
}

/// The body of `GET /health`.
#[derive(Debug, Serialize)]
struct Health {
    status: &'static str,
    version: &'static str,
}

async fn health() -> Json<Health> {
    Json(Health {
        status: "ok",
        version: env!("CARGO_PKG_VERSION"),
    })
}

pub fn router() -> Router {
    Router::new().route("/health", get(health))
}

#[cfg(test)]
mod tests {
    use axum::body::Body;
    use axum::http::{Request, StatusCode};
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    use super::*;

    fn parse(line: &str) -> Result<Args, String> {
        parse_args(line.split_whitespace().map(str::to_owned))
    }

    async fn get_path(uri: &str) -> (StatusCode, String) {
        let request = Request::builder().uri(uri).body(Body::empty()).unwrap();
        let response = router().oneshot(request).await.unwrap();
        let status = response.status();
        let body = response.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8(body.to_vec()).unwrap())
    }

    #[test]
    fn dev_mode_takes_the_repository_the_metadata_and_the_address() {
        let args = parse("dev --repo tests/r --meta tests/r/meta --listen 127.0.0.1:7100").unwrap();
        assert_eq!(args.mode, Mode::Dev);
        assert_eq!(args.repo, Some(PathBuf::from("tests/r")));
        assert_eq!(args.meta, Some(PathBuf::from("tests/r/meta")));
        assert_eq!(args.listen, Some("127.0.0.1:7100".parse().unwrap()));
        assert_eq!(args.access, None);
    }

    #[test]
    fn dev_mode_takes_code_or_metadata_access() {
        let dev = "dev --repo r --meta m --access";
        assert_eq!(
            parse(&format!("{dev} code")).unwrap().access,
            Some(AccessMode::Code)
        );
        assert_eq!(
            parse(&format!("{dev} metadata")).unwrap().access,
            Some(AccessMode::Metadata)
        );
        assert!(parse(&format!("{dev} code_model")).is_err());
        assert!(parse(&format!("{dev} everything")).is_err());
    }

    #[test]
    fn dev_mode_needs_the_repository_and_the_metadata() {
        assert!(parse("dev").is_err());
        assert!(parse("dev --repo r").is_err());
        assert!(parse("dev --meta m").is_err());
    }

    #[test]
    fn access_is_for_dev_mode_only() {
        assert!(parse("serve --access metadata").is_err());
        assert!(parse("sync --access code").is_err());
    }

    #[test]
    fn the_reader_takes_the_listen_address_alone() {
        assert_eq!(parse("reader").unwrap().mode, Mode::Reader);
        let args = parse("reader --listen 0.0.0.0:7100").unwrap();
        assert_eq!(args.listen, Some("0.0.0.0:7100".parse().unwrap()));
        for line in ["reader --repo r", "reader --meta m", "reader --access code"] {
            assert!(parse(line).is_err(), "{line}");
        }
        assert_eq!(Mode::Reader.as_str(), "reader");
    }

    #[test]
    fn repo_and_meta_are_for_dev_mode_only() {
        for line in ["serve --repo r", "serve --meta m", "sync --repo r --meta m"] {
            assert!(parse(line).is_err(), "{line}");
        }
        assert!(parse("serve --listen 127.0.0.1:7000").is_ok());
    }

    #[cfg(unix)]
    fn not_utf8() -> OsString {
        use std::os::unix::ffi::OsStringExt;
        OsString::from_vec(vec![b'/', 0xff])
    }

    #[cfg(windows)]
    fn not_utf8() -> OsString {
        use std::os::windows::ffi::OsStringExt;
        OsString::from_wide(&[u16::from(b'/'), 0xD800])
    }

    #[test]
    fn an_argument_that_is_not_utf8_is_refused() {
        let args = ["dev", "--repo"]
            .map(OsString::from)
            .into_iter()
            .chain([not_utf8()])
            .chain(["--meta", "m"].map(OsString::from));
        assert_eq!(parse_os_args(args).unwrap_err(), "an argument isn't valid UTF-8");
        let fine = ["serve"].map(OsString::from);
        assert_eq!(parse_os_args(fine).unwrap().mode, Mode::Serve);
    }

    fn command(line: &str) -> Result<Command, String> {
        parse_command(line.split_whitespace().map(str::to_owned))
    }

    #[test]
    fn the_network_tools_are_commands_of_their_own() {
        assert_eq!(command("healthcheck 7100"), Ok(Command::Healthcheck(7100)));
        assert_eq!(
            command("reach https://example.com --timeout 2"),
            Ok(Command::Reach(net::reach::ReachArgs {
                target: net::reach::Target::Https(net::HostPort {
                    host: "example.com".to_owned(),
                    port: 443
                }),
                timeout: std::time::Duration::from_secs(2),
            }))
        );
        assert_eq!(
            command("egress --listen 0.0.0.0:8888 --allow /etc/mercury/allowlist"),
            Ok(Command::Egress(net::egress::EgressArgs {
                listen: "0.0.0.0:8888".parse().unwrap(),
                allow: PathBuf::from("/etc/mercury/allowlist"),
                ports: vec![443],
            }))
        );
        assert_eq!(command("serve"), Ok(Command::Run(parse("serve").unwrap())));
        for line in [
            "",
            "healthcheck",
            "reach",
            "egress",
            "serve healthcheck 7000",
            "health 7000",
        ] {
            assert!(command(line).is_err(), "{line}");
        }
        let args = ["reach".into(), not_utf8()];
        assert_eq!(
            parse_os_command(args).unwrap_err(),
            "an argument isn't valid UTF-8"
        );
    }

    #[test]
    fn options_are_optional() {
        let args = parse("serve").unwrap();
        assert_eq!(
            args,
            Args {
                mode: Mode::Serve,
                listen: None,
                repo: None,
                meta: None,
                access: None
            }
        );
    }

    #[test]
    fn a_missing_or_unknown_mode_is_refused() {
        assert!(parse("").is_err());
        assert!(parse("watch").is_err());
        assert!(parse("--listen 127.0.0.1:7000").is_err());
    }

    #[test]
    fn an_unknown_option_is_refused() {
        assert_eq!(
            parse("dev --verbose").unwrap_err(),
            r#"unknown option "--verbose""#
        );
    }

    #[test]
    fn an_option_without_a_value_is_refused() {
        assert_eq!(parse("dev --repo").unwrap_err(), "--repo needs a value");
    }

    #[test]
    fn a_malformed_listen_address_is_refused() {
        assert!(parse("dev --listen 7000").is_err());
    }

    #[tokio::test]
    async fn health_answers_ok_with_the_product_version() {
        let (status, body) = get_path("/health").await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(body, r#"{"status":"ok","version":"0.0.0"}"#);
    }

    #[tokio::test]
    async fn nothing_else_is_served_yet() {
        let (status, _) = get_path("/mcp").await;
        assert_eq!(status, StatusCode::NOT_FOUND);
    }
}
