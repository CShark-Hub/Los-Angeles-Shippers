//! The index on the fixture repository, in `code` and in `metadata` mode: every status and
//! `tested` rule, the module mapping, the counts and the health score, read back through the
//! `mercury-schema` types.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::collections::BTreeMap;
use std::fs;
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use mercury_probe::index::Index;
use mercury_probe::probe::{LiveIndex, Probe, Source};
use mercury_schema::facts::{
    BuildStatus, HealthReply, ModuleStats, ProgressReply, SchemaId, Status, Tested, TestedReply, WaitingOn,
};
use mercury_schema::{AccessMode, Fact};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/repo-delta")
}

fn now() -> DateTime<Utc> {
    "2026-09-30T12:05:00Z".parse().unwrap()
}

async fn index(repo: &Path, access: Option<AccessMode>) -> Index {
    let probe = Probe::dev(repo, &repo.join("meta"), access).unwrap();
    let index = Index::in_memory().await.unwrap();
    index.replace(&probe.rows(now()).unwrap()).await.unwrap();
    index
}

/// Facts by the number of the pull request or issue they are about: their first evidence.
fn by_number(facts: Vec<Fact>) -> BTreeMap<u32, Fact> {
    facts
        .into_iter()
        .map(|fact| {
            let number = fact.evidence[0].reference.expect("a ref").get();
            (number, fact)
        })
        .collect()
}

/// One expected fact: status, confidence, tested, module, title (or withheld), waiting_on.
type Row = (
    u32,
    Status,
    f64,
    Tested,
    &'static str,
    Option<&'static str>,
    Option<WaitingOn>,
);

fn check(facts: &BTreeMap<u32, Fact>, rows: &[Row]) {
    let mut expected: Vec<u32> = rows.iter().map(|r| r.0).collect();
    expected.sort_unstable();
    assert_eq!(facts.keys().copied().collect::<Vec<_>>(), expected, "the items");
    for &(number, status, confidence, tested, module, title, waiting_on) in rows {
        let fact = &facts[&number];
        let what = format!("#{number}");
        assert_eq!(fact.status, status, "{what} status");
        assert!(
            (fact.confidence.get() - confidence).abs() < 1e-9,
            "{what} confidence {:?}",
            fact.confidence
        );
        assert_eq!(fact.tested, tested, "{what} tested");
        assert_eq!(fact.module.as_str(), module, "{what} module");
        match title {
            Some(title) => assert_eq!(
                (fact.title.as_str(), fact.title_withheld),
                (title, false),
                "{what} title"
            ),
            None => assert_eq!(
                (fact.title.as_str(), fact.title_withheld),
                (format!("Work item in {module}").as_str(), true),
                "{what} title"
            ),
        }
        assert_eq!(fact.waiting_on, waiting_on, "{what} waiting_on");
        assert!(fact.people.is_empty(), "{what} people");
    }
}

#[tokio::test]
async fn code_mode_facts_follow_every_rule() {
    use Status::*;
    use Tested::*;
    let facts = by_number(index(&fixture(), None).await.facts().await.unwrap());
    #[rustfmt::skip]
    let rows: &[Row] = &[
        // pull requests: merged with a closed link, merged without, open in review, draft, open
        // with neither, closed unmerged, and the conflict (in review, fixes a blocked issue)
        (395, Done, 0.8, Passing, "Invoicing", Some("Billing cycle calendar"), None),
        (410, Unclear, 0.5, Unknown, "Export", Some("Export to spreadsheet"), None),
        (412, Done, 0.95, Passing, "Invoicing", Some("Invoice list with filters"), None),
        (415, Done, 0.8, Passing, "Export", Some("Faster statement export"), None),
        // merge run green, but Auth has no tests
        (418, Done, 0.95, Missing, "Auth", Some("Session timeout warning"), None),
        (419, Done, 0.95, Passing, "Invoicing", None, None),
        (421, InReview, 0.85, Unknown, "Invoicing", Some("Late fee rules for billing cycles"), None),
        (422, Unclear, 0.5, Missing, "Auth", Some("Retry failed token refresh"), None),
        (423, InProgress, 0.7, Unknown, "Export", Some("SEPA export for the client"), None),
        // a tie between a module and Other goes to the module; the title has a backtick
        (424, InProgress, 0.7, Unknown, "Invoicing", None, None),
        // only an Other file; its merge run failed, and Other has no tests: missing comes first
        (425, Done, 0.8, Missing, "Other", Some("Bump the logging package"), None),
        // issues no pull request links
        (405, Blocked, 0.9, Missing, "Auth", Some("Single sign-on for the partner portal"), Some(WaitingOn::Unknown)),
        (411, Blocked, 0.9, Unknown, "Export", Some("Statement export times out for large accounts"), Some(WaitingOn::Unknown)),
        (426, NotStarted, 0.7, Unknown, "Export", None, None),
        (427, NotStarted, 0.7, Unknown, "Export", None, None),
        (428, NotStarted, 0.7, Unknown, "Invoicing", Some("Reminder emails bounce for [email]"), None),
        (430, NotStarted, 0.7, Unknown, "Invoicing", Some("Credit notes for partial refunds"), None),
        (431, NotStarted, 0.7, Unknown, "Export", Some("the client wants a monthly statement digest"), None),
        (432, NotStarted, 0.7, Missing, "Other", Some("Tidy up the release checklist"), None),
    ];
    check(&facts, rows);
    // a linked issue joins its pull request's item
    let refs: Vec<String> = facts[&422]
        .evidence
        .iter()
        .map(|e| format!("{:?}", e.reference))
        .collect();
    assert!(refs.iter().any(|r| r.contains("407")), "{refs:?}");
    for linked in [398, 401, 407, 409] {
        assert!(!facts.contains_key(&linked), "#{linked} has its own item");
    }
}

