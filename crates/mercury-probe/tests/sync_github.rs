//! `sync` against a GitHub API mock that serves the fixture's metadata, then `serve` on the folder
//! `sync` wrote. The mock records every request, so the tests see what the probe asked for: in
//! `metadata` mode no tarball, commit detail, compare, PR files or file contents, whatever the token
//! would have allowed.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use mercury_probe::index::Index;
use mercury_probe::meta::{self, AccessFile, ManifestFile, ManifestSource};
use mercury_probe::probe::{LiveIndex, Probe, Source, sha256};
use mercury_probe::registration;
use mercury_probe::sync::github::{GitHub, GitHubToken};
use mercury_probe::sync::source::SourceError;
use mercury_probe::sync::{self, SyncError, Syncer};
use mercury_probe::tools::Tools;
use mercury_schema::{AccessLimit, AccessMode, ProjectId};
use regex::Regex;
use serde_json::{Value, json};
use wiremock::matchers::{header, method, path, path_regex, query_param};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const TOKEN: &str = "github_pat_test_token_0123456789";
const REPO: &str = "example-org/delta";
const HEAD: &str = "5f1c0e7a9b2d4c6e8f0a1b3c5d7e9f1a2b4c6d8e";
/// In every `patch` the mock serves: it must never reach a file `sync` writes.
const PATCH: &str = "@@ -1 +1 @@ PATCH_SENTINEL_for_the_test";
/// In every PR body the mock serves: only the link phrases may be kept.
const BODY_CODE: &str = "public sealed class BodySentinelWriter";

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/repo-delta")
}

fn fixture_github() -> Value {
    serde_json::from_slice(&fs::read(fixture().join("meta/github.json")).unwrap()).unwrap()
}

fn fixture_manifest() -> Vec<u8> {
    fs::read(fixture().join(".mercury/probe.yaml")).unwrap()
}

/// The fixture's `fetched_at`: the round runs "then", so its windows match the fixture's.
fn then() -> DateTime<Utc> {
    "2026-09-30T12:00:00Z".parse().unwrap()
}

fn delta() -> ProjectId {
    ProjectId::parse("delta").unwrap()
}

// ---------------------------------------------------------------- projects.yaml

/// `projects.yaml` with `delta` under `access_max`, and with a copy of the fixture's manifest
/// (its rules without `version`, `project` and `display_name`) when `copy` is set.
fn projects_yaml(access_max: &str, copy: bool) -> String {
    let mut yaml = format!(
        "projects:\n  delta:\n    display_name: \"Delta — invoicing\"\n    access_max: {access_max}\n"
    );
    if copy {
        yaml.push_str("    manifest:\n");
        for line in String::from_utf8(fixture_manifest()).unwrap().lines() {
            if ["version:", "project:", "display_name:"]
                .iter()
                .any(|key| line.starts_with(key))
            {
                continue;
            }
            yaml.push_str(&format!("      {line}\n"));
        }
    }
    yaml.push_str("    probe:\n      url: http://probe-delta:7000/mcp\n      token_file: /run/mercury/probe-tokens/delta.token\n");
    yaml
}

struct Dirs {
    _root: tempfile::TempDir,
    projects: PathBuf,
    meta: PathBuf,
    repo: PathBuf,
}

fn dirs(access_max: &str, copy: bool) -> Dirs {
    let root = tempfile::tempdir().unwrap();
    let projects = root.path().join("projects.yaml");
    fs::write(&projects, projects_yaml(access_max, copy)).unwrap();
    let meta = root.path().join("meta");
    let repo = root.path().join("repo");
    fs::create_dir(&meta).unwrap();
    fs::create_dir(&repo).unwrap();
    Dirs {
        _root: root,
        projects,
        meta,
        repo,
    }
}

// ---------------------------------------------------------------- the mock

/// What the mock's token may read, and what the manifest request answers.
#[derive(Clone, Copy)]
struct Setup {
    contents: bool,
    manifest: ManifestReply,
    rate_limited: bool,
}

#[derive(Clone, Copy)]
enum ManifestReply {
    Fixture,
    Missing,
    Bytes(&'static str),
}

fn api(suffix: &str) -> String {
    format!("/repos/{REPO}{suffix}")
}

fn authorized(m: wiremock::MockBuilder) -> wiremock::MockBuilder {
    m.and(header("authorization", format!("Bearer {TOKEN}").as_str()))
}

/// A list endpoint with an ETag, and a `304` when the probe sends it back.
async fn list(server: &MockServer, at: &str, state: Option<&str>, body: Value, etag: &str) {
    let quoted = format!("\"{etag}\"");
    let mut fresh = authorized(Mock::given(method("GET")).and(path(api(at))));
    let mut unchanged = authorized(Mock::given(method("GET")).and(path(api(at))))
        .and(header("if-none-match", quoted.as_str()));
    if let Some(state) = state {
        fresh = fresh.and(query_param("state", state));
        unchanged = unchanged.and(query_param("state", state));
    }
    unchanged
        .respond_with(ResponseTemplate::new(304))
        .with_priority(1)
        .mount(server)
        .await;
    fresh
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", quoted.as_str())
                .set_body_json(body),
        )
        .mount(server)
        .await;
}

/// A pull request's files from the fixture, each with a `patch`, as the API returns them.
struct PullFiles(Value);

impl Respond for PullFiles {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let number: u64 = request.url.path().split('/').nth(5).unwrap().parse().unwrap();
        let pull = self.0["pulls"]
            .as_array()
            .unwrap()
            .iter()
            .find(|p| p["number"] == number)
            .cloned()
            .unwrap_or_default();
        let files: Vec<Value> = pull["files"]
            .as_array()
            .cloned()
            .unwrap_or_default()
            .into_iter()
            .map(|f| {
                json!({
                    "sha": HEAD, "filename": f["filename"], "status": "modified",
                    "additions": 1, "deletions": 1, "changes": 2, "patch": PATCH
                })
            })
            .collect();
        ResponseTemplate::new(200).set_body_json(files)
    }
}

