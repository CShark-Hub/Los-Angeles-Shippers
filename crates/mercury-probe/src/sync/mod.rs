//! `sync` mode: reads the repository through its host's API and writes `/meta` for `serve`.
//!
//! Before anything is downloaded it works out the mode: the admin's `access_max` and the
//! server-side manifest copy from `projects.yaml`, the head (which needs the code permission, so it
//! is the token's test) and the manifest alone at the head; the strictest of them is the mode. Then
//! the metadata every `PROBE_META_INTERVAL`: pull requests, issues, the default branch's runs and
//! releases, and in the code modes each updated pull request's file list. `metadata` mode asks for
//! nothing with code or `patch` in it, whatever the token would allow.
//!
//! In the code modes, when the head moves, the commit's tarball streams into the tmpfs `/repo`
//! (`snapshot`), and `history.json` follows it: the commits of the last 90 days and their file
//! names. A drop to `metadata` deletes the snapshots in the same round.
//!
//! Every file is written to a temporary name and renamed, so `serve` never reads half a file. The
//! log lines carry counts, sizes, modes and commit ids only (the [`Display`](std::fmt::Display) of the
//! reports and errors here), never a token, a file name or a file's contents.

pub mod github;
pub mod snapshot;
pub mod source;

use std::collections::HashMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use chrono::{DateTime, Utc};
use futures_util::stream::{self, StreamExt, TryStreamExt};
use mercury_schema::config::ProjectConfig;
use mercury_schema::manifest::ProbeManifest;
use mercury_schema::{AccessLimit, AccessMode, ProjectId};
use serde::{Deserialize, Serialize};

use crate::access::{self, Switches};
use crate::dev;
use crate::meta::{
    self, AccessFile, Issue, ManifestFile, ManifestSource, Pull, PullFile, Release, WorkflowRun,
};
use crate::probe;
use snapshot::{SnapshotError, Unpacked};
use source::{CommitRef, Fetched, Repository, SourceAdapter, SourceError};

/// Where Compose mounts the core's `projects.yaml`, read-only.
pub const PROJECTS_FILE: &str = "/config/projects.yaml";

/// The ETag cache in `/meta`, kept across restarts; `serve` doesn't read it.
pub const ETAGS_FILE: &str = "etags.json";

/// The work kept: every open item, what closed or ran in the last 90 days, and the commits of the
/// last 90 days with their file names (the code modes) or the merged PRs' commit counts (`metadata`).
pub const WINDOW: chrono::Duration = chrono::Duration::days(90);

/// How long a token that couldn't read the code is taken at its word: then the head is asked again,
/// so one refused or lost answer doesn't hold the probe in `metadata` for the rest of the run.
pub const TOKEN_RECHECK: Duration = Duration::from_secs(10 * 60);

/// Requests for single pull requests in flight at once.
pub const PER_PR_AT_ONCE: usize = 8;

/// The largest manifest read through the API.
pub const MAX_MANIFEST_BYTES: usize = 64 * 1024;

/// What `sync` is told by its environment.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncConfig {
    pub project: ProjectId,
    /// `owner/name` on the host.
    pub repo: String,
    pub token_file: PathBuf,
    /// How often the head and the mode are checked (`PROBE_CODE_INTERVAL`).
    pub code_interval: Duration,
    /// How often the metadata is fetched (`PROBE_META_INTERVAL`).
    pub meta_interval: Duration,
    /// The largest snapshot, in megabytes (`PROBE_SNAPSHOT_MAX_MB`).
    pub snapshot_max_mb: u64,
    /// Whether a code reader is configured (`PROBE_CODE_READER_URL`, a private host): without one,
    /// a project that asks for `code_model` runs `code`.
    pub code_reader: bool,
}

impl SyncConfig {
    /// `PROBE_PROJECT`, `PROBE_GITHUB_REPO`, `PROBE_GITHUB_TOKEN_FILE`, the two intervals (`60s` and
    /// `300s` by default), `PROBE_SNAPSHOT_MAX_MB` (512 by default) and `PROBE_CODE_READER_URL` (a
    /// private host, or the start stops). `HTTPS_PROXY` (or `https_proxy`) must be set: `sync`
    /// reaches the host only through its egress proxy.
    pub fn from_env(var: impl Fn(&str) -> Option<String>) -> Result<Self, String> {
        let required = |name: &str| {
            var(name)
                .filter(|v| !v.trim().is_empty())
                .ok_or(format!("{name} is not set"))
        };
        if !["HTTPS_PROXY", "https_proxy"]
            .iter()
            .any(|name| var(name).is_some_and(|v| !v.trim().is_empty()))
        {
            return Err(
                "HTTPS_PROXY is not set: sync reaches the git host only through its egress proxy, never directly"
                    .to_owned(),
            );
        }
        let project = ProjectId::parse(required("PROBE_PROJECT")?.trim())
            .map_err(|_| "PROBE_PROJECT is not a project id".to_owned())?;
        let code_reader = crate::reader::client::configured_url(&var)?.is_some();
        Ok(Self {
            project,
            repo: required("PROBE_GITHUB_REPO")?.trim().to_owned(),
            token_file: required("PROBE_GITHUB_TOKEN_FILE")?.into(),
            code_interval: interval("PROBE_CODE_INTERVAL", var("PROBE_CODE_INTERVAL"), 60)?,
            meta_interval: interval("PROBE_META_INTERVAL", var("PROBE_META_INTERVAL"), 300)?,
            snapshot_max_mb: match var("PROBE_SNAPSHOT_MAX_MB").map(|v| v.trim().to_owned()) {
                None => snapshot::DEFAULT_MAX_MB,
                Some(v) if v.is_empty() => snapshot::DEFAULT_MAX_MB,
                Some(v) => v
                    .parse::<u64>()
                    .ok()
                    .filter(|&mb| (1..=1024 * 1024).contains(&mb))
                    .ok_or("PROBE_SNAPSHOT_MAX_MB must be a whole number of megabytes")?,
            },
            code_reader,
        })
    }
}

