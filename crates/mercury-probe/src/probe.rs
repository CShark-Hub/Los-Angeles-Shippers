//! What a probe serves from: its manifest, its mode and, in the code modes, its files, whether
//! `dev` read them from a local folder or `serve` from what `sync` wrote; and the index kept in
//! step with them.

use std::collections::hash_map::DefaultHasher;
use std::fs;
use std::hash::{Hash, Hasher};
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use globset::GlobSet;
use mercury_schema::ids::Sha256Hex;
use mercury_schema::manifest::ProbeManifest;
use mercury_schema::{AccessLimit, AccessMode, ProjectId, Redactor};
use sha2::{Digest, Sha256};

use crate::facts::{self, Inputs, Rows};
use crate::index::Index;
use crate::judging::{JudgeClient, Judging, MAX_RETRY_AFTER, Report};
use crate::meta::{self, Meta};

/// A probe loaded and ready to index and serve.
#[derive(Debug)]
pub struct Probe {
    pub manifest: ProbeManifest,
    /// The manifest's redaction rules, compiled.
    pub redactor: Redactor,
    pub access: AccessMode,
    pub access_limited_by: Option<AccessLimit>,
    /// The SHA-256 of the manifest file's bytes as read, which registration reports.
    pub manifest_hash: Sha256Hex,
    /// The files under `repo`, relative and with `/` separators, in the code modes only.
    pub files: Option<Vec<String>>,
    /// The folder the files are listed from.
    pub repo: PathBuf,
    pub meta: PathBuf,
    /// The manifest's `exclude` globs, compiled.
    pub exclude: GlobSet,
}

impl Probe {
    /// A probe whose mode is `code_model` with no code reader to call is `code`, with
    /// `no_code_reader` as the reason.
    #[must_use]
    pub fn needing_a_reader(mut self, code_reader: bool) -> Self {
        if !code_reader && self.access == AccessMode::CodeModel {
            self.access = AccessMode::Code;
            self.access_limited_by = Some(AccessLimit::NoCodeReader);
        }
        self
    }

    /// The index rows of the files and the metadata as they are now.
    pub fn rows(&self, now: DateTime<Utc>) -> Result<Rows, String> {
        let meta = Meta::load(&self.meta)?;
        facts::build(
            &Inputs {
                manifest: &self.manifest,
                redactor: &self.redactor,
                access: self.access,
                access_limited_by: self.access_limited_by,
                files: self.files.as_deref().map(|files| (self.repo.as_path(), files)),
                exclude: &self.exclude,
                meta: &meta,
            },
            now,
        )
    }

    /// A fingerprint of everything the index is built from: the manifest's hash and the mode, the
    /// two metadata files' sizes and times, and in the code modes the folder and its listed files'
    /// names, sizes and times. In `metadata` mode the folder isn't listed, so nothing of it is read.
    pub fn fingerprint(&self) -> u64 {
        let mut hasher = DefaultHasher::new();
        self.manifest_hash.as_str().hash(&mut hasher);
        self.access.hash(&mut hasher);
        self.access_limited_by.hash(&mut hasher);
        for name in [meta::GITHUB_FILE, meta::HISTORY_FILE] {
            stamp(&self.meta.join(name), &mut hasher);
        }
        if let Some(files) = &self.files {
            self.repo.hash(&mut hasher);
            for file in files {
                stamp(&self.repo.join(file), &mut hasher);
                file.hash(&mut hasher);
            }
        }
        hasher.finish()
    }
}

/// The lowercase hexadecimal SHA-256 of some bytes.
pub fn sha256(bytes: &[u8]) -> Sha256Hex {
    let hex: String = Sha256::digest(bytes).iter().map(|b| format!("{b:02x}")).collect();
    #[allow(
        clippy::expect_used,
        reason = "64 lowercase hexadecimal digits by construction"
    )]
    Sha256Hex::parse(&hex).expect("a SHA-256 in hexadecimal")
}