async fn github_mock(mock: Setup) -> MockServer {
    let server = MockServer::start().await;
    let github = fixture_github();
    let pulls = github["pulls"].as_array().unwrap();
    let listed = |state: &str| -> Value {
        pulls
            .iter()
            .filter(|p| p["state"] == state)
            .map(|p| {
                let mut p = p.clone();
                // the list carries no files; the body is the description, code and all
                p.as_object_mut().unwrap().remove("files");
                let body = format!(
                    "{}\n```\n{BODY_CODE} {{ }}\n```",
                    p["body"].as_str().unwrap_or("")
                );
                p["body"] = json!(body);
                p
            })
            .collect()
    };
    let issues = |state: &str| -> Vec<Value> {
        github["issues"]
            .as_array()
            .unwrap()
            .iter()
            .filter(|i| i["state"] == state)
            .cloned()
            .collect()
    };

    authorized(Mock::given(method("GET")).and(path(api(""))))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "id": 1, "full_name": REPO, "default_branch": "main", "private": true
        })))
        .mount(&server)
        .await;

    let head = if mock.rate_limited {
        ResponseTemplate::new(403)
            .insert_header("x-ratelimit-remaining", "0")
            .set_body_json(json!({ "message": "API rate limit exceeded" }))
    } else if mock.contents {
        ResponseTemplate::new(200)
            .insert_header("etag", "\"head-1\"")
            .set_body_string(HEAD)
    } else {
        ResponseTemplate::new(403)
            .insert_header("x-accepted-github-permissions", "contents=read")
            .set_body_json(json!({ "message": "Resource not accessible by personal access token" }))
    };
    authorized(Mock::given(method("GET")).and(path(api("/commits/main"))))
        .and(header("accept", "application/vnd.github.sha"))
        .respond_with(head)
        .mount(&server)
        .await;

    let manifest = match (mock.contents, mock.manifest) {
        (false, _) => ResponseTemplate::new(403),
        (true, ManifestReply::Fixture) => ResponseTemplate::new(200).set_body_bytes(fixture_manifest()),
        (true, ManifestReply::Missing) => ResponseTemplate::new(404),
        (true, ManifestReply::Bytes(text)) => ResponseTemplate::new(200).set_body_string(text),
    };
    authorized(Mock::given(method("GET")).and(path(api("/contents/.mercury/probe.yaml"))))
        .and(query_param("ref", HEAD))
        .and(header("accept", "application/vnd.github.raw+json"))
        .respond_with(manifest)
        .mount(&server)
        .await;

    list(
        &server,
        "/pulls",
        Some("open"),
        json!(listed("open")),
        "pulls-open",
    )
    .await;
    list(
        &server,
        "/pulls",
        Some("closed"),
        json!(listed("closed")),
        "pulls-closed",
    )
    .await;
    // /issues lists pull requests too
    let mut open_issues = issues("open");
    open_issues.push(json!({
        "number": 424, "title": "Rename a setting", "state": "open",
        "created_at": "2026-09-29T09:10:00Z", "updated_at": "2026-09-29T09:40:00Z", "closed_at": null,
        "labels": [], "pull_request": { "url": "https://api.example.com/pulls/424" }
    }));
    list(
        &server,
        "/issues",
        Some("open"),
        json!(open_issues),
        "issues-open",
    )
    .await;
    list(
        &server,
        "/issues",
        Some("closed"),
        json!(issues("closed")),
        "issues-closed",
    )
    .await;
    let runs = github["workflow_runs"].clone();
    list(
        &server,
        "/actions/runs",
        None,
        json!({ "total_count": runs.as_array().unwrap().len(), "workflow_runs": runs }),
        "runs",
    )
    .await;
    if mock.contents {
        list(&server, "/releases", None, github["releases"].clone(), "releases").await;
    } else {
        authorized(Mock::given(method("GET")).and(path(api("/releases"))))
            .respond_with(ResponseTemplate::new(403))
            .mount(&server)
            .await;
    }

    // what metadata mode must never ask for: served all the same, so asking would get an answer
    authorized(Mock::given(method("GET")).and(path_regex(r"^/repos/example-org/delta/pulls/\d+/files$")))
        .respond_with(PullFiles(github.clone()))
        .mount(&server)
        .await;
    authorized(Mock::given(method("GET")).and(path_regex(r"^/repos/example-org/delta/pulls/\d+$")))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({ "commits": 1, "body": BODY_CODE })))
        .mount(&server)
        .await;
    for code in [r"/tarball/.*", r"/zipball/.*", r"/compare/.*", r"/commits$"] {
        authorized(Mock::given(method("GET")).and(path_regex(format!("^/repos/example-org/delta{code}"))))
            .respond_with(ResponseTemplate::new(200).set_body_string(PATCH))
            .mount(&server)
            .await;
    }
    server
}

fn syncer(server: &MockServer, dirs: &Dirs) -> Syncer<GitHub> {
    let source = GitHub::new(&server.uri(), REPO, &GitHubToken::new(TOKEN).unwrap()).unwrap();
    Syncer::new(source, delta(), &dirs.projects, &dirs.meta)
}

async fn requests(server: &MockServer) -> Vec<Request> {
    server.received_requests().await.unwrap()
}

static PR_FILES: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"/pulls/\d+/files$").unwrap());

/// Every request that would bring code or `patch`: the tarball, a commit's detail (anything under
/// `/commits` that isn't the head's id alone), compare, PR files, and file contents unless
/// `manifest` allows the manifest.
fn code_requests(requests: &[Request], manifest: bool) -> Vec<String> {
    requests
        .iter()
        .filter(|r| {
            let at = r.url.path();
            let accept = r
                .headers
                .get("accept")
                .and_then(|v| v.to_str().ok())
                .unwrap_or("");
            at.contains("/tarball")
                || at.contains("/zipball")
                || at.contains("/compare/")
                || PR_FILES.is_match(at)
                || (at.contains("/commits") && accept != "application/vnd.github.sha")
                || (at.contains("/contents/") && !manifest)
                || accept.contains("diff")
                || accept.contains("patch")
        })
        .map(|r| format!("{} {}", r.url.path(), r.url.query().unwrap_or("")))
        .collect()
}

/// `serve` on what `sync` wrote: the probe and its tools over a fresh index.
async fn serve(dirs: &Dirs) -> (Probe, Tools) {
    let probe = Probe::serve(&dirs.repo, &dirs.meta, Some(&delta())).unwrap();
    let index = Index::in_memory().await.unwrap();
    index
        .replace(&probe.rows("2026-09-30T12:05:00Z".parse().unwrap()).unwrap())
        .await
        .unwrap();
    let tools = Tools::new(index, &probe).unwrap();
    (probe, tools)
}

fn written(dirs: &Dirs) -> String {
    let mut all = String::new();
    for entry in fs::read_dir(&dirs.meta).unwrap() {
        all.push_str(&fs::read_to_string(entry.unwrap().path()).unwrap());
    }
    all
}

// ---------------------------------------------------------------- the tests