/// An interval written `60s`, `5m` or `60` (seconds), at least 10 seconds.
pub fn interval(name: &str, value: Option<String>, default_secs: u64) -> Result<Duration, String> {
    let Some(value) = value.map(|v| v.trim().to_owned()).filter(|v| !v.is_empty()) else {
        return Ok(Duration::from_secs(default_secs));
    };
    let (number, unit) = match value.strip_suffix('m') {
        Some(minutes) => (minutes, 60),
        None => (value.strip_suffix('s').unwrap_or(&value), 1),
    };
    let secs = number
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(unit))
        .filter(|&secs| secs >= 10)
        .ok_or(format!(
            "{name} must be a number of seconds (60s) or minutes (5m), at least 10s"
        ))?;
    Ok(Duration::from_secs(secs))
}

/// Why a round wrote nothing. The messages name files and endpoints, never what is in them.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SyncError {
    #[error("{0}")]
    Source(#[from] SourceError),
    #[error("{0}")]
    Projects(String),
    #[error("projects.yaml doesn't list the project {0}")]
    NotListed(ProjectId),
    #[error(
        "no manifest: the repository's .mercury/probe.yaml can't be read and projects.yaml has no copy for this project; nothing is written, so serve stays unregistered"
    )]
    NoManifest,
    #[error("the copy of the manifest in projects.yaml is invalid: {0}")]
    InvalidCopy(String),
    #[error("cannot write the metadata ({0})")]
    Write(&'static str),
    #[error("the mode isn't known yet")]
    NoMode,
    #[error("{0}")]
    Snapshot(#[from] SnapshotError),
}

/// The mode a round worked out, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModeReport {
    pub access: AccessFile,
    pub source: ManifestSource,
    /// The first seven digits of the head, when the token can read it.
    pub head: Option<String>,
    /// Why the repository's own manifest wasn't used: its position-only parse error, or that it
    /// is missing.
    pub manifest_note: Option<String>,
    /// Whether the mode or the manifest differs from the last round's.
    pub changed: bool,
}

impl fmt::Display for ModeReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "access {}", access::mode_name(self.access.access))?;
        if let Some(limit) = self.access.access_limited_by {
            write!(f, " (limited by {})", access::limit_name(limit, "the token"))?;
        }
        match self.source {
            ManifestSource::Repository => f.write_str(", the repository's manifest")?,
            ManifestSource::ProjectsCopy => f.write_str(", the manifest's copy in projects.yaml")?,
        }
        if let Some(head) = &self.head {
            write!(f, " at {head}")?;
        }
        if let Some(note) = &self.manifest_note {
            write!(f, " ({note})")?;
        }
        Ok(())
    }
}

/// What a metadata round fetched, for the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MetaReport {
    pub pulls: usize,
    pub issues: usize,
    pub runs: usize,
    pub releases: usize,
    /// Pull requests whose file list (code modes) or detail (`metadata`) was fetched this round.
    pub pulls_fetched: usize,
    pub took: Duration,
}

impl fmt::Display for MetaReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "metadata ready: {} pull requests ({} fetched one by one), {} issues, {} runs, {} releases, in {:.1} s",
            self.pulls,
            self.pulls_fetched,
            self.issues,
            self.runs,
            self.releases,
            self.took.as_secs_f64()
        )
    }
}

/// What a code round did, for the log.
#[derive(Debug, Clone, PartialEq)]
pub enum CodeReport {
    /// A new snapshot is in use, with its history.
    Snapshot {
        head: String,
        unpacked: Unpacked,
        commits: usize,
        took: Duration,
    },
    /// The snapshot already in use; the history caught up with it.
    History { head: String, commits: usize },
    /// The snapshot was larger than the cap: `metadata` until the head moves.
    Capped { head: String, max_mb: u64 },
    /// The mode is below `code`: every snapshot and the history were deleted.
    Cleared,
}

impl CodeReport {
    /// Whether the round changed the mode, so the metadata is fetched again at once.
    pub fn changed_mode(&self) -> bool {
        matches!(self, CodeReport::Capped { .. })
    }
}

impl fmt::Display for CodeReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodeReport::Snapshot {
                head,
                unpacked,
                commits,
                took,
            } => write!(
                f,
                "snapshot {head} in use: {unpacked}, in {:.1} s; history: {commits} commits in 90 days",
                took.as_secs_f64()
            ),
            CodeReport::History { head, commits } => {
                write!(f, "history at {head}: {commits} commits in 90 days")
            }
            CodeReport::Capped { head, max_mb } => write!(
                f,
                "snapshot {head} is larger than PROBE_SNAPSHOT_MAX_MB ({max_mb} MB): metadata mode until the head moves"
            ),
            CodeReport::Cleared => {
                f.write_str("the mode is below code: the snapshots and the history are deleted")
            }
        }
    }
}

/// `history.json` as `sync` writes it; `serve` reads the same file as [`meta::History`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HistoryFile {
    head: String,
    window_start: DateTime<Utc>,
    commits: Vec<HistoryCommit>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct HistoryCommit {
    sha: String,
    committed_at: DateTime<Utc>,
    files: Vec<String>,
}

fn short(sha: &str) -> String {
    sha.chars().take(7).collect()
}

/// What is known of one pull request between rounds, by its number.
#[derive(Debug, Clone)]
struct PullExtra {
    updated_at: DateTime<Utc>,
    files: Option<Vec<PullFile>>,
    commits: Option<u32>,
}

/// One `sync` for one repository: its state between rounds.
#[derive(Debug)]
pub struct Syncer<S> {
    source: S,
    project: ProjectId,
    projects_file: PathBuf,
    meta: PathBuf,
    repository: Option<Repository>,
    /// When the token last turned out not to read the code (the head or the tarball refused):
    /// nothing that needs Contents is asked until [`TOKEN_RECHECK`] has passed.
    code_refused_at: Option<tokio::time::Instant>,
    /// The head and what the manifest at it came to.
    manifest_at: Option<(String, Result<ManifestFile, String>)>,
    last: Option<(AccessFile, ManifestFile)>,
    pulls: HashMap<u32, PullExtra>,
    /// The runs of the window seen so far, by id: GitHub's run list can come back short for a
    /// round, so a run stays until it leaves the window, and a newer reply updates it.
    runs: HashMap<u64, WorkflowRun>,
    /// The snapshots' folder (`/repo`) and the cap in bytes, once [`Syncer::with_snapshots`] set them.
    snapshots: Option<(PathBuf, u64)>,
    /// The head whose snapshot was larger than the cap: `metadata` until the head moves.
    capped: Option<String>,
    /// Whether a code reader is configured: without one, `code_model` is `code`.
    code_reader: bool,
    /// The head the last [`Syncer::check_mode`] read.
    head: Option<String>,
    history: Option<HistoryFile>,
}

