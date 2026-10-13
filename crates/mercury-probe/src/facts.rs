//! Turns the manifest, the metadata and (in the code modes) the files into index rows. Pure: the
//! same inputs and the same `now` give the same rows. Only facts come out: titles as `SafeText`,
//! enums, numbers, dates and commit ids; never a body, a branch, a path or a line.

use std::collections::{BTreeMap, BTreeSet};
use std::path::Path;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use globset::GlobSet;
use mercury_schema::facts::{BuildStatus, EvidenceState, EvidenceType, Kind, Status, Tested, WaitingOn};
use mercury_schema::manifest::ProbeManifest;
use mercury_schema::probe_judge::{
    IssueSignals, ItemSignals, JudgeText, MAX_PER_ITEM, PullSignals, PullState,
};
use mercury_schema::redact::safe_text;
use mercury_schema::{AccessLimit, AccessMode, FactId, IssueNumber, ModuleName, Redactor, SafeText};

use crate::aggregates::{self, FileCounts, Heat};
use crate::meta::{Issue, Meta, Pull, RunOutcome, State, WorkflowRun};
use crate::rules::{self, MergeRun, ModuleMap, PrState, Signals};

/// What a rebuild reads.
pub struct Inputs<'a> {
    pub manifest: &'a ProbeManifest,
    pub redactor: &'a Redactor,
    pub access: AccessMode,
    pub access_limited_by: Option<AccessLimit>,
    /// The repository folder and its listed files, in the code modes only.
    pub files: Option<(&'a Path, &'a [String])>,
    pub exclude: &'a GlobSet,
    pub meta: &'a Meta,
}

/// One row of `items` with its evidence.
#[derive(Debug, Clone, PartialEq)]
pub struct Item {
    pub id: FactId,
    pub kind: Kind,
    pub title: SafeText,
    pub title_withheld: bool,
    pub module: ModuleName,
    pub status: Status,
    pub tested: Tested,
    pub confidence: f64,
    pub waiting_on: Option<WaitingOn>,
    pub updated_at: DateTime<Utc>,
    /// When an open pull request was opened; `None` for every other item.
    pub open_pr_since: Option<DateTime<Utc>>,
    pub evidence: Vec<EvidenceRow>,
    /// What the core's judge may be asked about the item; never indexed.
    pub ask: Ask,
}

/// What the rules left to the core's judge about one item, and what the judge may read of it
/// beside its title ([`crate::judging`]).
#[derive(Debug, Clone, PartialEq, Default)]
pub struct Ask {
    /// The item's labels, redacted with the manifest's rules: those a judge text holds, at most 20.
    pub labels: Vec<JudgeText>,
    /// The labels didn't settle the kind.
    pub kind: bool,
    /// The signals point two ways.
    pub conflict: Option<Conflict>,
}

/// A status conflict: the statuses the judge chooses between, and what it reads to choose.
#[derive(Debug, Clone, PartialEq)]
pub struct Conflict {
    pub candidates: Vec<Status>,
    pub signals: ItemSignals,
    /// What the item waits on if the judge finds it blocked: the blocking issue's first label's.
    pub waiting_on: WaitingOn,
}

#[derive(Debug, Clone, PartialEq)]
pub struct EvidenceRow {
    pub kind: EvidenceType,
    pub reference: Option<IssueNumber>,
    pub state: EvidenceState,
    pub at: NaiveDate,
}

/// One row of `module_stats`. The three counts are `None` in `metadata` mode.
#[derive(Debug, Clone, PartialEq)]
pub struct ModuleRow {
    pub module: ModuleName,
    pub tests_total: Option<u32>,
    pub tests_changed_14d: Option<u32>,
    pub todo_fixme: Option<u32>,
    pub commits_14d: u32,
    pub last_touched: Option<DateTime<Utc>>,
}

