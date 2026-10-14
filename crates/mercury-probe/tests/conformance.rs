//! The conformance runner: it passes against `dev` on the fixture in both access modes, and fails
//! with a named check against a probe that serves an extra field, leaks a fixture line, has no
//! body cap, has a `describe` that names other tools or modules than the spec's and the fixture's,
//! or agrees to a later MCP revision.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::time::{Duration, Instant};

use axum::Router;
use axum::body::{Body, Bytes, to_bytes};
use axum::extract::{Request, State};
use axum::http::header::CONTENT_LENGTH;
use axum::middleware::{self, Next};
use axum::response::Response;
use mercury_probe::conformance::{self, Config, Report};
use mercury_probe::index::Index;
use mercury_probe::mcp::{ProbeToken, app};
use mercury_probe::probe::Probe;
use mercury_probe::tools::Tools;
use serde_json::Value;

mod support;

const TOKEN: &str = "mcy_probe_conformance_token";
const READER_MODEL: &str = "demo/reader-model";

/// A line of the fixture's README: plain words, so the schema's text rules let it through.
const LEAKED_LINE: &str = "Invoicing for small subscription businesses: an invoice ledger, billing cycles with late fees, statement and";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/repo-delta")
}

fn config(addr: SocketAddr) -> Config {
    Config {
        url: format!("http://{addr}/mcp"),
        token: TOKEN.to_owned(),
        fixture: fixture(),
    }
}

// ---------------------------------------------------------------- `dev` and the runner's binary

/// A `mercury-probe dev` process, stopped when it goes out of scope.
struct Dev {
    child: Child,
    addr: SocketAddr,
    _token_dir: tempfile::TempDir,
    token_file: PathBuf,
}

impl Dev {
    fn start(access: &str) -> Self {
        Self::spawn(&fixture(), Some(access), None)
    }

    /// `dev` on `repo`: with `--access` when it is given, and with a code reader when `reader`
    /// names one (`code_model` needs a manifest that asks for it, and a reader to call).
    fn spawn(repo: &Path, access: Option<&str>, reader: Option<&str>) -> Self {
        let dir = tempfile::tempdir().unwrap();
        let token_file = dir.path().join("probe-token");
        std::fs::write(&token_file, TOKEN).unwrap();
        // a free port: bound and released, so the probe can have it
        let addr = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let mut command = Command::new(env!("CARGO_BIN_EXE_mercury-probe"));
        command
            .args(["dev", "--repo"])
            .arg(repo)
            .arg("--meta")
            .arg(repo.join("meta"))
            .args(["--listen", &addr.to_string()]);
        if let Some(access) = access {
            command.args(["--access", access]);
        }
        command
            .env("PROBE_TOKEN_FILE", &token_file)
            .env_remove("PROBE_CORE_INTERNAL_URL")
            .env_remove("PROBE_CODE_READER_URL")
            .env_remove("PROBE_CODE_READER_MODEL");
        if let Some(url) = reader {
            command
                .env("PROBE_CODE_READER_URL", url)
                .env("PROBE_CODE_READER_MODEL", READER_MODEL);
        }
        let child = command
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut dev = Self {
            child,
            addr,
            _token_dir: dir,
            token_file,
        };
        // `dev` binds only once its index is built
        let deadline = Instant::now() + Duration::from_secs(20);
        while TcpStream::connect(addr).is_err() {
            assert!(Instant::now() < deadline, "dev didn't start");
            assert!(dev.child.try_wait().unwrap().is_none(), "dev stopped");
            std::thread::sleep(Duration::from_millis(100));
        }
        dev
    }
}

/// A `mercury-probe reader` process over a stub upstream, stopped when it goes out of scope.
struct ReaderProcess {
    child: Child,
    url: String,
}

impl ReaderProcess {
    fn start(upstream: &str) -> Self {
        let addr = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
        let child = Command::new(env!("CARGO_BIN_EXE_mercury-probe"))
            .args(["reader", "--listen", &addr.to_string()])
            .env("READER_UPSTREAM_URL", format!("{upstream}/v1/chat/completions"))
            .env("READER_MODEL", READER_MODEL)
            .env_remove("READER_KEY_FILE")
            .env_remove("READER_ALLOW_OUTSIDE")
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()
            .unwrap();
        let mut process = Self {
            child,
            url: format!("http://{addr}"),
        };
        let deadline = Instant::now() + Duration::from_secs(20);
        while TcpStream::connect(addr).is_err() {
            assert!(Instant::now() < deadline, "the reader didn't start");
            assert!(process.child.try_wait().unwrap().is_none(), "the reader stopped");
            std::thread::sleep(Duration::from_millis(100));
        }
        process
    }
}