/// `github.json` as `sync` writes it.
#[derive(Serialize)]
struct GitHubFile<'a> {
    fetched_at: DateTime<Utc>,
    repository: &'a Repository,
    pulls: &'a [Pull],
    issues: &'a [Issue],
    workflow_runs: &'a [WorkflowRun],
    releases: &'a [Release],
}

impl<S: SourceAdapter> Syncer<S> {
    pub fn new(source: S, project: ProjectId, projects_file: &Path, meta: &Path) -> Self {
        Self {
            source,
            project,
            projects_file: projects_file.to_owned(),
            meta: meta.to_owned(),
            repository: None,
            code_refused_at: None,
            manifest_at: None,
            last: None,
            pulls: HashMap::new(),
            runs: HashMap::new(),
            snapshots: None,
            capped: None,
            code_reader: false,
            head: None,
            history: None,
        }
    }

    /// Keeps the code snapshots in `repo`, each at most `max_mb` megabytes. Without it, `sync`
    /// writes the metadata only.
    #[must_use]
    pub fn with_snapshots(mut self, repo: &Path, max_mb: u64) -> Self {
        self.snapshots = Some((repo.to_owned(), max_mb.saturating_mul(1024 * 1024)));
        self
    }

    /// Whether a code reader is configured, so that a project that asks for `code_model` gets it.
    #[must_use]
    pub fn with_code_reader(mut self, configured: bool) -> Self {
        self.code_reader = configured;
        self
    }

    /// The mode the last round worked out.
    pub fn mode(&self) -> Option<AccessFile> {
        self.last.as_ref().map(|(access, _)| *access)
    }

    /// Works out the mode and writes `access.json` and `manifest.json` when they changed. On an
    /// error nothing is written and the last files stay.
    pub async fn check_mode(&mut self) -> Result<ModeReport, SyncError> {
        let entry = read_project(&self.projects_file, &self.project)?;
        let repository = self.source.repository().await?;
        let refused_lately = self
            .code_refused_at
            .is_some_and(|at| at.elapsed() < TOKEN_RECHECK);
        let head = if refused_lately {
            None
        } else {
            let head = self.source.head(&repository.default_branch).await?;
            self.code_refused_at = head.is_none().then(tokio::time::Instant::now);
            head
        };
        self.repository = Some(repository);

        let mut manifest_note = None;
        let from_repository = match &head {
            Some(sha) => match &self.manifest_at {
                Some((at, read)) if at == sha => read.clone(),
                _ => {
                    let read = self.read_manifest(sha).await?;
                    self.manifest_at = Some((sha.clone(), read.clone()));
                    read
                }
            },
            None => Err("the token can't read it".to_owned()),
        };
        let file = match from_repository {
            Ok(file) => file,
            Err(note) => {
                manifest_note = Some(format!("the repository's manifest isn't used: {note}"));
                let copy = entry.manifest.clone().ok_or(SyncError::NoManifest)?;
                let manifest = copy.into_manifest(self.project.clone(), entry.display_name.clone());
                dev::compile(&manifest).map_err(SyncError::InvalidCopy)?;
                let json =
                    serde_json::to_vec(&manifest).map_err(|_| SyncError::InvalidCopy("not JSON".into()))?;
                ManifestFile {
                    source: ManifestSource::ProjectsCopy,
                    sha256: probe::sha256(&json),
                    manifest,
                }
            }
        };
        let (mut access, mut access_limited_by) = access::effective(Switches {
            token: (head.is_none()).then_some(AccessMode::Metadata),
            manifest: match file.source {
                ManifestSource::Repository => file.manifest.access,
                // no readable manifest: metadata, whatever the copy says
                ManifestSource::ProjectsCopy => AccessMode::Metadata,
            },
            access_max: Some(entry.access_max),
            code_reader: self.code_reader,
        });
        // a snapshot above the cap lowers it further, until the head moves
        if self.capped.is_some() && self.capped != head {
            self.capped = None;
        }
        if access >= AccessMode::Code && self.capped.is_some() {
            access = AccessMode::Metadata;
            access_limited_by = Some(AccessLimit::SizeCap);
        }
        self.head.clone_from(&head);
        let access = AccessFile {
            access,
            access_limited_by,
        };
        write_if_changed(&self.meta, meta::MANIFEST_FILE, &json_bytes(&file)?)?;
        write_if_changed(&self.meta, meta::ACCESS_FILE, &json_bytes(&access)?)?;
        let changed = self.last.as_ref() != Some(&(access, file.clone()));
        let source = file.source;
        self.last = Some((access, file));
        self.source.save_cache();
        Ok(ModeReport {
            access,
            source,
            head: head.map(|sha| sha.chars().take(7).collect()),
            manifest_note,
            changed,
        })
    }

    /// The manifest at a commit, checked as `dev` checks one: `Err` with a note that names no
    /// content when there is none to use.
    async fn read_manifest(&self, sha: &str) -> Result<Result<ManifestFile, String>, SyncError> {
        let bytes = match self.source.manifest(sha).await? {
            Fetched::Read(bytes) => bytes,
            Fetched::Missing => return Ok(Err("there is none at the head".to_owned())),
            Fetched::Refused => return Ok(Err("the token can't read it".to_owned())),
        };
        if bytes.len() > MAX_MANIFEST_BYTES {
            return Ok(Err("it is larger than 64 KB".to_owned()));
        }
        let manifest: ProbeManifest = match dev::parse_manifest(&bytes) {
            Ok(manifest) => manifest,
            Err(err) => return Ok(Err(err)),
        };
        if manifest.project != self.project {
            return Ok(Err("it names another project".to_owned()));
        }
        if let Err(err) = dev::compile(&manifest) {
            return Ok(Err(err));
        }
        Ok(Ok(ManifestFile {
            source: ManifestSource::Repository,
            sha256: probe::sha256(&bytes),
            manifest,
        }))
    }