/// A file's size and modification time into a fingerprint; a missing file hashes as missing.
fn stamp(path: &Path, hasher: &mut DefaultHasher) {
    match fs::metadata(path) {
        Ok(m) => {
            m.len().hash(hasher);
            m.modified()
                .ok()
                .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
                .map(|d| d.as_nanos())
                .hash(hasher);
        }
        Err(_) => u64::MAX.hash(hasher),
    }
}

/// Where a probe is loaded from.
#[derive(Debug, Clone)]
pub enum Source {
    /// `dev`: a local folder with its manifest, the metadata beside it, and `--access` standing in
    /// for the git host token.
    Dev {
        repo: PathBuf,
        meta: PathBuf,
        token: Option<AccessMode>,
    },
    /// `serve`: `/meta` as `sync` writes it and the snapshots under `/repo`; `project` is the one
    /// the manifest must name, when it is set.
    Serve {
        repo: PathBuf,
        meta: PathBuf,
        project: Option<ProjectId>,
    },
}

impl Source {
    pub fn load(&self) -> Result<Probe, String> {
        self.load_with(false)
    }

    /// The probe as it is now, given whether a code reader is configured here: `dev` works the
    /// mode out with it, and `serve`, which reads the mode `sync` wrote, serves a `code_model` it
    /// has no reader for as `code`, so it never registers a tool it can't call.
    pub fn load_with(&self, code_reader: bool) -> Result<Probe, String> {
        match self {
            Source::Dev { repo, meta, token } => Probe::dev_with(repo, meta, *token, code_reader),
            Source::Serve { repo, meta, project } => {
                Probe::serve(repo, meta, project.as_ref()).map(|probe| probe.needing_a_reader(code_reader))
            }
        }
    }

    /// The core's internal address, `PROBE_CORE_INTERNAL_URL`, which the judge's questions and
    /// registration go to. `serve` doesn't start without it, as `sync` doesn't without its proxy:
    /// with no core to ask, its titles would be the rules' alone. Only `dev` runs without a core.
    pub fn core_address(&self, var: impl Fn(&str) -> Option<String>) -> Result<Option<String>, String> {
        let core = var("PROBE_CORE_INTERNAL_URL")
            .map(|v| v.trim().to_owned())
            .filter(|v| !v.is_empty());
        match (self, core) {
            (Source::Serve { .. }, None) => Err(
                "PROBE_CORE_INTERNAL_URL is not set: serve asks the core's judge about every title, and never shows one on its rules alone"
                    .to_owned(),
            ),
            (_, core) => Ok(core),
        }
    }
}

/// How often `dev` and `serve` check whether what the index is built from changed.
pub const REFRESH_EVERY: Duration = Duration::from_secs(30);

/// How long titles the core answered with its rules alone wait before they are asked again: while
/// no judge answers, a probe sends at most 12 such rounds an hour, not one every tick.
pub const RULES_RECHECK: Duration = Duration::from_secs(5 * 60);

/// Keeps an index in step with its source: [`Self::refresh`] rebuilds it when the fingerprint moved,
/// when a request to the core's judge got no answer at the last rebuild, or when titles got only
/// the core's rules' answer and [`RULES_RECHECK`] has passed since.
#[derive(Debug)]
pub struct LiveIndex {
    pub index: Index,
    source: Source,
    fingerprint: Option<u64>,
    /// The fingerprint of a probe served from the last index instead of rebuilding it (a code
    /// mode's snapshot not there yet).
    kept: Option<u64>,
    /// The core's judge, when the probe has a core to ask.
    judging: Option<Judging>,
    /// What the last rebuild's questions to the judge came to.
    judged: Option<Report>,
    /// When the index was last rebuilt.
    rebuilt_at: Option<DateTime<Utc>>,
    /// Whether a code reader is configured here (`PROBE_CODE_READER_URL`).
    code_reader: bool,
}

impl LiveIndex {
    pub fn new(index: Index, source: Source) -> Self {
        Self {
            index,
            source,
            fingerprint: None,
            kept: None,
            judging: None,
            judged: None,
            rebuilt_at: None,
            code_reader: false,
        }
    }