impl Drop for ReaderProcess {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Drop for Dev {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn runner() -> Command {
    Command::new(env!("CARGO_BIN_EXE_mercury-probe-conformance"))
}

#[test]
fn the_runner_passes_against_dev_on_the_fixture_in_both_modes() {
    for access in ["code", "metadata"] {
        let dev = Dev::start(access);
        let output = runner()
            .arg("--url")
            .arg(format!("http://{}/mcp", dev.addr))
            .arg("--token-file")
            .arg(&dev.token_file)
            .arg("--fixture")
            .arg(fixture())
            .output()
            .unwrap();
        let stdout = String::from_utf8(output.stdout).unwrap();
        assert!(output.status.success(), "{access}: {stdout}");
        assert!(stdout.contains("v0.1-compatible: 8 checks passed"), "{stdout}");
        assert!(stdout.contains(&format!("in {access} mode")), "{stdout}");
        assert!(!stdout.contains("FAIL"), "{stdout}");
        // one line per check, and the token is never printed
        assert_eq!(
            stdout.lines().filter(|l| l.starts_with("ok ")).count(),
            8,
            "{stdout}"
        );
        assert!(!stdout.contains(TOKEN));
    }
}

#[test]
fn the_runner_exits_1_when_a_check_fails_and_2_on_a_bad_command_line() {
    let dev = Dev::start("code");
    let url = format!("http://{}/mcp", dev.addr);
    let wrong_token = runner()
        .args(["--url", &url, "--token", "not-the-token", "--fixture"])
        .arg(fixture())
        .output()
        .unwrap();
    assert_eq!(wrong_token.status.code(), Some(1));
    let stdout = String::from_utf8(wrong_token.stdout).unwrap();
    assert!(stdout.contains("FAIL  auth"), "{stdout}");
    assert!(stdout.contains("not v0.1-compatible"), "{stdout}");
    assert!(!stdout.contains("not-the-token"));

    for args in [
        &["--url", "http://127.0.0.1:1/mcp"][..],
        &["--url", "http://127.0.0.1:1/", "--token", "t", "--fixture", "f"],
        &[
            "--url",
            "http://127.0.0.1:1/mcp",
            "--token",
            "t",
            "--token-file",
            "f",
            "--fixture",
            "f",
        ],
        &["--verbose"],
    ] {
        let output = runner().args(args).output().unwrap();
        assert_eq!(output.status.code(), Some(2), "{args:?}");
        assert!(String::from_utf8(output.stderr).unwrap().contains("usage:"));
    }
}

/// `--token=<t>` is a wrong command line, and its message names the flag, never the token.
#[test]
fn a_token_given_after_an_equals_sign_exits_2_without_printing_it() {
    let output = runner()
        .args([
            "--url",
            "http://127.0.0.1:1/mcp",
            &format!("--token={TOKEN}"),
            "--fixture",
            "f",
        ])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(2));
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("--token takes its value as the next argument"),
        "{stderr}"
    );
    assert!(!stderr.contains(TOKEN), "{stderr}");
    assert!(output.stdout.is_empty(), "nothing was checked");
}

#[test]
fn help_names_the_flags() {
    let output = runner().arg("--help").output().unwrap();
    assert!(output.status.success());
    let text = String::from_utf8(output.stdout).unwrap();
    for flag in ["--url", "--token", "--token-file", "--fixture", "--help"] {
        assert!(text.contains(flag), "{flag} isn't in --help");
    }
}

// ---------------------------------------------------------------- a probe that doctors its replies

/// What a doctored probe does to a reply's structured content; `false` leaves the reply alone.
type Edit = fn(&mut Value) -> bool;