/// One row of `ci_runs`: a default-branch run, by its commit and outcome.
#[derive(Debug, Clone, PartialEq)]
pub struct RunRow {
    pub id: u64,
    pub head_sha: String,
    pub outcome: RunOutcome,
    pub attempt: u32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

/// One row of `changes`: a commit or a merged pull request, in one module, at its time.
#[derive(Debug, Clone, PartialEq)]
pub struct ChangeRow {
    pub at: DateTime<Utc>,
    pub module: ModuleName,
    /// The commits it stands for: 1 for a commit of the history; for a merged pull request 0 when
    /// there is a history (it counts them), else its `commits` (1 when `sync` didn't record it).
    pub commits: u32,
    /// Whether it is a merged pull request.
    pub merged_pr: bool,
    /// The merged pull request's item, whose title may be `notable`.
    pub item: Option<FactId>,
}

/// The `snapshot` row.
#[derive(Debug, Clone, PartialEq)]
pub struct SnapshotRow {
    /// The first 7 hexadecimal digits of the head, in the code modes only.
    pub commit_short: Option<String>,
    pub indexed_at: DateTime<Utc>,
    pub meta_at: DateTime<Utc>,
    pub access: AccessMode,
    pub access_limited_by: Option<AccessLimit>,
    pub release_name: Option<SafeText>,
    pub release_at: Option<DateTime<Utc>>,
}

/// Every row of one rebuild.
#[derive(Debug, Clone, PartialEq)]
pub struct Rows {
    pub items: Vec<Item>,
    pub modules: Vec<ModuleRow>,
    pub runs: Vec<RunRow>,
    pub changes: Vec<ChangeRow>,
    pub snapshot: SnapshotRow,
}

impl Rows {
    /// The TODO/FIXME count of the whole snapshot, `None` in `metadata` mode.
    pub fn todo_total(&self) -> Option<u32> {
        self.modules
            .iter()
            .map(|m| m.todo_fixme)
            .try_fold(0u32, |sum, n| n.map(|n| sum.saturating_add(n)))
    }
}

pub fn build(input: &Inputs<'_>, now: DateTime<Utc>) -> Result<Rows, String> {
    let map = ModuleMap::new(input.manifest)?;
    let github = &input.meta.github;
    let fetched = github.fetched_at;
    let window_14d = fetched - Duration::days(14);
    let code = input.access >= AccessMode::Code;

    // code-derived counts and heat
    let (counts, heat): (
        Option<BTreeMap<ModuleName, FileCounts>>,
        BTreeMap<ModuleName, Heat>,
    ) = match (code, input.files) {
        (true, Some((repo, files))) => {
            let changed = aggregates::changed_since(&input.meta.history, window_14d, fetched);
            let frameworks = &input.manifest.tests.frameworks;
            let counts = aggregates::file_counts(repo, files, &map, frameworks, &changed);
            let heat = aggregates::heat_from_history(
                &input.meta.history,
                &map,
                |path| input.exclude.is_match(path),
                window_14d,
                fetched,
            );
            (Some(counts), heat)
        }
        _ => (
            None,
            heat_from_merged_pulls(&github.pulls, &map, window_14d, fetched),
        ),
    };
    let module_tests = |module: &ModuleName| -> Option<u32> {
        counts
            .as_ref()
            .map(|c| c.get(module).map_or(0, |n| n.tests_total))
    };

    let runs: Vec<RunRow> = github
        .workflow_runs
        .iter()
        .filter(|run| run.created_at <= fetched)
        .map(run_row)
        .collect();

    let issues: BTreeMap<u32, &Issue> = github.issues.iter().map(|i| (i.number, i)).collect();

    // the commits come from the history once `sync` has written one (the code modes), else from
    // the merged pull requests
    let from_history = code && input.meta.history.head.is_some();
    let mut items = Vec::new();
    let mut changes = Vec::new();
    let mut linked_anywhere = BTreeSet::new();
    for pull in &github.pulls {
        let linked: Vec<&Issue> = pull
            .linked_issues()
            .into_iter()
            .filter_map(|n| issues.get(&n).copied())
            .collect();
        linked_anywhere.extend(linked.iter().map(|i| i.number));
        let item = pull_item(input, &map, pull, &linked, &runs, window_14d, &module_tests)?;
        if let Some(merged) = pull.merged_at.filter(|&at| at <= fetched) {
            changes.push(ChangeRow {
                at: merged,
                module: item.module.clone(),
                commits: if from_history {
                    0
                } else {
                    pull.commits.unwrap_or(1)
                },
                merged_pr: true,
                item: Some(item.id.clone()),
            });
        }
        items.push(item);
    }
    if from_history {
        changes.extend(commit_changes(input, &map, fetched));
    }
    for issue in github
        .issues
        .iter()
        .filter(|i| !linked_anywhere.contains(&i.number))
    {
        let module = map.module_of_labels(issue.labels.iter().map(|l| l.name.as_str()));
        let blocked_by = blocking(input.manifest, issue);
        let open = issue.state == State::Open;
        let (status, confidence) = rules::status(Signals::Issue {
            open,
            blocked: blocked_by.is_some(),
        });
        let tested = rules::tested(MergeRun::None, module_tests(&module));
        let (title, title_withheld) = rules::title(&issue.title, &module, input.redactor);
        let labels: Vec<&str> = issue.labels.iter().map(|l| l.name.as_str()).collect();
        let kind = rules::kind_of_labels(labels.iter().copied());
        items.push(Item {
            id: fact_id("issue", issue.number)?,
            kind: kind.unwrap_or(Kind::Unknown),
            title,
            title_withheld,
            module,
            status,
            tested,
            confidence,
            waiting_on: (status == Status::Blocked)
                .then(|| blocked_by.map_or(WaitingOn::Unknown, rules::waiting_on)),
            updated_at: issue.updated_at,
            open_pr_since: None,
            evidence: vec![issue_evidence(issue)?],
            ask: Ask {
                labels: judge_labels(&labels, input.redactor),
                kind: kind.is_none(),
                conflict: None,
            },
        });
    }
    items.sort_by(|a, b| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| a.id.as_str().cmp(b.id.as_str()))
    });

    let modules = map
        .modules()
        .into_iter()
        .map(|module| {
            let file_counts = counts
                .as_ref()
                .map(|c| c.get(&module).cloned().unwrap_or_default());
            let heat = heat.get(&module).cloned().unwrap_or_default();
            ModuleRow {
                tests_total: file_counts.as_ref().map(|c| c.tests_total),
                tests_changed_14d: file_counts.as_ref().map(|c| c.tests_changed_14d),
                todo_fixme: file_counts.as_ref().map(|c| c.todo_fixme),
                commits_14d: heat.commits_14d,
                last_touched: heat.last_touched,
                module,
            }
        })
        .collect();

    let release = github
        .releases
        .iter()
        .filter(|r| !r.draft && r.published_at.is_some_and(|at| at <= fetched))
        .max_by_key(|r| r.published_at);
    let release_name = release
        .and_then(|r| {
            r.name
                .as_deref()
                .filter(|n| !n.trim().is_empty())
                .or(r.tag_name.as_deref())
        })
        .and_then(|name| safe_text(name, input.redactor).ok());

    let commit_short = if code {
        input
            .meta
            .history
            .head
            .as_deref()
            .filter(|sha| sha.len() >= 7 && sha.chars().all(|c| c.is_ascii_hexdigit()))
            .map(|sha| sha[..7].to_ascii_lowercase())
    } else {
        None
    };

    Ok(Rows {
        items,
        modules,
        runs,
        changes,
        snapshot: SnapshotRow {
            commit_short,
            indexed_at: now,
            meta_at: fetched,
            access: input.access,
            access_limited_by: input.access_limited_by,
            release_name,
            release_at: release.and_then(|r| r.published_at),
        },
    })
}