#[tokio::test]
async fn a_token_without_contents_runs_metadata_and_asks_for_no_code() {
    let server = github_mock(Setup {
        contents: false,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    let dirs = dirs("code", true);
    let mut sync = syncer(&server, &dirs);

    let mode = sync.check_mode().await.unwrap();
    assert_eq!(
        (mode.access, mode.source),
        (
            AccessFile {
                access: AccessMode::Metadata,
                access_limited_by: Some(AccessLimit::Token)
            },
            ManifestSource::ProjectsCopy
        )
    );
    let report = sync.refresh_meta(then()).await.unwrap();
    assert_eq!(
        (report.pulls, report.issues, report.runs, report.releases),
        (11, 12, 10, 0)
    );

    // nothing with code in it was asked for, though the mock would have served it
    let asked = requests(&server).await;
    assert_eq!(code_requests(&asked, false), Vec::<String>::new());
    // and the head was asked for once, as an id: a token without Contents isn't asked again
    sync.check_mode().await.unwrap();
    let heads = requests(&server)
        .await
        .iter()
        .filter(|r| r.url.path().ends_with("/commits/main"))
        .count();
    assert_eq!(heads, 1);

    // what was written: the mode, the copy, and metadata without files, patches or descriptions
    let access: AccessFile = meta::read_json(&dirs.meta, meta::ACCESS_FILE).unwrap();
    assert_eq!(access, mode.access);
    let manifest: ManifestFile = meta::read_json(&dirs.meta, meta::MANIFEST_FILE).unwrap();
    assert_eq!(
        manifest.sha256,
        sha256(&serde_json::to_vec(&manifest.manifest).unwrap())
    );
    let all = written(&dirs);
    for gone in [
        "PATCH_SENTINEL",
        "BodySentinelWriter",
        "\"files\"",
        TOKEN,
        "\"user\"",
    ] {
        assert!(!all.contains(gone), "{gone} was written");
    }

    // serve: describe says metadata and why, and the three counts are null
    let (_, tools) = serve(&dirs).await;
    let described = tools.call("describe", json!({})).await.unwrap();
    assert_eq!(described["access"], "metadata");
    assert_eq!(described["access_limited_by"], "token");
    assert_eq!(described["snapshot"]["commit_short"], Value::Null);
    let tested = tools.call("tested", json!({})).await.unwrap();
    for module in tested["modules"].as_array().unwrap() {
        assert_eq!(module["tests_total"], Value::Null, "{module}");
        assert_eq!(module["tests_changed_14d"], Value::Null, "{module}");
    }
    let health = tools.call("health", json!({})).await.unwrap();
    assert_eq!(health["todo_fixme"], Value::Null);
}

#[tokio::test]
async fn metadata_mode_through_the_api_gives_the_facts_dev_mode_gives() {
    let server = github_mock(Setup {
        contents: false,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    let dirs = dirs("code", true);
    let mut sync = syncer(&server, &dirs);
    sync.check_mode().await.unwrap();
    sync.refresh_meta(then()).await.unwrap();

    let now = "2026-09-30T12:05:00Z".parse().unwrap();
    let served = Probe::serve(&dirs.repo, &dirs.meta, Some(&delta()))
        .unwrap()
        .rows(now)
        .unwrap();
    let repo = fixture();
    let dev = Probe::dev(&repo, &repo.join("meta"), Some(AccessMode::Metadata))
        .unwrap()
        .rows(now)
        .unwrap();
    assert_eq!(served.items, dev.items);
    assert_eq!(served.modules, dev.modules);
    assert_eq!(served.runs, dev.runs);
    // releases need Contents: a metadata token reads none
    assert_eq!(served.snapshot.release_name, None);
    assert!(dev.snapshot.release_name.is_some());
}

#[tokio::test]
async fn a_token_with_contents_reads_the_manifest_and_the_pr_files_without_patch() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    let dirs = dirs("code", false);
    let mut sync = syncer(&server, &dirs);

    let mode = sync.check_mode().await.unwrap();
    assert_eq!(
        (mode.access, mode.source, mode.head.as_deref()),
        (
            AccessFile {
                access: AccessMode::Code,
                access_limited_by: None
            },
            ManifestSource::Repository,
            Some(&HEAD[..7])
        )
    );
    let report = sync.refresh_meta(then()).await.unwrap();
    assert_eq!((report.pulls, report.pulls_fetched, report.releases), (11, 11, 2));

    // the file lists were asked for; no tarball, commit detail or compare (that is the snapshot's)
    let asked = requests(&server).await;
    let code: Vec<String> = code_requests(&asked, true)
        .into_iter()
        .filter(|r| !PR_FILES.is_match(r.split(' ').next().unwrap()))
        .collect();
    assert_eq!(code, Vec::<String>::new());

    // the file names are kept, their patches are not
    let github: Value = meta::read_json(&dirs.meta, meta::GITHUB_FILE).unwrap();
    let fixture = fixture_github();
    for (synced, original) in github["pulls"]
        .as_array()
        .unwrap()
        .iter()
        .zip(sorted_pulls(&fixture))
    {
        assert_eq!(synced["number"], original["number"]);
        assert_eq!(synced["files"], original["files"], "#{}", synced["number"]);
    }
    let all = written(&dirs);
    for gone in ["PATCH_SENTINEL", "BodySentinelWriter", TOKEN, "\"patch\""] {
        assert!(!all.contains(gone), "{gone} was written");
    }

    // serve registers the manifest's own hash, and describes code mode with no snapshot yet
    let (probe, tools) = serve(&dirs).await;
    assert_eq!(
        registration::body(&probe).manifest_hash,
        sha256(&fixture_manifest())
    );
    assert!(probe.files.is_none(), "no snapshot yet");
    let described = tools.call("describe", json!({})).await.unwrap();
    assert_eq!(
        (&described["access"], &described["access_limited_by"]),
        (&json!("code"), &Value::Null)
    );
}

/// The fixture's pull requests in the order `sync` writes them: the latest update first.
fn sorted_pulls(github: &Value) -> Vec<Value> {
    let mut pulls = github["pulls"].as_array().unwrap().clone();
    pulls.sort_by(|a, b| {
        b["updated_at"]
            .as_str()
            .cmp(&a["updated_at"].as_str())
            .then(b["number"].as_u64().cmp(&a["number"].as_u64()))
    });
    pulls
}

#[tokio::test]
async fn a_second_round_sends_the_etags_and_fetches_no_unchanged_pull_request() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    let dirs = dirs("code", false);
    let mut sync = syncer(&server, &dirs);
    sync.check_mode().await.unwrap();
    sync.refresh_meta(then()).await.unwrap();
    let first = fs::read_to_string(dirs.meta.join(meta::GITHUB_FILE)).unwrap();
    let before = requests(&server).await.len();

    let mode = sync.check_mode().await.unwrap();
    assert!(!mode.changed);
    let report = sync.refresh_meta(then()).await.unwrap();
    assert_eq!(report.pulls_fetched, 0);
    let again = requests(&server).await;
    let conditional = again[before..]
        .iter()
        .filter(|r| r.headers.contains_key("if-none-match"))
        .count();
    // the head, both pull lists, both issue lists, the runs and the releases
    assert_eq!(conditional, 7);
    assert!(
        again[before..].iter().all(|r| !PR_FILES.is_match(r.url.path())),
        "no file list fetched again"
    );
    assert_eq!(
        fs::read_to_string(dirs.meta.join(meta::GITHUB_FILE)).unwrap(),
        first
    );
}

#[tokio::test]
async fn access_max_caps_a_contents_token_and_the_metadata_follows_a_change_at_once() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    let dirs = dirs("code", false);
    let mut sync = syncer(&server, &dirs);
    sync.check_mode().await.unwrap();
    sync.refresh_meta(then()).await.unwrap();
    let github: Value = meta::read_json(&dirs.meta, meta::GITHUB_FILE).unwrap();
    assert!(github["pulls"][0].get("files").is_some());
    let before = requests(&server).await.len();

    // the admin tightens the cap: the next round is metadata, and the file lists go at once
    fs::write(&dirs.projects, projects_yaml("metadata", false)).unwrap();
    let mode = sync.check_mode().await.unwrap();
    assert!(mode.changed);
    assert_eq!(
        (mode.access.access, mode.access.access_limited_by, mode.source),
        (
            AccessMode::Metadata,
            Some(AccessLimit::AccessMax),
            ManifestSource::Repository
        )
    );
    sync.refresh_meta(then()).await.unwrap();
    let after = requests(&server).await;
    assert_eq!(code_requests(&after[before..], true), Vec::<String>::new());
    let github: Value = meta::read_json(&dirs.meta, meta::GITHUB_FILE).unwrap();
    for pull in github["pulls"].as_array().unwrap() {
        assert!(pull.get("files").is_none(), "#{} kept its files", pull["number"]);
    }
    // metadata mode's commit counts (`changes`, the change heat): one detail per PR merged in the
    // 90-day window, all six of the fixture's
    let details = after[before..]
        .iter()
        .filter(|r| Regex::new(r"/pulls/\d+$").unwrap().is_match(r.url.path()))
        .count();
    assert_eq!(details, 6);
    let (_, tools) = serve(&dirs).await;
    let described = tools.call("describe", json!({})).await.unwrap();
    assert_eq!(described["access_limited_by"], "access_max");
}

#[tokio::test]
async fn no_manifest_at_the_head_falls_back_to_the_copy_and_without_one_nothing_is_written() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Missing,
        rate_limited: false,
    })
    .await;

    let dirs_without = dirs("code", false);
    let err = syncer(&server, &dirs_without).check_mode().await.unwrap_err();
    assert_eq!(err, SyncError::NoManifest);
    assert_eq!(
        fs::read_dir(&dirs_without.meta).unwrap().count(),
        0,
        "nothing written"
    );
    assert!(Probe::serve(&dirs_without.repo, &dirs_without.meta, None).is_err());

    let dirs_with = dirs("code", true);
    let mode = syncer(&server, &dirs_with).check_mode().await.unwrap();
    assert_eq!(
        (mode.access.access, mode.access.access_limited_by, mode.source),
        (
            AccessMode::Metadata,
            Some(AccessLimit::Manifest),
            ManifestSource::ProjectsCopy
        )
    );
}