async fn doctor(State(edit): State<Edit>, request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    let (mut parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    let Ok(mut reply) = serde_json::from_slice::<Value>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    let mut structured = reply["result"]["structuredContent"].clone();
    if structured.is_null() || !edit(&mut structured) {
        return Response::from_parts(parts, Body::from(bytes));
    }
    // the text content carries the same JSON, so the doctoring is not caught by that alone
    reply["result"]["content"][0]["text"] = Value::String(structured.to_string());
    reply["result"]["structuredContent"] = structured;
    parts.headers.remove(CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(reply.to_string()))
}

/// A probe with no body cap: a request body of any size is read whole and handed on as compact
/// JSON, so the probe behind it serves a call it would have refused for its size.
async fn uncapped(request: Request, next: Next) -> Response {
    let (mut parts, body) = request.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    let bytes = match serde_json::from_slice::<Value>(&bytes) {
        Ok(json) => Bytes::from(json.to_string()),
        Err(_) => bytes,
    };
    parts.headers.insert(CONTENT_LENGTH, bytes.len().into());
    next.run(Request::from_parts(parts, Body::from(bytes))).await
}

/// The probe on the fixture in `dev`, in this process.
async fn fixture_probe() -> Router {
    let repo = fixture();
    let probe = Probe::dev(&repo, &repo.join("meta"), None).unwrap();
    let index = Index::in_memory().await.unwrap();
    index
        .replace(&probe.rows("2026-09-30T12:05:00Z".parse().unwrap()).unwrap())
        .await
        .unwrap();
    let tools = Tools::new(index, &probe).unwrap();
    app(tools, ProbeToken::new(Some(TOKEN)))
}

async fn serve(app: Router) -> SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
    addr
}

/// The probe on the fixture behind a proxy that applies `edit` to its replies.
async fn doctored(edit: Edit) -> SocketAddr {
    serve(
        fixture_probe()
            .await
            .layer(middleware::from_fn_with_state(edit, doctor)),
    )
    .await
}

fn failed_with(report: &Report, name: &str) -> String {
    report
        .checks
        .iter()
        .find(|check| check.name == name)
        .and_then(|check| check.result.as_ref().err().cloned())
        .unwrap_or_else(|| panic!("{name} didn't fail:\n{report}"))
}

#[tokio::test]
async fn an_undoctored_probe_passes() {
    let addr = doctored(|_| false).await;
    let report = conformance::run(&config(addr)).await;
    assert!(report.passed(), "{report}");
    // its own cap refused the 1 MiB describe, and the line says how
    let limits = report
        .checks
        .iter()
        .find(|check| check.name == "limits")
        .and_then(|check| check.result.as_ref().ok())
        .unwrap();
    assert!(
        limits.ends_with("a 1 MiB describe refused with 413")
            || limits.ends_with("a 1 MiB describe refused by a closed connection"),
        "{limits}"
    );
}

#[tokio::test]
async fn a_probe_that_serves_an_extra_field_fails_the_schemas_check() {
    let addr = doctored(|structured| {
        let Some(health) = structured.as_object_mut().filter(|o| o.contains_key("build")) else {
            return false;
        };
        health.insert("debug".to_owned(), Value::String("on".to_owned()));
        true
    })
    .await;
    let report = conformance::run(&config(addr)).await;
    assert_eq!(report.failed(), ["schemas"], "{report}");
    let why = failed_with(&report, "schemas");
    assert!(why.starts_with("health {}"), "{why}");
    assert!(why.contains("unknown field `debug`"), "{why}");
}

#[tokio::test]
async fn a_probe_that_leaks_a_fixture_line_fails_the_leak_oracle() {
    assert!(
        std::fs::read_to_string(fixture().join("README.md"))
            .unwrap()
            .contains(LEAKED_LINE)
    );
    let addr = doctored(|structured| {
        let Some(item) = structured["items"].get_mut(0) else {
            return false;
        };
        item["title"] = Value::String(LEAKED_LINE.to_owned());
        true
    })
    .await;
    let report = conformance::run(&config(addr)).await;
    assert_eq!(report.failed(), ["leak_oracle"], "{report}");
    let why = failed_with(&report, "leak_oracle");
    assert!(why.contains("progress {} leaked"), "{why}");
    assert!(why.contains("leaked"), "{why}");
}

#[tokio::test]
async fn a_probe_that_leaks_code_fails_the_leak_oracle_and_the_schemas() {
    let addr = doctored(|structured| {
        let Some(item) = structured["items"].get_mut(0) else {
            return false;
        };
        item["title"] = Value::String("see src/Export/SepaExportWriter.cs".to_owned());
        true
    })
    .await;
    let report = conformance::run(&config(addr)).await;
    assert!(report.failed().contains(&"schemas"), "{report}");
    assert!(report.failed().contains(&"leak_oracle"), "{report}");
}