    /// In the code modes, the snapshot of the head the last [`Self::check_mode`] read, and the
    /// history with it; below `code`, deletes the snapshots. `None` when there was nothing to do.
    pub async fn refresh_code(&mut self, now: DateTime<Utc>) -> Result<Option<CodeReport>, SyncError> {
        let Some((repo, cap)) = self.snapshots.clone() else {
            return Ok(None);
        };
        let (access, file) = self.last.clone().ok_or(SyncError::NoMode)?;
        let Some(head) = self.head.clone().filter(|_| access.access >= AccessMode::Code) else {
            // below `code` nothing of the code stays: the snapshots, nor the file names of the history
            let snapshots = snapshot::current(&repo).is_some() || repo.join(snapshot::SNAPSHOTS).exists();
            let history = self.forget_history()?;
            if !snapshots && !history {
                return Ok(None);
            }
            if snapshots {
                snapshot::clear(&repo)?;
            }
            return Ok(Some(CodeReport::Cleared));
        };
        let in_use = snapshot::current(&repo).as_deref() == Some(head.as_str());
        let mut made = None;
        if !in_use {
            let started = Instant::now();
            let (_, exclude) = dev::compile(&file.manifest).map_err(SyncError::InvalidCopy)?;
            match self.unpack(&repo, &head, cap, exclude).await {
                Ok(unpacked) => made = Some((unpacked, started)),
                Err(SyncError::Snapshot(SnapshotError::TooLarge(max_mb))) => {
                    return self.cap(&repo, head, max_mb);
                }
                Err(SyncError::Source(SourceError::Forbidden(_))) => {
                    // a refused tarball means metadata too: the token's test said otherwise
                    self.code_refused_at = Some(tokio::time::Instant::now());
                    return Err(SyncError::Source(SourceError::Forbidden("the snapshot")));
                }
                Err(err) => return Err(err),
            }
        } else if self.history.as_ref().is_some_and(|h| h.head == head) {
            return Ok(None);
        }
        let commits = self.update_history(&head, now).await?;
        self.source.save_cache();
        if let Some((unpacked, started)) = made {
            snapshot::switch(&repo, &head)?;
            Ok(Some(CodeReport::Snapshot {
                head: short(&head),
                unpacked,
                commits,
                took: started.elapsed(),
            }))
        } else {
            Ok(Some(CodeReport::History {
                head: short(&head),
                commits,
            }))
        }
    }

    /// The tarball, from the host's reply straight into the unpacker on a blocking thread.
    async fn unpack(
        &self,
        repo: &Path,
        head: &str,
        cap: u64,
        exclude: globset::GlobSet,
    ) -> Result<Unpacked, SyncError> {
        let mut tarball = match self.source.tarball(head).await? {
            Fetched::Read(tarball) => tarball,
            Fetched::Refused => return Err(SyncError::Source(SourceError::Forbidden("the snapshot"))),
            Fetched::Missing => return Err(SyncError::Source(SourceError::NotFound("the snapshot"))),
        };
        let (sender, reader) = snapshot::chunks();
        let (repo, commit) = (repo.to_owned(), head.to_owned());
        let unpacking =
            tokio::task::spawn_blocking(move || snapshot::make(&repo, &commit, reader, cap, &exclude));
        let mut broke = None;
        while let Some(chunk) = tarball.next().await {
            match chunk {
                // the unpacker stopped (the cap, a write): it says why
                Ok(chunk) => {
                    if sender.send(Ok(chunk)).await.is_err() {
                        break;
                    }
                }
                Err(err) => {
                    let _ = sender.send(Err(())).await;
                    broke = Some(err);
                    break;
                }
            }
        }
        drop(sender);
        let made = unpacking
            .await
            .map_err(|_| SyncError::Snapshot(SnapshotError::Write("the unpacker")))?;
        match (broke, made) {
            (Some(err), _) => Err(err.into()),
            (None, made) => Ok(made?),
        }
    }

    /// The snapshot was larger than the cap: `metadata` with `size_cap` until the head moves, and
    /// no snapshot left.
    fn cap(&mut self, repo: &Path, head: String, max_mb: u64) -> Result<Option<CodeReport>, SyncError> {
        snapshot::clear(repo)?;
        let (_, file) = self.last.clone().ok_or(SyncError::NoMode)?;
        let access = AccessFile {
            access: AccessMode::Metadata,
            access_limited_by: Some(AccessLimit::SizeCap),
        };
        write_if_changed(&self.meta, meta::ACCESS_FILE, &json_bytes(&access)?)?;
        self.last = Some((access, file));
        self.capped = Some(head.clone());
        self.forget_history()?;
        Ok(Some(CodeReport::Capped {
            head: short(&head),
            max_mb,
        }))
    }

    /// Drops the history, in memory and `history.json` in `/meta`: its file names are the code's,
    /// which `metadata` mode keeps nowhere. Whether there was a file to delete.
    fn forget_history(&mut self) -> Result<bool, SyncError> {
        self.history = None;
        match fs::remove_file(self.meta.join(meta::HISTORY_FILE)) {
            Ok(()) => Ok(true),
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => Ok(false),
            Err(_) => Err(SyncError::Write(meta::HISTORY_FILE)),
        }
    }

    /// `history.json` at `head`: a compare from the last head when there is one, it reaches back the
    /// whole window and the host can answer it, the window's commits otherwise; each new commit's
    /// file names; nothing older than the window. Returns how many commits it holds.
    async fn update_history(&mut self, head: &str, now: DateTime<Utc>) -> Result<usize, SyncError> {
        let window_start = now - WINDOW;
        let branch = self
            .repository
            .as_ref()
            .ok_or(SyncError::NoMode)?
            .default_branch
            .clone();
        let last = match self.history.take() {
            Some(history) => Some(history),
            // after a restart, the file the last run wrote
            None => meta::read_json::<HistoryFile>(&self.meta, meta::HISTORY_FILE).ok(),
        }
        // a history that starts inside the window covers only part of it, so it is fetched afresh
        .filter(|last| last.window_start <= window_start);
        let mut commits = match last {
            Some(last) if last.head == head => last.commits,
            Some(last) => match self.source.compare(&last.head, head).await? {
                Some(new) => {
                    let mut commits = self.with_files(new).await?;
                    commits.extend(last.commits);
                    commits
                }
                None => {
                    let all = self.source.commits(&branch, window_start).await?;
                    self.with_files(all).await?
                }
            },
            None => {
                let all = self.source.commits(&branch, window_start).await?;
                self.with_files(all).await?
            }
        };
        commits.retain(|c| c.committed_at >= window_start);
        commits.sort_by(|a, b| {
            b.committed_at
                .cmp(&a.committed_at)
                .then_with(|| a.sha.cmp(&b.sha))
        });
        commits.dedup_by(|a, b| a.sha == b.sha);
        let history = HistoryFile {
            head: head.to_owned(),
            window_start,
            commits,
        };
        write_atomic(&self.meta, meta::HISTORY_FILE, &json_bytes(&history)?)?;
        let count = history.commits.len();
        self.history = Some(history);
        Ok(count)
    }