#[tokio::test]
async fn metadata_mode_facts_come_from_labels_and_ci_alone() {
    use Status::*;
    use Tested::*;
    let facts = by_number(
        index(&fixture(), Some(AccessMode::Metadata))
            .await
            .facts()
            .await
            .unwrap(),
    );
    #[rustfmt::skip]
    let rows: &[Row] = &[
        (395, Done, 0.8, Passing, "Invoicing", Some("Billing cycle calendar"), None),
        (410, Unclear, 0.5, Unknown, "Export", Some("Export to spreadsheet"), None),
        (412, Done, 0.95, Passing, "Invoicing", Some("Invoice list with filters"), None),
        (415, Done, 0.8, Passing, "Export", Some("Faster statement export"), None),
        // never missing in metadata mode: CI alone
        (418, Done, 0.95, Passing, "Auth", Some("Session timeout warning"), None),
        (419, Done, 0.95, Passing, "Invoicing", None, None),
        (421, InReview, 0.85, Unknown, "Invoicing", Some("Late fee rules for billing cycles"), None),
        (422, Unclear, 0.5, Unknown, "Auth", Some("Retry failed token refresh"), None),
        (423, InProgress, 0.7, Unknown, "Export", Some("SEPA export for the client"), None),
        // no module label: Other, whatever its files
        (424, InProgress, 0.7, Unknown, "Other", None, None),
        (425, Done, 0.8, Failing, "Other", Some("Bump the logging package"), None),
        (405, Blocked, 0.9, Unknown, "Auth", Some("Single sign-on for the partner portal"), Some(WaitingOn::Unknown)),
        (411, Blocked, 0.9, Unknown, "Export", Some("Statement export times out for large accounts"), Some(WaitingOn::Unknown)),
        (426, NotStarted, 0.7, Unknown, "Export", None, None),
        (427, NotStarted, 0.7, Unknown, "Export", None, None),
        (428, NotStarted, 0.7, Unknown, "Invoicing", Some("Reminder emails bounce for [email]"), None),
        (430, NotStarted, 0.7, Unknown, "Invoicing", Some("Credit notes for partial refunds"), None),
        (431, NotStarted, 0.7, Unknown, "Export", Some("the client wants a monthly statement digest"), None),
        (432, NotStarted, 0.7, Unknown, "Other", Some("Tidy up the release checklist"), None),
    ];
    check(&facts, rows);
    assert!(facts.values().all(|f| f.tested != Missing));
}

fn stats(modules: &[ModuleStats]) -> Vec<(&str, Option<u32>, Option<u32>)> {
    modules
        .iter()
        .map(|m| (m.module.as_str(), m.tests_total, m.tests_changed_14d))
        .collect()
}