/// `describe`'s own list, doctored: `edit` applied to its `tools` or its `modules`. Only `describe`
/// has both, so no other reply is touched.
fn describe_list(structured: &mut Value, list: &str, edit: impl FnOnce(&mut Vec<Value>)) -> bool {
    if structured.get("tools").is_none() || structured.get("modules").is_none() {
        return false;
    }
    edit(structured[list].as_array_mut().unwrap());
    true
}

/// A tool `describe` adds is named; the schema type knows only the six, so `schemas` fails too.
#[tokio::test]
async fn a_probe_whose_describe_adds_a_tool_fails_the_tools_check() {
    let addr =
        doctored(|structured| describe_list(structured, "tools", |tools| tools.push("search".into()))).await;
    let report = conformance::run(&config(addr)).await;
    assert_eq!(report.failed(), ["tools", "schemas"], "{report}");
    let why = failed_with(&report, "tools");
    assert_eq!(
        why,
        r#"describe's tools aren't the six of the tool list: extra ["search"]"#
    );
}

#[tokio::test]
async fn a_probe_whose_describe_renames_a_tool_fails_the_tools_check() {
    let addr = doctored(|structured| {
        describe_list(structured, "tools", |tools| {
            for tool in tools.iter_mut().filter(|tool| *tool == "changes") {
                *tool = "diff".into();
            }
        })
    })
    .await;
    let report = conformance::run(&config(addr)).await;
    assert_eq!(report.failed(), ["tools", "schemas"], "{report}");
    let why = failed_with(&report, "tools");
    assert_eq!(
        why,
        r#"describe's tools aren't the six of the tool list: missing ["changes"], extra ["diff"]"#
    );
}

/// A module `describe` leaves out is still called: the calls go over the fixture's four modules,
/// and only the `tools` check fails.
#[tokio::test]
async fn a_probe_whose_describe_drops_a_module_fails_the_tools_check() {
    let addr = doctored(|structured| {
        describe_list(structured, "modules", |modules| modules.retain(|m| m != "Auth"))
    })
    .await;
    let report = conformance::run(&config(addr)).await;
    assert_eq!(report.failed(), ["tools"], "{report}");
    assert_eq!(
        failed_with(&report, "tools"),
        r#"describe's modules aren't the fixture manifest's: missing ["Auth"]"#
    );
    let oracle = report
        .checks
        .iter()
        .find(|check| check.name == "leak_oracle")
        .and_then(|check| check.result.as_ref().ok())
        .unwrap();
    assert!(oracle.contains("over 4 modules"), "{oracle}");
}

/// A module `describe` adds is never called: the probe would refuse it, and the `schemas` check,
/// which goes over the fixture's modules, still passes.
#[tokio::test]
async fn a_probe_whose_describe_adds_a_module_fails_the_tools_check() {
    let addr =
        doctored(|structured| describe_list(structured, "modules", |modules| modules.push("Payroll".into())))
            .await;
    let report = conformance::run(&config(addr)).await;
    assert_eq!(report.failed(), ["tools"], "{report}");
    assert_eq!(
        failed_with(&report, "tools"),
        r#"describe's modules aren't the fixture manifest's: extra ["Payroll"]"#
    );
}

/// A probe that agrees to whatever revision a client proposes: its `initialize` answers with the
/// later revision the runner offers.
async fn agrees_to_any_revision(request: Request, next: Next) -> Response {
    let response = next.run(request).await;
    let (mut parts, body) = response.into_parts();
    let bytes = to_bytes(body, usize::MAX).await.unwrap();
    let Ok(mut reply) = serde_json::from_slice::<Value>(&bytes) else {
        return Response::from_parts(parts, Body::from(bytes));
    };
    if reply["result"].get("protocolVersion").is_none() {
        return Response::from_parts(parts, Body::from(bytes));
    }
    reply["result"]["protocolVersion"] = Value::String("2026-07-28".to_owned());
    parts.headers.remove(CONTENT_LENGTH);
    Response::from_parts(parts, Body::from(reply.to_string()))
}

#[tokio::test]
async fn a_probe_that_agrees_to_a_later_revision_fails_the_initialize_check() {
    let addr = serve(
        fixture_probe()
            .await
            .layer(middleware::from_fn(agrees_to_any_revision)),
    )
    .await;
    let report = conformance::run(&config(addr)).await;
    assert_eq!(report.failed(), ["initialize"], "{report}");
    assert_eq!(
        failed_with(&report, "initialize"),
        r#"a client that proposes 2026-07-28 must be answered with 2025-11-25, got "2026-07-28""#
    );
}