#[tokio::test]
async fn an_invalid_manifest_is_not_used_and_not_quoted() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Bytes(
            "version: 0.1\nproject: delta\ndisplay_name: Delta\nsecret_value: github_pat_LEAKED_0000\n",
        ),
        rate_limited: false,
    })
    .await;
    let dirs = dirs("code", true);
    let mode = syncer(&server, &dirs).check_mode().await.unwrap();
    assert_eq!(
        (mode.access.access, mode.access.access_limited_by, mode.source),
        (
            AccessMode::Metadata,
            Some(AccessLimit::Manifest),
            ManifestSource::ProjectsCopy
        )
    );
    let line = mode.to_string();
    assert!(line.contains("invalid at line"), "{line}");
    for quoted in ["secret_value", "LEAKED", "github_pat"] {
        assert!(!line.contains(quoted), "{line}");
    }

    // a manifest of another project is not used either
    let other = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Bytes(
            "version: 0.1\nproject: omega\ndisplay_name: Omega\nallow: [progress]\njudge_context: []\n",
        ),
        rate_limited: false,
    })
    .await;
    let mode = syncer(&other, &dirs).check_mode().await.unwrap();
    assert_eq!(mode.source, ManifestSource::ProjectsCopy);
}

#[tokio::test]
async fn a_spent_rate_limit_is_an_error_not_a_mode() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: true,
    })
    .await;
    let dirs = dirs("code", true);
    let err = syncer(&server, &dirs).check_mode().await.unwrap_err();
    assert_eq!(err, SyncError::Source(SourceError::RateLimited("the head")));
    assert_eq!(fs::read_dir(&dirs.meta).unwrap().count(), 0, "nothing written");
}

#[tokio::test]
async fn the_etags_outlive_a_restart_and_the_cache_file_holds_no_body_and_no_file_name() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    let dirs = dirs("code", false);
    let cache = dirs.meta.join(sync::ETAGS_FILE);
    let cached_syncer = || {
        let source = GitHub::new(&server.uri(), REPO, &GitHubToken::new(TOKEN).unwrap())
            .unwrap()
            .with_cache_file(&cache);
        Syncer::new(source, delta(), &dirs.projects, &dirs.meta)
    };
    let mut first = cached_syncer();
    first.check_mode().await.unwrap();
    first.refresh_meta(then()).await.unwrap();
    let saved = fs::read_to_string(&cache).unwrap();
    // the PR lists are kept with their bodies cut to their links, and the replies that name the
    // code's files stay in memory only
    for gone in [BODY_CODE, PATCH, "SessionTimeoutPolicy.cs", "\"login\"", TOKEN] {
        assert!(!saved.contains(gone), "{gone} in the cache file");
    }
    assert!(saved.contains("closes #"), "the links stay");
    drop(first);

    // a restart: the new process reads the file and asks with the ETags at once
    let before = requests(&server).await.len();
    let mut second = cached_syncer();
    second.check_mode().await.unwrap();
    second.refresh_meta(then()).await.unwrap();
    let after = requests(&server).await;
    let conditional = after[before..]
        .iter()
        .filter(|r| r.headers.contains_key("if-none-match"))
        .count();
    // the head, both pull lists, both issue lists, the runs and the releases
    assert_eq!(conditional, 7);
    // a cache from another repository's run is no cache
    let other = GitHub::new(
        &server.uri(),
        "example-org/omega",
        &GitHubToken::new(TOKEN).unwrap(),
    )
    .unwrap()
    .with_cache_file(&cache);
    let mut third = Syncer::new(other, delta(), &dirs.projects, &dirs.meta);
    let before = requests(&server).await.len();
    let _ = third.check_mode().await;
    assert!(
        requests(&server).await[before..]
            .iter()
            .all(|r| !r.headers.contains_key("if-none-match")),
        "another repository's ETags are never sent"
    );
}

#[tokio::test]
async fn sync_reads_its_own_projects_entry_and_nothing_else() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    let dirs = dirs("code", false);
    // another project's entry that the core's check would refuse doesn't stop this probe
    let mut yaml = projects_yaml("code", false);
    yaml.push_str(
        "  omega:\n    display_name: Omega\n    access_max: everything\n    secret_note: OMEGA_SENTINEL\n",
    );
    fs::write(&dirs.projects, &yaml).unwrap();
    let mut sync = syncer(&server, &dirs);
    let mode = sync.check_mode().await.unwrap();
    assert_eq!(mode.access.access, AccessMode::Code);

    // its own entry broken: an error that names the project, never what is there
    let broken = projects_yaml("everything", false).replace(
        "access_max: everything",
        "access_max: everything\n    note: DELTA_SENTINEL",
    );
    fs::write(&dirs.projects, broken).unwrap();
    let err = sync.check_mode().await.unwrap_err().to_string();
    assert!(
        err.contains("entry for delta is invalid") && !err.contains("SENTINEL"),
        "{err}"
    );
}