#[tokio::test]
async fn code_mode_counts_tests_markers_and_heat_per_module() {
    let index = index(&fixture(), None).await;
    let modules = index.module_stats().await.unwrap();
    assert_eq!(
        stats(&modules),
        [
            ("Invoicing", Some(8), Some(6)),
            ("Export", Some(3), Some(3)),
            ("Auth", Some(0), Some(0)),
            ("Other", Some(0), Some(0)),
        ]
    );
    for m in &modules {
        assert_eq!(m.last_ci.status, BuildStatus::Failed);
        assert!((m.pass_rate_7d.get() - 0.8).abs() < 1e-9);
        assert!((m.flaky_7d.get() - 0.2).abs() < 1e-9);
    }
    let heat: Vec<(String, u32)> = index
        .change_heat()
        .await
        .unwrap()
        .into_iter()
        .map(|(m, commits, _)| (m.as_str().to_owned(), commits))
        .collect();
    // the commit that touched only excluded paths counts for no module
    assert_eq!(
        heat,
        [
            ("Invoicing".into(), 2),
            ("Export".into(), 2),
            ("Auth".into(), 1),
            ("Other".into(), 3)
        ]
    );

    let health = index.health().await.unwrap();
    assert_eq!(health.build.status, BuildStatus::Failed);
    assert!((health.ci_pass_rate_7d.get() - 0.8).abs() < 1e-9);
    assert_eq!((health.open_prs.count, health.open_prs.oldest_days), (4, 12));
    let todo = health.todo_fixme.as_ref().expect("counted in code mode");
    // the TODO in the excluded file is not counted
    assert_eq!((todo.count, todo.trend_14d), (8, 0));
    assert_eq!(
        health.last_release.name.as_ref().map(|n| n.as_str()),
        Some("1.4.0 Autumn invoicing")
    );
    // 40·0.8 + 20·0 + 15·(1 − 12/30) + 15·(1 − 0.2) + 10·1
    assert_eq!(health.score.get(), 63);
    assert_eq!(
        health.as_of,
        "2026-09-30T12:00:00Z".parse::<DateTime<Utc>>().unwrap()
    );

    let snapshot = index.snapshot().await.unwrap();
    assert_eq!(snapshot.snapshot.commit_short.unwrap().as_str(), "31b03fd");
    assert_eq!(snapshot.snapshot.indexed_at, now());
    assert_eq!(snapshot.access, AccessMode::Code);
}

#[tokio::test]
async fn metadata_mode_has_no_counts() {
    let index = index(&fixture(), Some(AccessMode::Metadata)).await;
    let modules = index.module_stats().await.unwrap();
    assert_eq!(
        stats(&modules),
        [
            ("Invoicing", None, None),
            ("Export", None, None),
            ("Auth", None, None),
            ("Other", None, None)
        ]
    );
    let heat: Vec<u32> = index
        .change_heat()
        .await
        .unwrap()
        .into_iter()
        .map(|(_, commits, _)| commits)
        .collect();
    // merged PRs of the window per module label
    assert_eq!(heat, [2, 1, 1, 1]);
    let health = index.health().await.unwrap();
    assert!(health.todo_fixme.is_none());
    // (40·0.8 + 0 + 15·0.6 + 15·0.8) · 100/90, rounded
    assert_eq!(health.score.get(), 59);
    let snapshot = index.snapshot().await.unwrap();
    assert!(snapshot.snapshot.commit_short.is_none());
    assert_eq!(snapshot.access, AccessMode::Metadata);
}