#[allow(
    clippy::too_many_arguments,
    reason = "one call site; the arguments are the item's context"
)]
fn pull_item(
    input: &Inputs<'_>,
    map: &ModuleMap,
    pull: &Pull,
    linked: &[&Issue],
    runs: &[RunRow],
    window_14d: DateTime<Utc>,
    module_tests: &dyn Fn(&ModuleName) -> Option<u32>,
) -> Result<Item, String> {
    let code = input.access >= AccessMode::Code;
    let module = match (&pull.files, code) {
        (Some(files), true) => map.module_of_files(files.iter().map(|f| f.filename.as_str())),
        _ => map.module_of_labels(pull.labels.iter().map(|l| l.name.as_str())),
    };
    let open = pull.state == State::Open && !pull.merged();
    let state = if pull.merged() {
        PrState::Merged
    } else if open {
        PrState::Open {
            draft: pull.draft,
            review_requested: !pull.requested_reviewers.is_empty(),
            recent: pull.updated_at >= window_14d,
        }
    } else {
        PrState::ClosedUnmerged
    };
    // the first open linked issue with a blocking label, and that label
    let blocked_by = linked
        .iter()
        .filter(|i| i.state == State::Open)
        .find_map(|i| blocking(input.manifest, i));
    let (status, confidence) = rules::status(Signals::Pull {
        state,
        linked_closed: linked.iter().any(|i| i.state == State::Closed),
        linked_blocked: blocked_by.is_some(),
    });
    let conflict = match (state, blocked_by) {
        // an open PR whose linked issue is blocked: the PR's own status, or blocked
        (
            PrState::Open {
                draft,
                review_requested,
                recent,
            },
            Some(label),
        ) => Some(Conflict {
            candidates: vec![
                if !draft && review_requested {
                    Status::InReview
                } else {
                    Status::InProgress
                },
                Status::Blocked,
            ],
            signals: ItemSignals {
                pr: Some(PullSignals {
                    state: if draft { PullState::Draft } else { PullState::Open },
                    review_requested,
                    updated_14d: recent,
                }),
                issues: linked
                    .iter()
                    .take(MAX_PER_ITEM)
                    .map(|i| IssueSignals {
                        open: i.state == State::Open,
                        blocked: blocking(input.manifest, i).is_some(),
                    })
                    .collect(),
                // the runs are the default branch's: an open PR has none of its own
                ci: BuildStatus::None,
            },
            waiting_on: rules::waiting_on(label),
        }),
        _ => None,
    };
    let merge_run = pull
        .merge_commit_sha
        .as_deref()
        .filter(|_| pull.merged())
        .and_then(|sha| latest_run_on(runs, sha));
    let tested = rules::tested(
        match merge_run.map(|r| r.outcome) {
            Some(RunOutcome::Passed) => MergeRun::Passed,
            Some(RunOutcome::Failed) => MergeRun::Failed,
            _ => MergeRun::None,
        },
        module_tests(&module),
    );
    let (title, title_withheld) = rules::title(&pull.title, &module, input.redactor);
    let labels: Vec<&str> = pull.labels.iter().map(|l| l.name.as_str()).collect();
    let kind = rules::kind_of_labels(labels.iter().copied());

    let mut evidence = vec![pull_evidence(pull)?];
    for issue in linked {
        evidence.push(issue_evidence(issue)?);
    }
    if let Some(run) = merge_run {
        evidence.push(EvidenceRow {
            kind: EvidenceType::Ci,
            reference: None,
            state: match run.outcome {
                RunOutcome::Passed => EvidenceState::Passed,
                RunOutcome::Failed => EvidenceState::Failed,
                RunOutcome::Running => EvidenceState::Running,
            },
            at: run.updated_at.date_naive(),
        });
    }
    let updated_at = linked
        .iter()
        .map(|i| i.updated_at)
        .fold(pull.updated_at, DateTime::max);
    Ok(Item {
        id: fact_id("pull", pull.number)?,
        kind: kind.unwrap_or(Kind::Unknown),
        title,
        title_withheld,
        module,
        status,
        tested,
        confidence,
        // only blocked facts wait on something, and the rules never give a PR that status; the
        // judge may, for a conflict
        waiting_on: None,
        updated_at,
        open_pr_since: open.then_some(pull.created_at),
        evidence,
        ask: Ask {
            labels: judge_labels(&labels, input.redactor),
            kind: kind.is_none(),
            conflict,
        },
    })
}

