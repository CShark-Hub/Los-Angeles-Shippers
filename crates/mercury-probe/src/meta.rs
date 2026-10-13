//! The metadata folder: `github.json` and `history.json`, as `sync` writes them, and the mode and
//! manifest `sync` worked out (`access.json`, `manifest.json`).
//!
//! The field names are the GitHub REST API's own, trimmed to what the probe reads. Unknown fields
//! are ignored, so a GitHub reply parses into these types and loses everything else on the way,
//! `patch` included, and the fixture may carry more than this reads. Bodies are read for links only
//! and never reach the index.

use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use mercury_schema::ids::Sha256Hex;
use mercury_schema::manifest::ProbeManifest;
use mercury_schema::{AccessLimit, AccessMode};
use regex::Regex;
use serde::{Deserialize, Serialize};

pub const GITHUB_FILE: &str = "github.json";
pub const HISTORY_FILE: &str = "history.json";
pub const ACCESS_FILE: &str = "access.json";
pub const MANIFEST_FILE: &str = "manifest.json";

/// Everything the probe knows about the repository besides its files.
#[derive(Debug, Clone)]
pub struct Meta {
    pub github: GitHub,
    pub history: History,
}

impl Meta {
    /// Reads both files; a missing `history.json` is an empty history (`metadata` mode writes
    /// none). Errors name the file, never what is in it.
    pub fn load(dir: &Path) -> Result<Self, String> {
        let history = if dir.join(HISTORY_FILE).exists() {
            read_json(dir, HISTORY_FILE)?
        } else {
            History::default()
        };
        Ok(Self {
            github: read_json(dir, GITHUB_FILE)?,
            history,
        })
    }
}

pub fn read_json<T: for<'de> Deserialize<'de>>(dir: &Path, name: &str) -> Result<T, String> {
    let raw = fs::read(dir.join(name)).map_err(|err| format!("cannot read the metadata ({name}): {err}"))?;
    serde_json::from_slice(&raw).map_err(|err| {
        format!(
            "the metadata ({name}) is invalid at line {}, column {}",
            err.line(),
            err.column()
        )
    })
}

#[derive(Debug, Clone, Deserialize)]
pub struct GitHub {
    pub fetched_at: DateTime<Utc>,
    #[serde(default)]
    pub pulls: Vec<Pull>,
    #[serde(default)]
    pub issues: Vec<Issue>,
    #[serde(default)]
    pub workflow_runs: Vec<WorkflowRun>,
    #[serde(default)]
    pub releases: Vec<Release>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Label {
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum State {
    Open,
    Closed,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Pull {
    pub number: u32,
    pub title: String,
    pub state: State,
    #[serde(default)]
    pub draft: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    pub merged_at: Option<DateTime<Utc>>,
    pub merge_commit_sha: Option<String>,
    #[serde(default)]
    pub labels: Vec<Label>,
    /// One entry per reviewer asked: whether a review is asked, never who.
    #[serde(default)]
    pub requested_reviewers: Vec<Reviewer>,
    pub body: Option<String>,
    /// Present in the code modes only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub files: Option<Vec<PullFile>>,
    /// The commit count from the PR detail, when `sync` recorded it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub commits: Option<u32>,
}

/// A reviewer asked for, without a name: GitHub's login is read past and never written, so
/// `github.json` holds one `{}` per reviewer.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Reviewer {}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PullFile {
    pub filename: String,
}

/// `closes|fixes|resolves #n`, case ignored, with the verb's other forms GitHub accepts.
static LINK: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used, reason = "a literal pattern, covered by the tests below")]
    Regex::new(r"(?i)\b(?:close[sd]?|fix(?:e[sd])?|resolve[sd]?)\s*:?\s+#(\d{1,9})\b")
        .expect("literal pattern")
});

impl Pull {
    pub fn merged(&self) -> bool {
        self.merged_at.is_some()
    }

    /// The body cut down to the phrases [`Self::linked_issues`] reads, so the rest of it is
    /// never written: `sync` keeps no description, and no code pasted into one.
    pub fn keep_links_only(&mut self) {
        let links: Vec<String> = self
            .linked_issues()
            .into_iter()
            .map(|n| format!("closes #{n}"))
            .collect();
        self.body = (!links.is_empty()).then(|| links.join(", "));
    }