#[cfg(unix)]
#[tokio::test]
async fn a_history_an_older_build_kept_for_14_days_is_fetched_again_for_90() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    snapshot_mocks(&server).await;
    let dirs = dirs("code", true);
    // what a build with a 14-day window left at this head
    let mut old = fixture_history();
    old["window_start"] = json!("2026-09-16T12:00:00Z");
    old["commits"] = json!([old["commits"][0].clone()]);
    fs::write(
        dirs.meta.join(meta::HISTORY_FILE),
        serde_json::to_vec(&old).unwrap(),
    )
    .unwrap();
    let mut sync = code_syncer(&server, &dirs, 512);
    sync.check_mode().await.unwrap();
    sync.refresh_code(then()).await.unwrap().unwrap();
    let listed: Vec<String> = requests(&server)
        .await
        .iter()
        .filter(|r| r.url.path() == api("/commits"))
        .filter_map(|r| {
            r.url
                .query_pairs()
                .find(|(k, _)| k == "since")
                .map(|(_, v)| v.into_owned())
        })
        .collect();
    assert_eq!(listed, ["2026-07-02T12:00:00Z"], "the whole window, asked once");
    let history: Value = meta::read_json(&dirs.meta, meta::HISTORY_FILE).unwrap();
    assert_eq!(history["window_start"], "2026-07-02T12:00:00Z");
    assert_eq!(
        history["commits"].as_array().unwrap().len(),
        fixture_history()["commits"].as_array().unwrap().len(),
        "every commit of the window"
    );
}

#[tokio::test]
async fn a_project_projects_yaml_does_not_list_writes_nothing() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    let dirs = dirs("code", false);
    let source = GitHub::new(&server.uri(), REPO, &GitHubToken::new(TOKEN).unwrap()).unwrap();
    let mut sync = Syncer::new(
        source,
        ProjectId::parse("omega").unwrap(),
        &dirs.projects,
        &dirs.meta,
    );
    assert!(matches!(sync.check_mode().await, Err(SyncError::NotListed(_))));
    fs::write(&dirs.projects, "projects: [\n  token: github_pat_LEAKED").unwrap();
    let err = sync.check_mode().await.unwrap_err().to_string();
    assert!(
        err.contains("invalid at line") && !err.contains("LEAKED"),
        "{err}"
    );
    assert_eq!(fs::read_dir(&dirs.meta).unwrap().count(), 0);
}

#[tokio::test]
async fn serve_waits_for_the_metadata_and_refuses_another_projects() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    let dirs = dirs("code", false);
    let mut sync = syncer(&server, &dirs);
    sync.check_mode().await.unwrap();
    // the mode is there, the metadata not yet
    let err = Probe::serve(&dirs.repo, &dirs.meta, Some(&delta())).unwrap_err();
    assert!(err.contains(meta::GITHUB_FILE), "{err}");
    sync.refresh_meta(then()).await.unwrap();
    assert!(Probe::serve(&dirs.repo, &dirs.meta, Some(&delta())).is_ok());
    let omega = ProjectId::parse("omega").unwrap();
    assert!(Probe::serve(&dirs.repo, &dirs.meta, Some(&omega)).is_err());
}

/// The probe's logging rule: what `sync` logs is the `Display` of its reports and errors, and none
/// of them holds the token, a file name, a patch or the manifest's text.
#[tokio::test]
async fn no_log_line_holds_a_token_a_file_name_or_a_file() {
    let mut lines = Vec::new();
    for (contents, manifest) in [
        (true, ManifestReply::Fixture),
        (false, ManifestReply::Fixture),
        (
            true,
            ManifestReply::Bytes("version: 0.1\nproject: [Invoicing.Tests]\n"),
        ),
    ] {
        let server = github_mock(Setup {
            contents,
            manifest,
            rate_limited: false,
        })
        .await;
        let dirs = dirs("code", true);
        let mut sync = syncer(&server, &dirs);
        lines.push(sync.check_mode().await.unwrap().to_string());
        lines.push(sync.refresh_meta(then()).await.unwrap().to_string());
    }
    let limited = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Missing,
        rate_limited: true,
    })
    .await;
    lines.push(
        syncer(&limited, &dirs("code", false))
            .check_mode()
            .await
            .unwrap_err()
            .to_string(),
    );
    let missing = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Missing,
        rate_limited: false,
    })
    .await;
    lines.push(
        syncer(&missing, &dirs("code", false))
            .check_mode()
            .await
            .unwrap_err()
            .to_string(),
    );

    let fixture = fixture_github();
    let mut never: Vec<String> = vec![
        TOKEN.into(),
        "PATCH_SENTINEL".into(),
        "BodySentinelWriter".into(),
        "Invoicing.Tests".into(),
        "megabank".into(),
        "http".into(),
    ];
    for pull in fixture["pulls"].as_array().unwrap() {
        for file in pull["files"].as_array().unwrap() {
            never.push(file["filename"].as_str().unwrap().to_owned());
        }
    }
    for line in &lines {
        for needle in &never {
            assert!(!line.contains(needle.as_str()), "{needle:?} in {line:?}");
        }
    }
    assert!(
        lines.iter().any(|l| l.starts_with("metadata ready:")),
        "{lines:#?}"
    );
}

/// GitHub can return a short page of runs before the last one; the total decides when to stop, and
/// a run that shifted between two pages is kept once.
#[tokio::test]
async fn the_runs_are_paged_by_their_total_and_kept_once() {
    use mercury_probe::sync::source::SourceAdapter;

    let server = MockServer::start().await;
    let run = |id: u64, at: &str| {
        json!({
            "id": id, "name": "build", "head_branch": "main", "head_sha": HEAD, "event": "push",
            "status": "completed", "conclusion": "success", "run_attempt": 1,
            "created_at": at, "updated_at": at
        })
    };
    for (page, runs) in [
        (
            "1",
            vec![run(3, "2026-09-30T10:00:00Z"), run(2, "2026-09-29T10:00:00Z")],
        ),
        (
            "2",
            vec![run(2, "2026-09-29T10:00:00Z"), run(1, "2026-09-28T10:00:00Z")],
        ),
    ] {
        authorized(Mock::given(method("GET")).and(path(api("/actions/runs"))))
            .and(query_param("page", page))
            .respond_with(
                ResponseTemplate::new(200).set_body_json(json!({ "total_count": 3, "workflow_runs": runs })),
            )
            .mount(&server)
            .await;
    }
    let github = GitHub::new(&server.uri(), REPO, &GitHubToken::new(TOKEN).unwrap()).unwrap();
    let runs = github
        .workflow_runs("main", then() - chrono::Duration::days(90))
        .await
        .unwrap();
    assert_eq!(runs.iter().map(|r| r.id).collect::<Vec<_>>(), vec![3, 2, 1]);
    let pages = requests(&server).await.len();
    assert_eq!(pages, 2, "a third page isn't asked for once the total is reached");
}

// ---------------------------------------------------------------- the snapshot and the history

/// In every commit message the mock serves: messages are never kept.
const MESSAGE: &str = "MESSAGE_SENTINEL fix the totals";
const HEAD_2: &str = "9a8b7c6d5e4f3a2b1c0d9e8f7a6b5c4d3e2f1a0b";