/// The 1 MiB request is a valid `describe`, so a probe that reads it whole serves it, and the
/// `limits` check fails on that alone.
#[tokio::test]
async fn a_probe_with_no_body_cap_fails_the_limits_check() {
    let addr = serve(fixture_probe().await.layer(middleware::from_fn(uncapped))).await;
    let report = conformance::run(&config(addr)).await;
    assert_eq!(report.failed(), ["limits"], "{report}");
    let why = failed_with(&report, "limits");
    assert_eq!(
        why,
        "a 1 MiB request body (a valid describe padded with whitespace) was served with 200: the probe has no body cap"
    );
}

#[tokio::test]
async fn a_probe_that_cannot_be_reached_fails_at_the_first_check() {
    let addr = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    let report = conformance::run(&config(addr)).await;
    assert_eq!(report.failed(), ["health"], "{report}");
    assert_eq!(failed_with(&report, "health"), "the probe could not connect");
}

// ---------------------------------------------------------------- `code_model`

/// A copy of the fixture whose manifest asks for `code_model`.
fn code_model_copy() -> (tempfile::TempDir, PathBuf) {
    fn copy(from: &Path, to: &Path) {
        std::fs::create_dir_all(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            let target = to.join(entry.file_name());
            if entry.file_type().unwrap().is_dir() {
                copy(&entry.path(), &target);
            } else {
                std::fs::copy(entry.path(), target).unwrap();
            }
        }
    }
    let scratch = tempfile::tempdir().unwrap();
    let repo = scratch.path().join("repo");
    copy(&fixture(), &repo);
    let manifest = repo.join(".mercury/probe.yaml");
    let text = std::fs::read_to_string(&manifest)
        .unwrap()
        .replacen("access: code", "access: code_model", 1);
    assert!(text.contains("access: code_model"));
    std::fs::write(&manifest, text).unwrap();
    (scratch, repo)
}

/// The binary against `dev` in `code_model`: the real reader process, over a stub upstream that
/// answers with what the request carried, each as a finding. Seven tools, `ask_code` called and
/// refused as the runner does, and no fixture string in any reply.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn the_runner_passes_against_dev_in_code_model_with_a_stub_reader() {
    let (upstream, stub) = support::upstream().await;
    let (_scratch, repo) = code_model_copy();
    let output = tokio::task::spawn_blocking(move || {
        let reader = ReaderProcess::start(&upstream);
        let dev = Dev::spawn(&repo, None, Some(&reader.url));
        runner()
            .arg("--url")
            .arg(format!("http://{}/mcp", dev.addr))
            .arg("--token-file")
            .arg(&dev.token_file)
            .arg("--fixture")
            .arg(&repo)
            .output()
            .unwrap()
    })
    .await
    .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("v0.1-compatible: 8 checks passed"), "{stdout}");
    assert!(stdout.contains("in code_model mode"), "{stdout}");
    assert!(stdout.contains("seven tools (code_model)"), "{stdout}");
    assert!(!stdout.contains("FAIL"), "{stdout}");
    assert!(!stdout.contains(TOKEN));
    // the four questions of the oracle reached the model, each with the files in it
    let sent = stub.sent();
    assert!(sent.len() >= 4, "the reader was asked {} times", sent.len());
    assert!(sent.iter().all(|request| request.contains("<<<FILE ")));
}

/// The same manifest where there is no reader: the probe runs `code`, serves six tools, and the
/// runner passes in that mode.
#[test]
fn a_code_model_manifest_with_no_reader_runs_code_and_the_runner_passes() {
    let (_scratch, repo) = code_model_copy();
    let dev = Dev::spawn(&repo, None, None);
    let output = runner()
        .arg("--url")
        .arg(format!("http://{}/mcp", dev.addr))
        .arg("--token-file")
        .arg(&dev.token_file)
        .arg("--fixture")
        .arg(&repo)
        .output()
        .unwrap();
    let stdout = String::from_utf8(output.stdout).unwrap();
    assert!(output.status.success(), "{stdout}");
    assert!(stdout.contains("in code mode"), "{stdout}");
    assert!(stdout.contains("six tools"), "{stdout}");
}