    /// Each commit's file names, [`PER_PR_AT_ONCE`] requests at a time.
    async fn with_files(&self, commits: Vec<CommitRef>) -> Result<Vec<HistoryCommit>, SyncError> {
        let source = &self.source;
        let found: Vec<HistoryCommit> = stream::iter(commits)
            .map(|commit| async move {
                let files = source.commit_files(&commit.sha).await?;
                Ok::<_, SourceError>(HistoryCommit {
                    sha: commit.sha,
                    committed_at: commit.committed_at,
                    files,
                })
            })
            .buffered(PER_PR_AT_ONCE)
            .try_collect()
            .await?;
        Ok(found)
    }

    /// Fetches the metadata in the mode the last [`Self::check_mode`] worked out and writes
    /// `github.json`. The windows count back from `now`, which becomes its `fetched_at`.
    pub async fn refresh_meta(&mut self, now: DateTime<Utc>) -> Result<MetaReport, SyncError> {
        let started = Instant::now();
        let mode = self.mode().ok_or(SyncError::NoMode)?.access;
        let repository = self.repository.clone().ok_or(SyncError::NoMode)?;
        let since = now - WINDOW;
        let (mut pulls, issues, fetched_runs, releases) = tokio::try_join!(
            self.source.pulls(since),
            self.source.issues(since),
            self.source.workflow_runs(&repository.default_branch, since),
            self.source.releases(),
        )?;

        // the date filter of GitHub's run list is a search, which can answer short for a round
        self.runs.retain(|_, run| run.created_at >= since);
        self.runs
            .extend(fetched_runs.into_iter().map(|run| (run.id, run)));
        let mut workflow_runs: Vec<WorkflowRun> = self.runs.values().cloned().collect();
        workflow_runs.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));

        let code = mode >= AccessMode::Code;
        let wanted = |pull: &Pull| code || pull.merged_at.is_some_and(|at| at >= since);
        let stale: Vec<u32> = pulls
            .iter()
            .filter(|pull| wanted(pull))
            .filter(|pull| {
                self.pulls.get(&pull.number).is_none_or(|extra| {
                    extra.updated_at != pull.updated_at
                        || (code && extra.files.is_none())
                        || (!code && extra.commits.is_none())
                })
            })
            .map(|pull| pull.number)
            .collect();
        let source = &self.source;
        let fetched: Vec<(u32, Option<Vec<PullFile>>, Option<u32>)> = stream::iter(stale)
            .map(|number| async move {
                if code {
                    Ok::<_, SourceError>((number, Some(source.pull_files(number).await?), None))
                } else {
                    Ok((number, None, source.pull_commits(number).await?))
                }
            })
            .buffer_unordered(PER_PR_AT_ONCE)
            .try_collect()
            .await?;
        let pulls_fetched = fetched.len();
        let updated: HashMap<u32, DateTime<Utc>> = pulls.iter().map(|p| (p.number, p.updated_at)).collect();
        for (number, files, commits) in fetched {
            let extra = self.pulls.entry(number).or_insert(PullExtra {
                updated_at: updated[&number],
                files: None,
                commits: None,
            });
            extra.updated_at = updated[&number];
            if files.is_some() {
                extra.files = files;
            }
            if commits.is_some() {
                extra.commits = commits;
            }
        }
        self.pulls.retain(|number, _| updated.contains_key(number));

        for pull in &mut pulls {
            pull.keep_links_only();
            let extra = self.pulls.get(&pull.number);
            // file lists only in the code modes: a drop to metadata takes them out at once
            pull.files = extra.and_then(|e| e.files.clone()).filter(|_| code);
            pull.commits = extra.and_then(|e| e.commits).filter(|_| !code);
        }
        let file = GitHubFile {
            fetched_at: now,
            repository: &repository,
            pulls: &pulls,
            issues: &issues,
            workflow_runs: &workflow_runs,
            releases: &releases,
        };
        write_atomic(&self.meta, meta::GITHUB_FILE, &json_bytes(&file)?)?;
        self.source.save_cache();
        Ok(MetaReport {
            pulls: pulls.len(),
            issues: issues.len(),
            runs: workflow_runs.len(),
            releases: releases.len(),
            pulls_fetched,
            took: started.elapsed(),
        })
    }
}

/// This project's entry of `projects.yaml`, and nothing else of the file: another project's entry,
/// valid or not, is never read. The errors give a position or the project, never what is there.
fn read_project(path: &Path, project: &ProjectId) -> Result<ProjectConfig, SyncError> {
    let yaml = fs::read_to_string(path)
        .map_err(|err| SyncError::Projects(format!("cannot read projects.yaml: {}", err.kind())))?;
    let document: serde_norway::Value = serde_norway::from_str(&yaml).map_err(|err| {
        SyncError::Projects(match err.location() {
            Some(at) => format!(
                "projects.yaml is invalid at line {}, column {}",
                at.line(),
                at.column()
            ),
            None => "projects.yaml is invalid".to_owned(),
        })
    })?;
    let entry = document
        .get("projects")
        .and_then(|projects| projects.get(project.as_str()))
        .ok_or_else(|| SyncError::NotListed(project.clone()))?;
    serde_norway::from_value(entry.clone())
        .map_err(|_| SyncError::Projects(format!("projects.yaml's entry for {project} is invalid")))
}

fn json_bytes<T: Serialize>(value: &T) -> Result<Vec<u8>, SyncError> {
    let mut bytes = serde_json::to_vec_pretty(value).map_err(|_| SyncError::Write("JSON"))?;
    bytes.push(b'\n');
    Ok(bytes)
}

/// Writes `name` in `dir` unless it already holds exactly these bytes, so an unchanged mode doesn't
/// move `serve`'s fingerprint.
fn write_if_changed(dir: &Path, name: &'static str, bytes: &[u8]) -> Result<(), SyncError> {
    if fs::read(dir.join(name)).is_ok_and(|old| old == bytes) {
        return Ok(());
    }
    write_atomic(dir, name, bytes)
}