/// The fixture's files as GitHub's tarball of a commit: one top folder, without the fixture's
/// own metadata folder (a repository has none).
fn fixture_tarball() -> Vec<u8> {
    let mut builder = tar::Builder::new(flate2::write::GzEncoder::new(
        Vec::new(),
        flate2::Compression::fast(),
    ));
    let root = fixture();
    let mut folders = vec![root.clone()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            let relative = path
                .strip_prefix(&root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if relative == "meta" {
                continue;
            }
            if path.is_dir() {
                folders.push(path);
            } else {
                // from the bytes, as git archive writes them: a file system that reports no
                // blocks would make the builder write a sparse file of holes
                let bytes = fs::read(&path).unwrap();
                let mut header = tar::Header::new_gnu();
                header.set_size(bytes.len() as u64);
                header.set_mode(0o644);
                builder
                    .append_data(
                        &mut header,
                        format!("example-org-delta-5f1c0e7/{relative}"),
                        &bytes[..],
                    )
                    .unwrap();
            }
        }
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn fixture_history() -> Value {
    serde_json::from_slice(&fs::read(fixture().join("meta/history.json")).unwrap()).unwrap()
}

/// A commit as the list and the compare return it, its message and all.
fn commit_entry(sha: &str, at: &str) -> Value {
    json!({ "sha": sha, "commit": { "message": MESSAGE, "committer": { "name": "Dana", "date": at } } })
}

/// A commit's detail: its files, each with a `patch`.
fn commit_detail(sha: &str, at: &str, files: &[Value]) -> Value {
    let files: Vec<Value> = files
        .iter()
        .map(|f| json!({ "filename": f, "status": "modified", "patch": PATCH }))
        .collect();
    let mut detail = commit_entry(sha, at);
    detail["files"] = json!(files);
    detail
}

/// The tarball of `HEAD`, the commits list (the fixture's history) and each commit's detail.
async fn snapshot_mocks(server: &MockServer) {
    authorized(Mock::given(method("GET")).and(path(api(&format!("/tarball/{HEAD}")))))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(fixture_tarball()))
        .with_priority(1)
        .mount(server)
        .await;
    let history = fixture_history();
    let commits = history["commits"].as_array().unwrap();
    let listed: Vec<Value> = commits
        .iter()
        .map(|c| commit_entry(c["sha"].as_str().unwrap(), c["committed_at"].as_str().unwrap()))
        .collect();
    authorized(Mock::given(method("GET")).and(path(api("/commits"))))
        .and(query_param("sha", "main"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!(listed)))
        .with_priority(1)
        .mount(server)
        .await;
    for c in commits {
        let sha = c["sha"].as_str().unwrap();
        authorized(Mock::given(method("GET")).and(path(api(&format!("/commits/{sha}")))))
            .and(header("accept", "application/vnd.github+json"))
            .respond_with(ResponseTemplate::new(200).set_body_json(commit_detail(
                sha,
                c["committed_at"].as_str().unwrap(),
                c["files"].as_array().unwrap(),
            )))
            .with_priority(1)
            .mount(server)
            .await;
    }
}

/// The head moves to `HEAD_2`: one new commit after `HEAD`, its tarball and its manifest.
async fn second_head(server: &MockServer) {
    authorized(Mock::given(method("GET")).and(path(api("/commits/main"))))
        .and(header("accept", "application/vnd.github.sha"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("etag", "\"head-2\"")
                .set_body_string(HEAD_2),
        )
        .with_priority(1)
        .mount(server)
        .await;
    authorized(Mock::given(method("GET")).and(path(api("/contents/.mercury/probe.yaml"))))
        .and(query_param("ref", HEAD_2))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(fixture_manifest()))
        .with_priority(1)
        .mount(server)
        .await;
    authorized(Mock::given(method("GET")).and(path(api(&format!("/tarball/{HEAD_2}")))))
        .respond_with(ResponseTemplate::new(200).set_body_bytes(fixture_tarball()))
        .with_priority(1)
        .mount(server)
        .await;
    let at = "2026-09-30T09:00:00Z";
    authorized(Mock::given(method("GET")).and(path(api(&format!("/compare/{HEAD}...{HEAD_2}")))))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "status": "ahead", "ahead_by": 1, "behind_by": 0, "total_commits": 1,
            "commits": [commit_entry(HEAD_2, at)],
            "files": [{ "filename": "src/Export/CsvStatementFormatter.cs", "patch": PATCH }]
        })))
        .with_priority(1)
        .mount(server)
        .await;
    authorized(Mock::given(method("GET")).and(path(api(&format!("/commits/{HEAD_2}")))))
        .and(header("accept", "application/vnd.github+json"))
        .respond_with(ResponseTemplate::new(200).set_body_json(commit_detail(
            HEAD_2,
            at,
            &[json!("src/Export/CsvStatementFormatter.cs")],
        )))
        .with_priority(1)
        .mount(server)
        .await;
}

fn code_syncer(server: &MockServer, dirs: &Dirs, max_mb: u64) -> Syncer<GitHub> {
    syncer(server, dirs).with_snapshots(&dirs.repo, max_mb)
}

fn access_file(dirs: &Dirs) -> AccessFile {
    meta::read_json(&dirs.meta, meta::ACCESS_FILE).unwrap()
}

fn snapshots(dirs: &Dirs) -> Vec<String> {
    let mut names: Vec<String> = fs::read_dir(dirs.repo.join("snapshots"))
        .map(|entries| {
            entries
                .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
                .collect()
        })
        .unwrap_or_default();
    names.sort();
    names
}

/// The whole of `sync` in `code` mode, then `serve` on the snapshot and the metadata it wrote: the
/// facts `dev --access code` gives on the fixture's folder. No `patch`, message or excluded file
/// is written anywhere, and the log line names no file.
#[cfg(unix)]
#[tokio::test]
async fn code_mode_through_the_api_gives_the_facts_dev_mode_gives() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    snapshot_mocks(&server).await;
    let dirs = dirs("code", true);
    let mut sync = code_syncer(&server, &dirs, 512);
    sync.check_mode().await.unwrap();
    let report = sync.refresh_code(then()).await.unwrap().unwrap();
    sync.refresh_meta(then()).await.unwrap();

    let line = report.to_string();
    assert!(line.starts_with("snapshot 5f1c0e7 in use: "), "{line}");
    assert!(
        line.contains("3 excluded") && line.contains("10 commits"),
        "{line}"
    );
    assert!(
        !line.contains('/') && !line.contains(".cs"),
        "no file in the log: {line}"
    );

    let snapshot = dirs.repo.join("current");
    assert_eq!(
        fs::read_link(&snapshot).unwrap(),
        Path::new("snapshots").join(HEAD)
    );
    for excluded in ["docs/customers", "deploy/secrets"] {
        let left = fs::read_dir(snapshot.join(excluded))
            .map(|d| d.count())
            .unwrap_or(0);
        assert_eq!(left, 0, "{excluded}");
    }
    let history: Value =
        serde_json::from_str(&fs::read_to_string(dirs.meta.join("history.json")).unwrap()).unwrap();
    assert_eq!(history["head"], HEAD);
    // the fixture's history from the API: all of it falls inside the 90-day window
    let mut expected = fixture_history()["commits"].as_array().unwrap().clone();
    expected.retain(|c| c["committed_at"].as_str().unwrap() >= "2026-07-02T12:00:00Z");
    assert_eq!(expected.len(), 10);
    assert_eq!(history["commits"], json!(expected));
    let all = written(&dirs);
    assert!(!all.contains("PATCH_SENTINEL") && !all.contains("MESSAGE_SENTINEL"));

    let now = "2026-09-30T12:05:00Z".parse().unwrap();
    let served = Probe::serve(&dirs.repo, &dirs.meta, Some(&delta()))
        .unwrap()
        .rows(now)
        .unwrap();
    let repo = fixture();
    let dev = Probe::dev(&repo, &repo.join("meta"), Some(AccessMode::Code))
        .unwrap()
        .rows(now)
        .unwrap();
    assert_eq!(
        served.modules, dev.modules,
        "tests, TODO counts and change heat per module"
    );
    assert_eq!(served.items, dev.items);
    assert_eq!(served.snapshot.commit_short.as_deref(), Some(&HEAD[..7]));
}