/// Every row the index holds leaves as a reply that parses back through the schema, which
/// rejects extra fields, missing fields and values out of shape.
#[tokio::test]
async fn every_index_row_round_trips_through_the_schema_types() {
    for access in [None, Some(AccessMode::Metadata)] {
        let index = index(&fixture(), access).await;
        let as_of = index.snapshot().await.unwrap().snapshot.meta_at;
        let progress = ProgressReply {
            schema: SchemaId,
            as_of,
            items: index.facts().await.unwrap(),
        };
        let tested = TestedReply {
            schema: SchemaId,
            as_of,
            modules: index.module_stats().await.unwrap(),
        };
        let health = index.health().await.unwrap();
        let progress_back: ProgressReply =
            serde_json::from_value(serde_json::to_value(&progress).unwrap()).unwrap();
        let tested_back: TestedReply =
            serde_json::from_value(serde_json::to_value(&tested).unwrap()).unwrap();
        let health_back: HealthReply =
            serde_json::from_value(serde_json::to_value(&health).unwrap()).unwrap();
        assert_eq!(progress_back, progress);
        assert_eq!(tested_back, tested);
        assert_eq!(health_back, health);
        assert_eq!(progress.items.len(), 19);
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[tokio::test]
async fn the_index_is_rebuilt_only_when_something_changed() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo-delta");
    copy_tree(&fixture(), &repo);
    let mut dev = LiveIndex::new(
        Index::in_memory().await.unwrap(),
        Source::Dev {
            repo: repo.clone(),
            meta: repo.join("meta"),
            token: None,
        },
    );
    assert!(dev.refresh().await.unwrap().is_some(), "the first refresh builds");
    assert!(dev.refresh().await.unwrap().is_none(), "nothing changed");

    // a new test in a source file: the counts move
    let tests = repo.join("tests/Export.Tests/SepaExportWriterTests.cs");
    let mut text = fs::read_to_string(&tests).unwrap();
    text.push_str("\n    [Fact]\n    public void Another() { }\n");
    fs::write(&tests, text).unwrap();
    assert!(dev.refresh().await.unwrap().is_some(), "a changed file rebuilds");
    let export = dev.index.module_stats().await.unwrap()[1].clone();
    assert_eq!((export.module.as_str(), export.tests_total), ("Export", Some(4)));

    // the metadata moves: a merged pull request's issue closes
    let github = repo.join("meta/github.json");
    let raw = fs::read_to_string(&github).unwrap();
    fs::write(
        &github,
        raw.replace(
            "Single sign-on for the partner portal",
            "Single sign-on for partners",
        ),
    )
    .unwrap();
    assert!(
        dev.refresh().await.unwrap().is_some(),
        "changed metadata rebuilds"
    );
    let facts = dev.index.facts().await.unwrap();
    assert!(
        facts
            .iter()
            .any(|f| f.title.as_str() == "Single sign-on for partners")
    );

    // broken metadata: the refresh fails and the last index stays
    fs::write(&github, "{").unwrap();
    assert!(dev.refresh().await.is_err());
    assert_eq!(dev.index.facts().await.unwrap().len(), 19);
}

/// The metadata-mode lock test, extended to the index: every top-level entry but the manifest's
/// folder and the metadata is made unreadable, and the index still builds, so it read none of them.
#[cfg(unix)]
#[tokio::test]
async fn metadata_mode_builds_the_index_without_reading_the_folder() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo-delta");
    copy_tree(&fixture(), &repo);
    let locked: Vec<PathBuf> = fs::read_dir(&repo)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| !p.ends_with(".mercury") && !p.ends_with("meta"))
        .collect();
    for path in &locked {
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
    }
    let enforced = fs::read_dir(repo.join("src")).is_err();
    let mut dev = LiveIndex::new(
        Index::in_memory().await.unwrap(),
        Source::Dev {
            repo: repo.clone(),
            meta: repo.join("meta"),
            token: Some(AccessMode::Metadata),
        },
    );
    let built = dev.refresh().await;
    let facts = dev.index.facts().await;
    let health = dev.index.health().await;
    if enforced {
        // code mode would have to read the locked folders, so it fails here
        let mut code = LiveIndex::new(
            Index::in_memory().await.unwrap(),
            Source::Dev {
                repo: repo.clone(),
                meta: repo.join("meta"),
                token: None,
            },
        );
        assert!(code.refresh().await.is_err(), "code mode read a locked folder");
    }
    for path in &locked {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(built.unwrap().is_some());
    assert_eq!(facts.unwrap().len(), 19);
    assert!(health.unwrap().todo_fixme.is_none());
}