/// Writes to a temporary name in the same folder, then renames: a reader sees the old file or the
/// new one, never half of one.
fn write_atomic(dir: &Path, name: &'static str, bytes: &[u8]) -> Result<(), SyncError> {
    let temporary = dir.join(format!(".{name}.tmp"));
    fs::write(&temporary, bytes).map_err(|_| SyncError::Write(name))?;
    fs::rename(&temporary, dir.join(name)).map_err(|_| {
        let _ = fs::remove_file(&temporary);
        SyncError::Write(name)
    })
}

/// The loop: the mode every `code_interval`, the metadata every `meta_interval` and at once when
/// the mode changed. A failed round is logged and the next tick tries again; the last files stay.
pub async fn run<S: SourceAdapter>(mut syncer: Syncer<S>, config: &SyncConfig, log: impl Fn(&str)) {
    let mut ticks = tokio::time::interval(config.code_interval);
    ticks.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
    let mut last_meta: Option<tokio::time::Instant> = None;
    let mut last_error: Option<String> = None;
    loop {
        let tick = ticks.tick().await;
        let report = match syncer.check_mode().await {
            Ok(report) => report,
            Err(err) => {
                report_error(&log, &mut last_error, &err);
                continue;
            }
        };
        if report.changed {
            log(&report.to_string());
        }
        let mut changed = report.changed;
        match syncer.refresh_code(Utc::now()).await {
            Ok(Some(code)) => {
                log(&code.to_string());
                changed |= code.changed_mode();
            }
            Ok(None) => {}
            Err(err) => report_error(&log, &mut last_error, &err),
        }
        let since = last_meta.map(|at| tick.duration_since(at));
        if !(meta_due(since, config) || changed) {
            continue;
        }
        match syncer.refresh_meta(Utc::now()).await {
            Ok(meta) => {
                // one line per round: the counts and the time it took, so a stalled sync shows
                log(&meta.to_string());
                last_meta = Some(tick);
                last_error = None;
            }
            Err(err) => report_error(&log, &mut last_error, &err),
        }
    }
}

/// Whether a tick refreshes the metadata: the first one, then the first at which `meta_interval`
/// has passed since the tick of the last round, give or take half a tick, so 300 s on 60 s ticks
/// is every fifth tick.
fn meta_due(since_last: Option<Duration>, config: &SyncConfig) -> bool {
    since_last.is_none_or(|since| since + config.code_interval / 2 >= config.meta_interval)
}