/// The labels as the judge may read them: redacted with the manifest's rules, each one a judge
/// text can hold (one that can't is left out), at most 20.
fn judge_labels(labels: &[&str], redactor: &Redactor) -> Vec<JudgeText> {
    labels
        .iter()
        .filter_map(|label| JudgeText::new(redactor.apply(label.trim())).ok())
        .take(MAX_PER_ITEM)
        .collect()
}

/// The issue's first label the manifest counts as blocking.
fn blocking<'i>(manifest: &ProbeManifest, issue: &'i Issue) -> Option<&'i str> {
    rules::blocking_label(manifest, issue.labels.iter().map(|l| l.name.as_str()))
}

fn run_row(run: &WorkflowRun) -> RunRow {
    RunRow {
        id: run.id,
        head_sha: run.head_sha.to_ascii_lowercase(),
        outcome: run.outcome(),
        attempt: run.run_attempt,
        created_at: run.created_at,
        updated_at: run.updated_at,
    }
}

/// The latest run on a commit: the last created, then the highest attempt.
fn latest_run_on<'a>(runs: &'a [RunRow], sha: &str) -> Option<&'a RunRow> {
    runs.iter()
        .filter(|run| run.head_sha.eq_ignore_ascii_case(sha))
        .max_by_key(|run| (run.created_at, run.attempt, run.updated_at))
}