    /// The issues this PR's body says it closes. The body itself goes no further.
    pub fn linked_issues(&self) -> Vec<u32> {
        let mut numbers: Vec<u32> = self
            .body
            .as_deref()
            .map(|body| {
                LINK.captures_iter(body)
                    .filter_map(|c| c.get(1)?.as_str().parse().ok())
                    .collect()
            })
            .unwrap_or_default();
        numbers.sort_unstable();
        numbers.dedup();
        numbers
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Issue {
    pub number: u32,
    pub title: String,
    pub state: State,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub closed_at: Option<DateTime<Utc>>,
    #[serde(default)]
    pub labels: Vec<Label>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub id: u64,
    pub name: Option<String>,
    pub head_branch: Option<String>,
    pub head_sha: String,
    pub status: Option<String>,
    pub conclusion: Option<String>,
    #[serde(default = "first_attempt")]
    pub run_attempt: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

fn first_attempt() -> u32 {
    1
}

/// What a run came to, in the evidence's words.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RunOutcome {
    Passed,
    Failed,
    Running,
}

impl WorkflowRun {
    pub fn outcome(&self) -> RunOutcome {
        if self.status.as_deref().is_some_and(|s| s != "completed") {
            return RunOutcome::Running;
        }
        match self.conclusion.as_deref() {
            Some("success") => RunOutcome::Passed,
            None => RunOutcome::Running,
            // failure, cancelled, timed_out, action_required, startup_failure, …
            _ => RunOutcome::Failed,
        }
    }

    /// Passed only on a second or later attempt.
    pub fn flaky(&self) -> bool {
        self.outcome() == RunOutcome::Passed && self.run_attempt > 1
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Release {
    pub name: Option<String>,
    pub tag_name: Option<String>,
    #[serde(default)]
    pub draft: bool,
    pub published_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Clone, Default, Deserialize)]
pub struct History {
    #[serde(default)]
    pub head: Option<String>,
    #[serde(default)]
    pub commits: Vec<Commit>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct Commit {
    pub committed_at: DateTime<Utc>,
    #[serde(default)]
    pub files: Vec<String>,
}

/// `access.json`: the mode `sync` worked out, and what lowered it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccessFile {
    pub access: AccessMode,
    pub access_limited_by: Option<AccessLimit>,
}

/// Where the manifest `sync` used came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManifestSource {
    /// `.mercury/probe.yaml` at the head.
    Repository,
    /// The server-side copy in `projects.yaml`.
    ProjectsCopy,
}

/// `manifest.json`: the manifest `sync` used, and the hash `serve` registers with it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ManifestFile {
    pub source: ManifestSource,
    /// The SHA-256 of the manifest file's bytes as read; of a copy, of the manifest's JSON.
    pub sha256: Sha256Hex,
    pub manifest: ProbeManifest,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pull(body: Option<&str>) -> Pull {
        serde_json::from_value(serde_json::json!({
            "number": 1, "title": "t", "state": "open", "draft": false,
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-01T00:00:00Z",
            "closed_at": null, "merged_at": null, "merge_commit_sha": null, "body": body
        }))
        .unwrap()
    }

    #[test]
    fn links_are_read_from_the_body_in_every_spelling() {
        let body = "Closes #12, fixes #7 and resolved: #9.\nFixed #7 again; see #40 and prefix#5.";
        assert_eq!(pull(Some(body)).linked_issues(), vec![7, 9, 12]);
        assert!(pull(None).linked_issues().is_empty());
        assert!(pull(Some("mentions #3 only")).linked_issues().is_empty());
    }

    #[test]
    fn only_the_links_of_a_body_are_kept() {
        let mut linked = pull(Some(
            "Closes #12 and fixes #7.\n```\nfn secret_helper() {}\n```\nResolves: #12",
        ));
        linked.keep_links_only();
        assert_eq!(linked.body.as_deref(), Some("closes #7, closes #12"));
        assert_eq!(linked.linked_issues(), vec![7, 12]);
        let mut plain = pull(Some("no links, only `code`"));
        plain.keep_links_only();
        assert_eq!(plain.body, None);
    }

    #[test]
    fn a_github_reply_loses_what_the_probe_does_not_read() {
        let reply = serde_json::json!({
            "number": 3, "title": "t", "state": "closed", "draft": false,
            "created_at": "2026-09-01T00:00:00Z", "updated_at": "2026-09-02T00:00:00Z",
            "closed_at": "2026-09-02T00:00:00Z", "merged_at": "2026-09-02T00:00:00Z",
            "merge_commit_sha": "abc", "body": null,
            "user": { "login": "someone", "id": 1 },
            "requested_reviewers": [{ "login": "reviewer", "id": 2, "avatar_url": "https://example.com/a" }],
            "head": { "ref": "feature/branch-name", "sha": "def" },
            "patch": "@@ -1 +1 @@"
        });
        let pull: Pull = serde_json::from_value(reply).unwrap();
        let written = serde_json::to_string(&pull).unwrap();
        for gone in [
            "someone",
            "\"login\"",
            "avatar_url",
            "feature/branch-name",
            "@@",
            "files",
            "commits",
        ] {
            assert!(!written.contains(gone), "{gone} in {written}");
        }
        // the reviewer is counted, never named
        assert!(written.contains(r#""requested_reviewers":[{}]"#), "{written}");
        assert_eq!(pull.requested_reviewers.len(), 1);
    }

    #[test]
    fn a_missing_history_is_an_empty_one() {
        let dir = tempfile::tempdir().unwrap();
        fs::write(
            dir.path().join(GITHUB_FILE),
            r#"{"fetched_at": "2026-09-30T12:00:00Z"}"#,
        )
        .unwrap();
        let meta = Meta::load(dir.path()).unwrap();
        assert!(meta.history.head.is_none() && meta.history.commits.is_empty());
        fs::write(dir.path().join(HISTORY_FILE), "{").unwrap();
        assert!(Meta::load(dir.path()).unwrap_err().contains(HISTORY_FILE));
    }

    #[test]
    fn run_outcomes() {
        let run = |status: &str, conclusion: Option<&str>, attempt: u32| WorkflowRun {
            id: 1,
            name: None,
            head_branch: None,
            head_sha: "a".into(),
            status: Some(status.into()),
            conclusion: conclusion.map(Into::into),
            run_attempt: attempt,
            created_at: DateTime::<Utc>::MIN_UTC,
            updated_at: DateTime::<Utc>::MIN_UTC,
        };
        assert_eq!(run("completed", Some("success"), 1).outcome(), RunOutcome::Passed);
        assert_eq!(
            run("completed", Some("cancelled"), 1).outcome(),
            RunOutcome::Failed
        );
        assert_eq!(run("in_progress", None, 1).outcome(), RunOutcome::Running);
        assert!(run("completed", Some("success"), 2).flaky());
        assert!(!run("completed", Some("failure"), 2).flaky());
    }
}