/// An error is logged once until it changes or a round succeeds.
fn report_error(log: &impl Fn(&str), last: &mut Option<String>, err: &SyncError) {
    let message = format!("the round wrote nothing: {err}");
    if last.as_deref() != Some(message.as_str()) {
        log(&message);
        *last = Some(message);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn intervals_take_seconds_or_minutes() {
        let at = |v: &str| interval("X", Some(v.to_owned()), 60);
        assert_eq!(at("60s"), Ok(Duration::from_secs(60)));
        assert_eq!(at("300"), Ok(Duration::from_secs(300)));
        assert_eq!(at("5m"), Ok(Duration::from_secs(300)));
        assert_eq!(interval("X", None, 60), Ok(Duration::from_secs(60)));
        assert_eq!(interval("X", Some(" ".into()), 300), Ok(Duration::from_secs(300)));
        for bad in ["5s", "0", "-1s", "1h", "sixty", "60 s"] {
            assert!(at(bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_environment_names_the_project_the_repository_and_the_token_file() {
        let env = |pairs: &[(&'static str, &'static str)]| {
            let pairs = pairs.to_vec();
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let proxy = ("HTTPS_PROXY", "http://egress-proxy:8888");
        let full = [
            ("PROBE_PROJECT", "delta"),
            ("PROBE_GITHUB_REPO", "your-org/delta"),
            ("PROBE_GITHUB_TOKEN_FILE", "/run/secrets/delta_github_token"),
            ("PROBE_CODE_INTERVAL", "60s"),
            ("PROBE_META_INTERVAL", "300s"),
            proxy,
        ];
        let config = SyncConfig::from_env(env(&full)).unwrap();
        assert_eq!(config.project.as_str(), "delta");
        assert_eq!(config.meta_interval, Duration::from_secs(300));
        assert_eq!(config.snapshot_max_mb, 512, "the default");
        let capped = [full[0], full[1], full[2], proxy, ("PROBE_SNAPSHOT_MAX_MB", "64")];
        assert_eq!(SyncConfig::from_env(env(&capped)).unwrap().snapshot_max_mb, 64);
        for bad in ["0", "64MB", "-1"] {
            let pairs = [full[0], full[1], full[2], proxy, ("PROBE_SNAPSHOT_MAX_MB", bad)];
            assert!(SyncConfig::from_env(env(&pairs)).is_err(), "{bad}");
        }
        assert!(
            SyncConfig::from_env(env(&full[1..]))
                .unwrap_err()
                .contains("PROBE_PROJECT")
        );
        let bad = [("PROBE_PROJECT", "Delta"), full[1], full[2], proxy];
        assert!(SyncConfig::from_env(env(&bad)).is_err());
    }

    #[test]
    fn sync_refuses_to_start_without_its_proxy() {
        let env = |pairs: Vec<(&'static str, &'static str)>| {
            move |name: &str| {
                pairs
                    .iter()
                    .find(|(k, _)| *k == name)
                    .map(|(_, v)| (*v).to_owned())
            }
        };
        let base = vec![
            ("PROBE_PROJECT", "delta"),
            ("PROBE_GITHUB_REPO", "your-org/delta"),
            ("PROBE_GITHUB_TOKEN_FILE", "/run/secrets/delta_github_token"),
        ];
        let err = SyncConfig::from_env(env(base.clone())).unwrap_err();
        assert!(err.contains("HTTPS_PROXY"), "{err}");
        let mut blank = base.clone();
        blank.push(("HTTPS_PROXY", "  "));
        assert!(
            SyncConfig::from_env(env(blank)).is_err(),
            "an empty proxy is no proxy"
        );
        let mut lower = base;
        lower.push(("https_proxy", "http://egress-proxy:8888"));
        assert!(
            SyncConfig::from_env(env(lower)).is_ok(),
            "the lower-case name counts too"
        );
    }

    /// A host behind the seam that answers each round's run list from a script, and nothing else.
    struct ScriptedRuns(std::sync::Mutex<Vec<Vec<WorkflowRun>>>);

    impl SourceAdapter for ScriptedRuns {
        async fn repository(&self) -> Result<Repository, SourceError> {
            Ok(Repository {
                full_name: "your-org/delta".into(),
                default_branch: "main".into(),
            })
        }
        async fn head(&self, _: &str) -> Result<Option<String>, SourceError> {
            Ok(None)
        }
        async fn manifest(&self, _: &str) -> Result<Fetched<Vec<u8>>, SourceError> {
            Ok(Fetched::Refused)
        }
        async fn pulls(&self, _: DateTime<Utc>) -> Result<Vec<Pull>, SourceError> {
            Ok(Vec::new())
        }
        async fn issues(&self, _: DateTime<Utc>) -> Result<Vec<Issue>, SourceError> {
            Ok(Vec::new())
        }
        async fn workflow_runs(&self, _: &str, _: DateTime<Utc>) -> Result<Vec<WorkflowRun>, SourceError> {
            Ok(self.0.lock().unwrap().remove(0))
        }
        async fn releases(&self) -> Result<Vec<Release>, SourceError> {
            Ok(Vec::new())
        }
        async fn pull_files(&self, _: u32) -> Result<Vec<PullFile>, SourceError> {
            Ok(Vec::new())
        }
        async fn tarball(&self, _: &str) -> Result<Fetched<source::Tarball>, SourceError> {
            Ok(Fetched::Refused)
        }
        async fn commits(&self, _: &str, _: DateTime<Utc>) -> Result<Vec<CommitRef>, SourceError> {
            Ok(Vec::new())
        }
        async fn commit_files(&self, _: &str) -> Result<Vec<String>, SourceError> {
            Ok(Vec::new())
        }
        async fn compare(&self, _: &str, _: &str) -> Result<Option<Vec<CommitRef>>, SourceError> {
            Ok(None)
        }
        async fn pull_commits(&self, _: u32) -> Result<Option<u32>, SourceError> {
            Ok(None)
        }
    }

    /// A host whose head answers from a script, one answer per round, and counts how often it was
    /// asked; its manifest asks for `code`.
    struct ScriptedHeads {
        heads: std::sync::Mutex<Vec<Option<String>>>,
        asked: std::sync::atomic::AtomicUsize,
    }

    impl SourceAdapter for ScriptedHeads {
        async fn repository(&self) -> Result<Repository, SourceError> {
            Ok(Repository {
                full_name: "your-org/delta".into(),
                default_branch: "main".into(),
            })
        }
        async fn head(&self, _: &str) -> Result<Option<String>, SourceError> {
            self.asked.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            Ok(self.heads.lock().unwrap().remove(0))
        }
        async fn manifest(&self, _: &str) -> Result<Fetched<Vec<u8>>, SourceError> {
            Ok(Fetched::Read(
                b"version: 0.1\nproject: delta\ndisplay_name: Delta\nallow: [progress]\njudge_context: []\n"
                    .to_vec(),
            ))
        }
        async fn pulls(&self, _: DateTime<Utc>) -> Result<Vec<Pull>, SourceError> {
            Ok(Vec::new())
        }
        async fn issues(&self, _: DateTime<Utc>) -> Result<Vec<Issue>, SourceError> {
            Ok(Vec::new())
        }
        async fn workflow_runs(&self, _: &str, _: DateTime<Utc>) -> Result<Vec<WorkflowRun>, SourceError> {
            Ok(Vec::new())
        }
        async fn releases(&self) -> Result<Vec<Release>, SourceError> {
            Ok(Vec::new())
        }
        async fn pull_files(&self, _: u32) -> Result<Vec<PullFile>, SourceError> {
            Ok(Vec::new())
        }
        async fn tarball(&self, _: &str) -> Result<Fetched<source::Tarball>, SourceError> {
            Ok(Fetched::Refused)
        }
        async fn commits(&self, _: &str, _: DateTime<Utc>) -> Result<Vec<CommitRef>, SourceError> {
            Ok(Vec::new())
        }
        async fn commit_files(&self, _: &str) -> Result<Vec<String>, SourceError> {
            Ok(Vec::new())
        }
        async fn compare(&self, _: &str, _: &str) -> Result<Option<Vec<CommitRef>>, SourceError> {
            Ok(None)
        }
        async fn pull_commits(&self, _: u32) -> Result<Option<u32>, SourceError> {
            Ok(None)
        }
    }

    /// One refused or lost head doesn't hold the probe in `metadata` for the rest of the run: the
    /// head isn't asked again at once, but it is after [`TOKEN_RECHECK`].
    #[tokio::test(start_paused = true)]
    async fn a_token_that_could_not_read_the_code_is_asked_again_after_a_while() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects.yaml");
        fs::write(
            &projects,
            "projects:\n  delta:\n    display_name: Delta\n    access_max: code\n    manifest:\n      allow: [progress]\n      judge_context: []\n    probe:\n      url: http://probe-delta:7000/mcp\n      token_file: /run/x\n",
        )
        .unwrap();
        let source = ScriptedHeads {
            heads: std::sync::Mutex::new(vec![
                None,
                Some("5f1c0e7a9b2d4c6e8f0a1b3c5d7e9f1a2b4c6d8e".to_owned()),
            ]),
            asked: std::sync::atomic::AtomicUsize::new(0),
        };
        let mut sync = Syncer::new(source, ProjectId::parse("delta").unwrap(), &projects, dir.path());
        let asked =
            |sync: &Syncer<ScriptedHeads>| sync.source.asked.load(std::sync::atomic::Ordering::SeqCst);
        let mode = |report: &ModeReport| (report.access.access, report.access.access_limited_by);

        let first = sync.check_mode().await.unwrap();
        assert_eq!(mode(&first), (AccessMode::Metadata, Some(AccessLimit::Token)));
        tokio::time::advance(TOKEN_RECHECK - Duration::from_secs(1)).await;
        let soon = sync.check_mode().await.unwrap();
        assert_eq!(mode(&soon), (AccessMode::Metadata, Some(AccessLimit::Token)));
        assert_eq!(asked(&sync), 1, "not asked again before the interval");

        tokio::time::advance(Duration::from_secs(2)).await;
        let later = sync.check_mode().await.unwrap();
        assert_eq!(asked(&sync), 2, "asked again once the interval passed");
        assert_eq!(mode(&later), (AccessMode::Code, None));
        assert!(later.changed);
        assert_eq!(later.source, ManifestSource::Repository);
    }

    /// Below `code` the history goes with the snapshots: no file name of the last 90 days stays on
    /// the persistent `/meta`, even when there is no snapshot to delete (after a restart, say).
    #[tokio::test]
    async fn below_code_the_history_left_in_meta_is_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let (meta, repo) = (dir.path().join("meta"), dir.path().join("repo"));
        fs::create_dir_all(&meta).unwrap();
        fs::create_dir_all(&repo).unwrap();
        let projects = dir.path().join("projects.yaml");
        fs::write(
            &projects,
            "projects:\n  delta:\n    display_name: Delta\n    access_max: code\n    manifest:\n      allow: [progress]\n      judge_context: []\n    probe:\n      url: http://probe-delta:7000/mcp\n      token_file: /run/x\n",
        )
        .unwrap();
        fs::write(
            meta.join(meta::HISTORY_FILE),
            r#"{"head": "0a1b2c3", "commits": []}"#,
        )
        .unwrap();
        let source = ScriptedHeads {
            heads: std::sync::Mutex::new(vec![None, None]),
            asked: std::sync::atomic::AtomicUsize::new(0),
        };
        let mut sync = Syncer::new(source, ProjectId::parse("delta").unwrap(), &projects, &meta)
            .with_snapshots(&repo, 512);
        let mode = sync.check_mode().await.unwrap();
        assert_eq!(mode.access.access, AccessMode::Metadata);
        let now: DateTime<Utc> = "2026-09-30T12:00:00Z".parse().unwrap();
        let report = sync.refresh_code(now).await.unwrap();
        assert_eq!(report, Some(CodeReport::Cleared));
        assert!(!meta.join(meta::HISTORY_FILE).exists());
        // nothing is left to delete the next time
        assert_eq!(sync.refresh_code(now).await.unwrap(), None);
    }

    fn run(id: u64, created: &str, conclusion: &str) -> WorkflowRun {
        WorkflowRun {
            id,
            name: Some("build".into()),
            head_branch: Some("main".into()),
            head_sha: "a".repeat(40),
            status: Some("completed".into()),
            conclusion: Some(conclusion.into()),
            run_attempt: 1,
            created_at: created.parse().unwrap(),
            updated_at: created.parse().unwrap(),
        }
    }

    #[tokio::test]
    async fn a_short_run_list_keeps_the_runs_an_earlier_round_saw() {
        let dir = tempfile::tempdir().unwrap();
        let projects = dir.path().join("projects.yaml");
        fs::write(
            &projects,
            "projects:\n  delta:\n    display_name: Delta\n    access_max: code\n    manifest:\n      allow: [progress]\n      judge_context: []\n    probe:\n      url: http://probe-delta:7000/mcp\n      token_file: /run/x\n",
        )
        .unwrap();
        let rounds = vec![
            vec![
                run(3, "2026-09-30T10:00:00Z", "failure"),
                run(2, "2026-09-29T10:00:00Z", "success"),
                run(1, "2026-09-01T10:00:00Z", "success"),
            ],
            // a short answer that also updates run 3
            vec![run(3, "2026-09-30T10:00:00Z", "success")],
            Vec::new(),
        ];
        let source = ScriptedRuns(std::sync::Mutex::new(rounds));
        let mut sync = Syncer::new(source, ProjectId::parse("delta").unwrap(), &projects, dir.path());
        sync.check_mode().await.unwrap();
        let runs = |dir: &Path| -> Vec<(u64, String)> {
            let github: serde_json::Value = meta::read_json(dir, meta::GITHUB_FILE).unwrap();
            github["workflow_runs"]
                .as_array()
                .unwrap()
                .iter()
                .map(|r| {
                    (
                        r["id"].as_u64().unwrap(),
                        r["conclusion"].as_str().unwrap().to_owned(),
                    )
                })
                .collect()
        };
        let then: DateTime<Utc> = "2026-09-30T12:00:00Z".parse().unwrap();
        sync.refresh_meta(then).await.unwrap();
        assert_eq!(runs(dir.path()).len(), 3);
        sync.refresh_meta(then).await.unwrap();
        assert_eq!(
            runs(dir.path()),
            vec![
                (3, "success".to_owned()),
                (2, "success".to_owned()),
                (1, "success".to_owned())
            ]
        );
        // 90 days on, the runs have left the window
        sync.refresh_meta(then + WINDOW + chrono::Duration::days(1))
            .await
            .unwrap();
        assert!(runs(dir.path()).is_empty());
    }

    #[test]
    fn the_metadata_is_due_once_its_interval_has_passed_on_the_ticks() {
        let config = |code: u64, meta: u64| SyncConfig {
            project: ProjectId::parse("delta").unwrap(),
            repo: "your-org/delta".into(),
            token_file: "/run/secrets/token".into(),
            code_interval: Duration::from_secs(code),
            meta_interval: Duration::from_secs(meta),
            snapshot_max_mb: 512,
            code_reader: false,
        };
        let due_ticks = |code: u64, meta: u64| {
            let config = config(code, meta);
            let mut last = None;
            let mut due = Vec::new();
            for tick in 0..=10u64 {
                if meta_due(last.map(|at| Duration::from_secs((tick - at) * code)), &config) {
                    due.push(tick);
                    last = Some(tick);
                }
            }
            due
        };
        assert_eq!(due_ticks(60, 300), vec![0, 5, 10]);
        assert_eq!(due_ticks(60, 60), (0..=10).collect::<Vec<_>>());
        assert_eq!(due_ticks(60, 600), vec![0, 10]);
    }

    #[test]
    fn a_write_replaces_the_file_whole() {
        let dir = tempfile::tempdir().unwrap();
        write_atomic(dir.path(), meta::ACCESS_FILE, b"one").unwrap();
        write_if_changed(dir.path(), meta::ACCESS_FILE, b"two").unwrap();
        assert_eq!(fs::read(dir.path().join(meta::ACCESS_FILE)).unwrap(), b"two");
        let names: Vec<_> = fs::read_dir(dir.path())
            .unwrap()
            .map(|e| e.unwrap().file_name())
            .collect();
        assert_eq!(names.len(), 1, "no temporary file is left: {names:?}");
    }
}