/// A second head is a compare from the first and the new commit's detail, not the whole history
/// again; two snapshots stay. A drop to `metadata` then deletes them all in the same round.
#[cfg(unix)]
#[tokio::test]
async fn a_new_head_reads_a_compare_and_a_drop_to_metadata_deletes_the_snapshots() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    snapshot_mocks(&server).await;
    let dirs = dirs("code", true);
    let mut sync = code_syncer(&server, &dirs, 512);
    sync.check_mode().await.unwrap();
    sync.refresh_code(then()).await.unwrap().unwrap();
    // the same head again: nothing to do
    sync.check_mode().await.unwrap();
    assert_eq!(sync.refresh_code(then()).await.unwrap(), None);

    second_head(&server).await;
    let seen = requests(&server).await.len();
    sync.check_mode().await.unwrap();
    let report = sync.refresh_code(then()).await.unwrap().unwrap();
    assert!(report.to_string().contains("11 commits"), "{report}");
    let asked: Vec<String> = requests(&server).await[seen..]
        .iter()
        .map(|r| r.url.path().to_owned())
        .collect();
    assert!(
        asked.contains(&api(&format!("/compare/{HEAD}...{HEAD_2}"))),
        "{asked:?}"
    );
    assert!(!asked.contains(&api("/commits")), "no second listing: {asked:?}");
    assert_eq!(
        asked
            .iter()
            .filter(|p| p.starts_with(&api("/commits/")) && !p.ends_with("/main"))
            .count(),
        1
    );
    let mut both = vec![HEAD.to_owned(), HEAD_2.to_owned()];
    both.sort();
    assert_eq!(snapshots(&dirs), both, "the new one and the one it replaced");
    let history: Value =
        serde_json::from_str(&fs::read_to_string(dirs.meta.join("history.json")).unwrap()).unwrap();
    assert_eq!(
        (history["head"].as_str(), history["commits"][0]["sha"].as_str()),
        (Some(HEAD_2), Some(HEAD_2))
    );

    fs::write(&dirs.projects, projects_yaml("metadata", true)).unwrap();
    sync.check_mode().await.unwrap();
    let report = sync.refresh_code(then()).await.unwrap().unwrap();
    assert_eq!(
        report.to_string(),
        "the mode is below code: the snapshots and the history are deleted"
    );
    assert!(!dirs.repo.join("current").exists() && snapshots(&dirs).is_empty());
    assert!(
        !dirs.meta.join("history.json").exists(),
        "no file name of the history stays in /meta below code"
    );
    // and the next round finds nothing more to delete
    sync.check_mode().await.unwrap();
    assert_eq!(sync.refresh_code(then()).await.unwrap(), None);
}

/// A snapshot above the cap: nothing kept, `metadata` with `size_cap`, the PR file lists gone at
/// the next metadata round, and the same head stays capped until it moves.
#[tokio::test]
async fn a_snapshot_above_the_cap_runs_metadata_until_the_head_moves() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    snapshot_mocks(&server).await;
    let dirs = dirs("code", true);
    // the history a run before left on the persistent volume
    fs::write(dirs.meta.join("history.json"), "{}").unwrap();
    // a cap of 0 MB: the first byte is past it
    let mut sync = code_syncer(&server, &dirs, 0);
    sync.check_mode().await.unwrap();
    let report = sync.refresh_code(then()).await.unwrap().unwrap();
    assert!(report.changed_mode());
    assert_eq!(
        report.to_string(),
        "snapshot 5f1c0e7 is larger than PROBE_SNAPSHOT_MAX_MB (0 MB): metadata mode until the head moves"
    );
    let capped = AccessFile {
        access: AccessMode::Metadata,
        access_limited_by: Some(AccessLimit::SizeCap),
    };
    assert_eq!(access_file(&dirs), capped);
    assert!(snapshots(&dirs).is_empty() && !dirs.repo.join("current").exists());
    assert!(
        !dirs.meta.join("history.json").exists(),
        "the cap leaves no history in /meta"
    );
    sync.refresh_meta(then()).await.unwrap();
    let github: Value =
        serde_json::from_str(&fs::read_to_string(dirs.meta.join("github.json")).unwrap()).unwrap();
    assert!(
        github["pulls"]
            .as_array()
            .unwrap()
            .iter()
            .all(|p| p.get("files").is_none_or(Value::is_null))
    );
    let (probe, _) = serve(&dirs).await;
    assert_eq!(probe.access_limited_by, Some(AccessLimit::SizeCap));

    // the same head: still capped, and no tarball asked again
    let seen = requests(&server).await.len();
    let report = sync.check_mode().await.unwrap();
    assert_eq!(report.access, capped);
    assert!(!report.changed);
    assert_eq!(sync.refresh_code(then()).await.unwrap(), None);
    let asked = requests(&server).await;
    assert!(asked[seen..].iter().all(|r| !r.url.path().contains("/tarball")));

    // a new head is tried again
    second_head(&server).await;
    let report = sync.check_mode().await.unwrap();
    assert_eq!(report.access.access, AccessMode::Code);
}

/// A refused tarball means the token can't read the code after all: `metadata` from the next
/// round, until the pair restarts.
#[tokio::test]
async fn a_refused_tarball_runs_metadata_from_the_next_round() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    authorized(Mock::given(method("GET")).and(path(api(&format!("/tarball/{HEAD}")))))
        .respond_with(ResponseTemplate::new(403))
        .with_priority(1)
        .mount(&server)
        .await;
    let dirs = dirs("code", true);
    let mut sync = code_syncer(&server, &dirs, 512);
    sync.check_mode().await.unwrap();
    let err = sync.refresh_code(then()).await.unwrap_err();
    assert_eq!(err, SyncError::Source(SourceError::Forbidden("the snapshot")));
    let report = sync.check_mode().await.unwrap();
    assert_eq!(report.access.access, AccessMode::Metadata);
    assert_eq!(report.access.access_limited_by, Some(AccessLimit::Token));
    assert!(snapshots(&dirs).is_empty());
}