/// `changes` over the fixture: each module's commits from the history (code) or its merged PRs'
/// commits (metadata), its merged PRs, and up to five titles of them, a withheld one left out.
#[tokio::test]
async fn changes_counts_each_modules_commits_and_merged_pull_requests_since_a_date() {
    use mercury_probe::tools::Tools;
    use mercury_schema::facts::ChangesReply;
    use serde_json::json;

    type Row = (&'static str, u32, u32, Vec<&'static str>);
    async fn changes(
        access: Option<AccessMode>,
        args: serde_json::Value,
    ) -> Vec<(String, u32, u32, Vec<String>)> {
        let repo = fixture();
        let probe = Probe::dev(&repo, &repo.join("meta"), access).unwrap();
        let index = Index::in_memory().await.unwrap();
        index.replace(&probe.rows(now()).unwrap()).await.unwrap();
        let tools = Tools::new(index, &probe).unwrap();
        let reply: ChangesReply = serde_json::from_value(tools.call("changes", args).await.unwrap()).unwrap();
        assert_eq!(
            reply.as_of,
            "2026-09-30T12:00:00Z".parse::<DateTime<Utc>>().unwrap()
        );
        reply
            .modules
            .into_iter()
            .map(|m| {
                (
                    m.module.as_str().to_owned(),
                    m.commits,
                    m.merged_prs,
                    m.notable.iter().map(|t| t.as_str().to_owned()).collect(),
                )
            })
            .collect()
    }
    fn owned(rows: &[Row]) -> Vec<(String, u32, u32, Vec<String>)> {
        rows.iter()
            .map(|(m, c, p, n)| {
                (
                    (*m).to_owned(),
                    *c,
                    *p,
                    n.iter().map(|t| (*t).to_owned()).collect(),
                )
            })
            .collect()
    }

    // code: the history's commits per module touched (the commit of excluded paths counts for none,
    // a file of no module for Other); #419's title names a class, so it is withheld and not notable
    let since_september = json!({ "since": "2026-09-01" });
    assert_eq!(
        changes(None, since_september.clone()).await,
        owned(&[
            (
                "Invoicing",
                3,
                3,
                vec!["Invoice list with filters", "Billing cycle calendar"]
            ),
            ("Export", 2, 1, vec!["Faster statement export"]),
            ("Auth", 1, 1, vec!["Session timeout warning"]),
            ("Other", 3, 1, vec!["Bump the logging package"]),
        ])
    );
    // from midnight UTC of the date on; a module that didn't change is listed with zeros
    assert_eq!(
        changes(None, json!({ "since": "2026-09-20" })).await,
        owned(&[
            ("Invoicing", 2, 2, vec!["Invoice list with filters"]),
            ("Export", 2, 1, vec!["Faster statement export"]),
            ("Auth", 0, 0, vec![]),
            ("Other", 3, 1, vec!["Bump the logging package"]),
        ])
    );
    assert_eq!(
        changes(None, json!({ "since": "2026-09-01", "module": "Export" })).await,
        owned(&[("Export", 2, 1, vec!["Faster statement export"])])
    );
    // metadata: no history, so each merged PR counts its commits (1 when sync didn't record them)
    assert_eq!(
        changes(Some(AccessMode::Metadata), since_september).await,
        owned(&[
            (
                "Invoicing",
                3,
                3,
                vec!["Invoice list with filters", "Billing cycle calendar"]
            ),
            ("Export", 1, 1, vec!["Faster statement export"]),
            ("Auth", 1, 1, vec!["Session timeout warning"]),
            ("Other", 1, 1, vec!["Bump the logging package"]),
        ])
    );
}

/// The probe keeps the 90 days before the metadata's time: a date whose midnight is before them is
/// refused, never answered with part of the period or part of its first day.
#[tokio::test]
async fn changes_refuses_a_date_before_the_window() {
    use mercury_probe::tools::{ToolError, Tools};
    use serde_json::json;

    async fn tools_of(repo: &Path) -> Tools {
        let probe = Probe::dev(repo, &repo.join("meta"), None).unwrap();
        let index = Index::in_memory().await.unwrap();
        index.replace(&probe.rows(now()).unwrap()).await.unwrap();
        Tools::new(index, &probe).unwrap()
    }

    // the metadata is of 2026-09-30 12:00, so the window starts at 2026-07-02 12:00: that date's
    // midnight is before it, and 2026-07-03 is the first day counted whole
    let tools = tools_of(&fixture()).await;
    assert_eq!(
        tools.call("changes", json!({ "since": "2026-07-02" })).await,
        Err(ToolError::TooOld)
    );
    assert!(
        tools
            .call("changes", json!({ "since": "2026-07-03" }))
            .await
            .is_ok()
    );
    assert_eq!(
        tools.call("changes", json!({})).await,
        Err(ToolError::InvalidArguments)
    );

    // metadata of midnight: the window starts at 2026-07-02's midnight, so that date is whole in it
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo-delta");
    copy_tree(&fixture(), &repo);
    let github = repo.join("meta/github.json");
    let raw = fs::read_to_string(&github).unwrap();
    let midnight = raw.replace(
        r#""fetched_at": "2026-09-30T12:00:00Z""#,
        r#""fetched_at": "2026-09-30T00:00:00Z""#,
    );
    assert_ne!(midnight, raw, "the metadata's time moved");
    fs::write(&github, midnight).unwrap();
    let tools = tools_of(&repo).await;
    assert!(
        tools
            .call("changes", json!({ "since": "2026-07-02" }))
            .await
            .is_ok()
    );
    assert_eq!(
        tools.call("changes", json!({ "since": "2026-07-01" })).await,
        Err(ToolError::TooOld)
    );
}