fn reference(number: u32) -> Result<Option<IssueNumber>, String> {
    IssueNumber::new(number)
        .map(Some)
        .map_err(|_| "a pull request or issue number is out of range".to_owned())
}

fn pull_evidence(pull: &Pull) -> Result<EvidenceRow, String> {
    let (state, at) = match (pull.merged_at, pull.state, pull.draft) {
        (Some(merged), _, _) => (EvidenceState::Merged, merged),
        (None, State::Closed, _) => (EvidenceState::Closed, pull.closed_at.unwrap_or(pull.updated_at)),
        (None, State::Open, true) => (EvidenceState::Draft, pull.updated_at),
        (None, State::Open, false) => (EvidenceState::Open, pull.updated_at),
    };
    Ok(EvidenceRow {
        kind: EvidenceType::Pr,
        reference: reference(pull.number)?,
        state,
        at: at.date_naive(),
    })
}

fn issue_evidence(issue: &Issue) -> Result<EvidenceRow, String> {
    let (state, at) = match issue.state {
        State::Open => (EvidenceState::Open, issue.updated_at),
        State::Closed => (EvidenceState::Closed, issue.closed_at.unwrap_or(issue.updated_at)),
    };
    Ok(EvidenceRow {
        kind: EvidenceType::Issue,
        reference: reference(issue.number)?,
        state,
        at: at.date_naive(),
    })
}

/// In the code modes: one row per commit of the history and module it touches (a path the
/// manifest excludes counts for none, a file of no module for `Other`).
fn commit_changes(input: &Inputs<'_>, map: &ModuleMap, until: DateTime<Utc>) -> Vec<ChangeRow> {
    let mut rows = Vec::new();
    for commit in input
        .meta
        .history
        .commits
        .iter()
        .filter(|c| c.committed_at <= until)
    {
        let modules: BTreeSet<ModuleName> = commit
            .files
            .iter()
            .filter(|path| !input.exclude.is_match(path.as_str()))
            .map(|path| {
                map.module_of_file(path)
                    .cloned()
                    .unwrap_or_else(ModuleName::other)
            })
            .collect();
        rows.extend(modules.into_iter().map(|module| ChangeRow {
            at: commit.committed_at,
            module,
            commits: 1,
            merged_pr: false,
            item: None,
        }));
    }
    rows
}

/// In `metadata` mode: the merged PRs of the window per module label, each counting its commits.
fn heat_from_merged_pulls(
    pulls: &[Pull],
    map: &ModuleMap,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> BTreeMap<ModuleName, Heat> {
    let mut heat = BTreeMap::new();
    for pull in pulls {
        let Some(merged) = pull.merged_at.filter(|&at| at <= until) else {
            continue;
        };
        let module = map.module_of_labels(pull.labels.iter().map(|l| l.name.as_str()));
        let entry: &mut Heat = heat.entry(module).or_default();
        if merged >= since {
            entry.commits_14d += pull.commits.unwrap_or(1);
        }
        if entry.last_touched.is_none_or(|at| at < merged) {
            entry.last_touched = Some(merged);
        }
    }
    heat
}

/// A stable id that doesn't show the number: `f_` and 10 base-36 digits of a hash of the item.
fn fact_id(kind: &str, number: u32) -> Result<FactId, String> {
    // FNV-1a, 64 bits: stable across runs and platforms, unlike the standard library's hasher
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in kind.bytes().chain(*b":").chain(number.to_string().bytes()) {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    let mut digits = String::new();
    for _ in 0..10 {
        let digit = u32::try_from(hash % 36).unwrap_or(0);
        digits.push(char::from_digit(digit, 36).unwrap_or('0'));
        hash /= 36;
    }
    FactId::parse(&format!("f_{digits}")).map_err(|_| "a fact id has the wrong shape".to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fact_ids_are_stable_and_distinct() {
        let a = fact_id("pull", 412).unwrap();
        assert_eq!(a, fact_id("pull", 412).unwrap());
        assert_ne!(a, fact_id("issue", 412).unwrap());
        assert_ne!(a, fact_id("pull", 413).unwrap());
        assert!(!a.as_str().contains("412"));
        assert_eq!(a.as_str().len(), 12);
    }
}