    /// The same, with a code reader configured: `dev` runs `code_model` when its manifest says so,
    /// and `serve` keeps the `code_model` `sync` wrote.
    #[must_use]
    pub fn with_code_reader(mut self, configured: bool) -> Self {
        self.code_reader = configured;
        self
    }

    /// The same, asking the core's judge what the rules leave open at every rebuild.
    pub fn with_judge(mut self, client: JudgeClient) -> Self {
        self.judging = Some(Judging::new(client));
        self
    }

    /// What the last rebuild's questions to the judge came to; `None` without a judge.
    pub fn judged(&self) -> Option<&Report> {
        self.judged.as_ref()
    }

    /// Whether the last rebuild's questions are to be asked again at `now`: at once when a request
    /// got no answer, and when titles got only the rules' answer, once [`RULES_RECHECK`] has passed
    /// since that rebuild (a clock set back counts as passed).
    fn judge_again(&self, now: DateTime<Utc>) -> bool {
        let Some(report) = &self.judged else {
            return false;
        };
        if report.failed() {
            // at once, unless the core said when to come back: not before then (a clock set back
            // counts as passed)
            return match report.retry_after {
                None => true,
                Some(wait) => self
                    .rebuilt_at
                    .and_then(|at| (now - at).to_std().ok())
                    .is_none_or(|waited| waited >= wait.min(MAX_RETRY_AFTER)),
            };
        }
        report.by_rules > 0
            && self
                .rebuilt_at
                .and_then(|at| (now - at).to_std().ok())
                .is_none_or(|waited| waited >= RULES_RECHECK)
    }

    /// Whether the probe the last [`Self::refresh`] returned answers from the index as it was,
    /// rather than from one rebuilt for it.
    pub fn kept(&self) -> bool {
        self.kept.is_some()
    }

    /// Reloads the probe and rebuilds the index if anything it is built from changed, or if the
    /// judge's questions of the last rebuild are to be asked again (see [`LiveIndex`]). Returns the
    /// probe it rebuilt from, or `None` when nothing changed. On an error the last index stays as
    /// it was. In a code mode whose snapshot isn't there yet (after a restart the tmpfs is empty
    /// until `sync` fetches it again), an index built from a snapshot stays too, and the probe is
    /// returned once, so it answers from that index with its true `as_of` (see [`Self::kept`]).
    ///
    /// With a judge, the rows the rules built are judged before they replace the index: a title
    /// waiting for an answer that didn't come, or that only the core's rules answered, is withheld,
    /// never shown unjudged.
    pub async fn refresh(&mut self) -> Result<Option<Probe>, String> {
        self.refresh_at(DateTime::<Utc>::from(SystemTime::now())).await
    }

    /// [`Self::refresh`] at `now`, which the rebuilt rows are indexed at and the wait before titles
    /// the rules answered are asked again is counted from.
    pub async fn refresh_at(&mut self, now: DateTime<Utc>) -> Result<Option<Probe>, String> {
        let probe = self.source.load_with(self.code_reader)?;
        let fingerprint = probe.fingerprint();
        if self.fingerprint == Some(fingerprint) && !self.judge_again(now) {
            return Ok(None);
        }
        if probe.access >= AccessMode::Code
            && probe.files.is_none()
            && self.index.has_code_counts().await.unwrap_or(false)
        {
            if self.kept == Some(fingerprint) {
                return Ok(None);
            }
            self.kept = Some(fingerprint);
            return Ok(Some(probe));
        }
        self.kept = None;
        let mut rows = probe.rows(now)?;
        let judged = match &mut self.judging {
            Some(judging) => Some(judging.apply(&mut rows, &probe.manifest).await),
            None => None,
        };
        self.index.replace(&rows).await.map_err(|err| err.to_string())?;
        self.fingerprint = Some(fingerprint);
        self.judged = judged;
        self.rebuilt_at = Some(now);
        Ok(Some(probe))
    }
}
