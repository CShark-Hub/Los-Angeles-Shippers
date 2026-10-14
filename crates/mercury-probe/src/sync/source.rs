//! The seam between `sync` and a git host: everything host-specific sits behind [`SourceAdapter`]
//! (GitHub now; GitLab, Azure DevOps or Jira later), the snapshot's tarball and the commit history
//! included.
//!
//! The replies are the [`crate::meta`] types: what is parsed from a host is already trimmed to what
//! the probe reads, so `patch`, bodies' text and every field the probe ignores are gone before
//! `sync` holds them.

use std::future::Future;

use chrono::{DateTime, Utc};
use futures_util::stream::BoxStream;
use serde::{Deserialize, Serialize};

use crate::meta::{Issue, Pull, PullFile, Release, WorkflowRun};

/// The repository as the host names it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Repository {
    pub full_name: String,
    pub default_branch: String,
}

/// A commit of the default branch, as the history needs it: no message, no author.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommitRef {
    pub sha: String,
    pub committed_at: DateTime<Utc>,
}

/// A tarball as it arrives: its chunks in order, an error ending it early.
pub type Tarball = BoxStream<'static, Result<Vec<u8>, SourceError>>;

/// A file the token may not be allowed to read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fetched<T> {
    Read(T),
    /// No such file at that commit.
    Missing,
    /// The token isn't allowed to read it.
    Refused,
}

/// What a git host gives `sync`. The lists are the work of the last `since` (and every open item),
/// newest first; the errors name the endpoint and the status, never a URL, a token or a reply.
pub trait SourceAdapter: Sync {
    /// The repository and its default branch: every token may read them.
    fn repository(&self) -> impl Future<Output = Result<Repository, SourceError>> + Send;

    /// The commit at the head of `branch`, or `None` when the token can't read the code: that
    /// answer is how `sync` learns the token's permission, so it never fetches anything with code
    /// or `patch` in it.
    fn head(&self, branch: &str) -> impl Future<Output = Result<Option<String>, SourceError>> + Send;

    /// The manifest file's bytes at a commit.
    fn manifest(&self, commit: &str) -> impl Future<Output = Result<Fetched<Vec<u8>>, SourceError>> + Send;

    /// Every open pull request, and the closed ones updated since `since`.
    fn pulls(&self, since: DateTime<Utc>) -> impl Future<Output = Result<Vec<Pull>, SourceError>> + Send;

    /// Every open issue, and the closed ones updated since `since`; issues only, no pull requests.
    fn issues(&self, since: DateTime<Utc>) -> impl Future<Output = Result<Vec<Issue>, SourceError>> + Send;

    /// The runs on `branch` created since `since`.
    fn workflow_runs(
        &self,
        branch: &str,
        since: DateTime<Utc>,
    ) -> impl Future<Output = Result<Vec<WorkflowRun>, SourceError>> + Send;

    /// The latest releases; none when the token can't read them.
    fn releases(&self) -> impl Future<Output = Result<Vec<Release>, SourceError>> + Send;

    /// A pull request's file names, `patch` dropped while parsing. Code modes only.
    fn pull_files(&self, number: u32) -> impl Future<Output = Result<Vec<PullFile>, SourceError>> + Send;

    /// A pull request's commit count, from its detail.
    fn pull_commits(&self, number: u32) -> impl Future<Output = Result<Option<u32>, SourceError>> + Send;

    /// A commit's gzipped tarball, as a stream that goes straight into the unpacker. Code modes
    /// only; `Refused` when the token can't read the code.
    fn tarball(&self, commit: &str) -> impl Future<Output = Result<Fetched<Tarball>, SourceError>> + Send;

    /// The commits on `branch` since `since`, newest first. Code modes only.
    fn commits(
        &self,
        branch: &str,
        since: DateTime<Utc>,
    ) -> impl Future<Output = Result<Vec<CommitRef>, SourceError>> + Send;

    /// A commit's file names, `patch` dropped while parsing. Code modes only.
    fn commit_files(&self, sha: &str) -> impl Future<Output = Result<Vec<String>, SourceError>> + Send;

    /// The commits after `base` up to `head`, newest first, or `None` when the host can't
    /// answer that as a straight line (`base` gone after a force-push, or too many commits between
    /// them), and the history starts again. Code modes only.
    fn compare(
        &self,
        base: &str,
        head: &str,
    ) -> impl Future<Output = Result<Option<Vec<CommitRef>>, SourceError>> + Send;

    /// Keeps what lets the next start ask conditionally, when the host has such a cache; called
    /// after each round. A failure costs only full replies after a restart.
    fn save_cache(&self) {}
}

/// Why a request to the host failed. Every message is fixed text and the endpoint's name.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SourceError {
    #[error("{0}: the host can't be reached")]
    Unreachable(&'static str),
    #[error("{0}: the request timed out")]
    Timeout(&'static str),
    #[error("{0}: the rate limit is spent; the next round tries again")]
    RateLimited(&'static str),
    #[error("{0}: the token isn't allowed to read it")]
    Forbidden(&'static str),
    #[error("{0}: not found")]
    NotFound(&'static str),
    #[error("{what}: the host answered {status}")]
    Status { what: &'static str, status: u16 },
    #[error("{0}: the reply doesn't parse")]
    Invalid(&'static str),
    #[error("{0}: the reply is larger than the limit")]
    TooLarge(&'static str),
}