/// After a restart the tmpfs is empty until `sync` fetches the snapshot again: `serve` keeps the
/// index it built from the last one, counts and `as_of` and all, and rebuilds once it is back.
#[cfg(unix)]
#[tokio::test]
async fn after_a_restart_serve_answers_from_the_last_index_until_the_snapshot_is_back() {
    use mercury_probe::probe::{LiveIndex, Source};

    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    snapshot_mocks(&server).await;
    let dirs = dirs("code", true);
    let mut sync = code_syncer(&server, &dirs, 512);
    sync.check_mode().await.unwrap();
    sync.refresh_code(then()).await.unwrap().unwrap();
    sync.refresh_meta(then()).await.unwrap();

    let file = dirs.meta.parent().unwrap().join("index.db");
    let source = || Source::Serve {
        repo: dirs.repo.clone(),
        meta: dirs.meta.clone(),
        project: Some(delta()),
    };
    let mut live = LiveIndex::new(Index::open(&file).await.unwrap(), source());
    live.refresh().await.unwrap().expect("the first index");
    let before = live.index.snapshot().await.unwrap();
    assert!(live.index.has_code_counts().await.unwrap());

    // the pair restarts: the tmpfs is empty, the index file stays
    mercury_probe::sync::snapshot::clear(&dirs.repo).unwrap();
    drop(live);
    let mut live = LiveIndex::new(Index::open(&file).await.unwrap(), source());
    let kept = live
        .refresh()
        .await
        .unwrap()
        .expect("the probe, to answer from the last index");
    assert!(live.kept() && kept.files.is_none());
    assert!(live.refresh().await.unwrap().is_none(), "once");
    assert!(
        live.index.has_code_counts().await.unwrap(),
        "the counts are still there"
    );
    assert_eq!(
        live.index.snapshot().await.unwrap().snapshot,
        before.snapshot,
        "and its true as_of"
    );

    // sync fetches it again
    let mut sync = code_syncer(&server, &dirs, 512);
    sync.check_mode().await.unwrap();
    sync.refresh_code(then()).await.unwrap().unwrap();
    assert!(
        live.refresh().await.unwrap().is_some(),
        "rebuilt from the snapshot"
    );
    assert!(!live.kept());
    assert!(live.index.has_code_counts().await.unwrap());
}

/// Without an index yet, a code mode's metadata is indexed while the snapshot is on its way.
#[tokio::test]
async fn without_an_index_yet_serve_indexes_the_metadata_while_the_snapshot_is_on_its_way() {
    use mercury_probe::probe::{LiveIndex, Source};

    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    let dirs = dirs("code", true);
    let mut sync = syncer(&server, &dirs);
    sync.check_mode().await.unwrap();
    sync.refresh_meta(then()).await.unwrap();
    let source = Source::Serve {
        repo: dirs.repo.clone(),
        meta: dirs.meta.clone(),
        project: Some(delta()),
    };
    let mut live = LiveIndex::new(Index::in_memory().await.unwrap(), source);
    let probe = live.refresh().await.unwrap().expect("an index from the metadata");
    assert_eq!((probe.access, probe.files.is_none()), (AccessMode::Code, true));
    assert!(!live.index.has_code_counts().await.unwrap());
}

// ---------------------------------------------------------------- the code reader and the mode

/// The fixture's manifest, asking for `code_model`.
fn code_model_manifest() -> &'static str {
    let text =
        String::from_utf8(fixture_manifest())
            .unwrap()
            .replacen("access: code", "access: code_model", 1);
    assert!(text.contains("access: code_model"));
    Box::leak(text.into_boxed_str())
}

/// `code_model` is the mode only where a code reader is configured, and only when the token, the
/// manifest and `access_max` all allow it; without a reader the project runs `code`, and says why.
#[tokio::test]
async fn a_code_model_manifest_with_a_reader_runs_code_model_and_without_one_code() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Bytes(code_model_manifest()),
        rate_limited: false,
    })
    .await;
    let dirs = dirs("code_model", true);

    let mode = syncer(&server, &dirs).check_mode().await.unwrap();
    assert_eq!(
        (mode.access.access, mode.access.access_limited_by),
        (AccessMode::Code, Some(AccessLimit::NoCodeReader))
    );
    assert!(
        mode.to_string()
            .contains("no code reader (PROBE_CODE_READER_URL unset)"),
        "{mode}"
    );
    let written: AccessFile = access_file(&dirs);
    assert_eq!(
        (written.access, written.access_limited_by),
        (AccessMode::Code, Some(AccessLimit::NoCodeReader))
    );

    let mode = syncer(&server, &dirs)
        .with_code_reader(true)
        .check_mode()
        .await
        .unwrap();
    assert_eq!(
        (mode.access.access, mode.access.access_limited_by),
        (AccessMode::CodeModel, None)
    );
    assert!(mode.to_string().starts_with("access code_model"), "{mode}");
    assert_eq!(access_file(&dirs).access, AccessMode::CodeModel);

    // a reader doesn't lift a cap: access_max still comes first
    fs::write(&dirs.projects, projects_yaml("code", true)).unwrap();
    let mode = syncer(&server, &dirs)
        .with_code_reader(true)
        .check_mode()
        .await
        .unwrap();
    assert_eq!(
        (mode.access.access, mode.access.access_limited_by),
        (AccessMode::Code, Some(AccessLimit::AccessMax))
    );
    // nor does it make a project that asked for `code` ask for more
    let plain = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Fixture,
        rate_limited: false,
    })
    .await;
    fs::write(&dirs.projects, projects_yaml("code_model", true)).unwrap();
    let mode = syncer(&plain, &dirs)
        .with_code_reader(true)
        .check_mode()
        .await
        .unwrap();
    assert_eq!(
        (mode.access.access, mode.access.access_limited_by),
        (AccessMode::Code, None)
    );
}

/// `sync` and `serve` are separate processes, and only `sync` decides the mode, so a pair whose
/// `serve` has no reader URL of its own serves and registers `code`, never a tool it can't call.
#[tokio::test]
async fn a_half_configured_serve_registers_code() {
    let server = github_mock(Setup {
        contents: true,
        manifest: ManifestReply::Bytes(code_model_manifest()),
        rate_limited: false,
    })
    .await;
    let dirs = dirs("code_model", true);
    let mut sync = syncer(&server, &dirs).with_code_reader(true);
    sync.check_mode().await.unwrap();
    sync.refresh_meta(then()).await.unwrap();
    assert_eq!(access_file(&dirs).access, AccessMode::CodeModel);

    let source = || Source::Serve {
        repo: dirs.repo.clone(),
        meta: dirs.meta.clone(),
        project: Some(delta()),
    };
    // with its reader: what `sync` wrote
    let mut live = LiveIndex::new(Index::in_memory().await.unwrap(), source()).with_code_reader(true);
    let probe = live.refresh().await.unwrap().unwrap();
    assert_eq!(
        (probe.access, probe.access_limited_by),
        (AccessMode::CodeModel, None)
    );
    assert_eq!(
        registration::body(&probe).tools,
        mercury_schema::ToolName::CODE_MODEL.to_vec()
    );

    // without one: `code`, with the reason, and the six tools
    let mut live = LiveIndex::new(Index::in_memory().await.unwrap(), source()).with_code_reader(false);
    let probe = live.refresh().await.unwrap().unwrap();
    assert_eq!(
        (probe.access, probe.access_limited_by),
        (AccessMode::Code, Some(AccessLimit::NoCodeReader))
    );
    let body = registration::body(&probe);
    assert_eq!(body.tools, mercury_schema::ToolName::ALL.to_vec());
    assert_eq!(body.access, AccessMode::Code);
    assert_eq!(body.access_limited_by, Some(AccessLimit::NoCodeReader));
    // the one `Source::load` has no reader to give, so it is the same
    assert_eq!(source().load().unwrap().access, AccessMode::Code);
}
