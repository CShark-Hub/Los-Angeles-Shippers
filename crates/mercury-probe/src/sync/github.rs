//! The GitHub REST API behind [`SourceAdapter`]: `reqwest` with rustls, through `HTTPS_PROXY` when
//! it is set, with no git binary and no git library.
//!
//! The token is read once from its file and sent as a sensitive header; it is never logged, and the
//! client's errors are cut down to the endpoint's name and the status. Conditional requests with
//! ETags keep the rate limit low: a `304` costs nothing, and the reply kept for it is the trimmed
//! one, so no `patch` is held between rounds either. The replies without file names are also kept in
//! a file ([`GitHub::with_cache_file`]), so a restart asks with its ETags too.

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use chrono::{DateTime, SecondsFormat, Utc};
use regex::Regex;
use reqwest::header::{self, HeaderMap, HeaderValue};
use reqwest::{StatusCode, Url};
use serde::de::{DeserializeOwned, IgnoredAny};
use serde::{Deserialize, Serialize};

use super::source::{CommitRef, Fetched, Repository, SourceAdapter, SourceError, Tarball};
use crate::meta::{Issue, Pull, PullFile, Release, WorkflowRun};

/// The public API; a test points the client at a mock instead.
pub const API_URL: &str = "https://api.github.com";

/// Items per page, the API's maximum.
pub const PER_PAGE: usize = 100;

/// Pages per list, so one list stops at 1,000 items.
pub const MAX_LIST_PAGES: u32 = 10;

/// Pages of one pull request's files: 300 files decide its module.
pub const MAX_FILE_PAGES: u32 = 3;

/// The largest reply read, in bytes.
pub const MAX_REPLY_BYTES: usize = 16 * 1024 * 1024;

const TIMEOUT: Duration = Duration::from_secs(30);
/// The tarball's request, which streams a whole repository.
const TARBALL_TIMEOUT: Duration = Duration::from_secs(600);
const API_VERSION: &str = "2022-11-28";
const JSON: &str = "application/vnd.github+json";
/// The head's id alone: the JSON media type would bring the commit's files and their `patch`.
const SHA: &str = "application/vnd.github.sha";
/// A file's bytes as they are.
const RAW: &str = "application/vnd.github.raw+json";

/// `owner/name`, as GitHub allows them.
static REPOSITORY: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used, reason = "a literal pattern, covered by the tests below")]
    Regex::new(r"^[A-Za-z0-9](?:[A-Za-z0-9-]{0,38})/[A-Za-z0-9._-]{1,100}$").expect("literal pattern")
});

/// The git host token, read from its file. It has no `Display`, and its `Debug` says only that it
/// is there.
pub struct GitHubToken(HeaderValue);

impl GitHubToken {
    /// Reads the token from `path`. The errors name the variable that points at the file, never
    /// what is in it.
    pub fn from_file(path: &Path) -> Result<Self, String> {
        let raw = std::fs::read_to_string(path)
            .map_err(|err| format!("cannot read PROBE_GITHUB_TOKEN_FILE: {}", err.kind()))?;
        Self::new(raw.trim())
    }

    pub fn new(token: &str) -> Result<Self, String> {
        if token.is_empty() {
            return Err("PROBE_GITHUB_TOKEN_FILE is empty".to_owned());
        }
        if !token.bytes().all(|b| b.is_ascii_graphic()) {
            return Err("the token in PROBE_GITHUB_TOKEN_FILE has characters a token can't have".to_owned());
        }
        let mut value = HeaderValue::from_str(&format!("Bearer {token}"))
            .map_err(|_| "the token in PROBE_GITHUB_TOKEN_FILE can't be sent".to_owned())?;
        value.set_sensitive(true);
        Ok(Self(value))
    }
}

impl std::fmt::Debug for GitHubToken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("GitHubToken(<set>)")
    }
}

/// A reply kept for its ETag: the trimmed items, and the count the caller read off the page (how
/// many it held before trimming, or the list's total).
struct Cached {
    etag: HeaderValue,
    raw_count: usize,
    value: serde_json::Value,
    /// Whether it may be written to the cache file: not a reply that names the code's files.
    keep: bool,
}

/// The replies whose kept part names the code's files: held in memory only, never written.
const CODE_REPLIES: [&str; 2] = ["a pull request's files", "a commit"];

/// The cache file: the replies kept for their ETags, by their URL, for one repository.
#[derive(Serialize, Deserialize)]
struct CacheFile {
    repo: String,
    entries: Vec<CacheEntry>,
}

#[derive(Serialize, Deserialize)]
struct CacheEntry {
    url: String,
    etag: String,
    raw_count: usize,
    value: serde_json::Value,
}

/// One repository on GitHub.
pub struct GitHub {
    client: reqwest::Client,
    api: Url,
    repo: String,
    cache: Mutex<HashMap<String, Cached>>,
    /// Where the cache is kept between runs, once [`GitHub::with_cache_file`] named it.
    cache_file: Option<PathBuf>,
    /// Whether the cache changed since it was last written.
    cache_changed: AtomicBool,
}

impl std::fmt::Debug for GitHub {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("GitHub")
            .field("repo", &self.repo)
            .finish_non_exhaustive()
    }
}

/// What one request came to.
enum Reply {
    Body {
        etag: Option<HeaderValue>,
        bytes: Vec<u8>,
    },
    NotModified,
    Status(StatusCode),
}

impl GitHub {
    /// A client for `repo` (`owner/name`) on the API at `api`. `HTTPS_PROXY` is honoured, as
    /// `reqwest` reads it; on Compose it is the egress proxy.
    pub fn new(api: &str, repo: &str, token: &GitHubToken) -> Result<Self, String> {
        if !REPOSITORY.is_match(repo) {
            return Err("PROBE_GITHUB_REPO must look like owner/name".to_owned());
        }
        let api = Url::parse(api.trim_end_matches('/'))
            .map_err(|_| "the GitHub API address is not a URL".to_owned())?;
        let mut headers = HeaderMap::new();
        headers.insert(header::AUTHORIZATION, token.0.clone());
        headers.insert("x-github-api-version", HeaderValue::from_static(API_VERSION));
        let client = reqwest::Client::builder()
            .user_agent(concat!("mercury-probe/", env!("CARGO_PKG_VERSION")))
            .default_headers(headers)
            .timeout(TIMEOUT)
            .connect_timeout(Duration::from_secs(10))
            .build()
            .map_err(|_| "no HTTP client for the GitHub API".to_owned())?;
        Ok(Self {
            client,
            api,
            repo: repo.to_owned(),
            cache: Mutex::new(HashMap::new()),
            cache_file: None,
            cache_changed: AtomicBool::new(false),
        })
    }

    /// Keeps the cache in `path` between runs and reads what an earlier run left there. A file that
    /// can't be read, or that another repository's run wrote, is an empty cache.
    #[must_use]
    pub fn with_cache_file(mut self, path: &Path) -> Self {
        let saved = std::fs::read(path)
            .ok()
            .and_then(|bytes| serde_json::from_slice::<CacheFile>(&bytes).ok())
            .filter(|file| file.repo == self.repo);
        if let (Some(saved), Ok(cache)) = (saved, self.cache.get_mut()) {
            for entry in saved.entries {
                if let Ok(etag) = HeaderValue::from_str(&entry.etag) {
                    cache.insert(
                        entry.url,
                        Cached {
                            etag,
                            raw_count: entry.raw_count,
                            value: entry.value,
                            keep: true,
                        },
                    );
                }
            }
        }
        self.cache_file = Some(path.to_owned());
        self
    }

    /// Writes the cache file when the cache changed: every reply but those that name the code's
    /// files, to a temporary name and renamed. A failed write costs only full replies after a
    /// restart, so it is not an error; it is tried again at the next change.
    pub fn save_cache_file(&self) {
        let Some(path) = &self.cache_file else {
            return;
        };
        if !self.cache_changed.swap(false, Ordering::Relaxed) {
            return;
        }
        let entries = match self.cache.lock() {
            Ok(cache) => cache
                .iter()
                .filter(|(_, cached)| cached.keep)
                .filter_map(|(url, cached)| {
                    Some(CacheEntry {
                        url: url.clone(),
                        etag: cached.etag.to_str().ok()?.to_owned(),
                        raw_count: cached.raw_count,
                        value: cached.value.clone(),
                    })
                })
                .collect(),
            Err(_) => return,
        };
        let file = CacheFile {
            repo: self.repo.clone(),
            entries,
        };
        let Ok(bytes) = serde_json::to_vec(&file) else {
            return;
        };
        let temporary = path.with_extension("json.tmp");
        if std::fs::write(&temporary, bytes).is_err() || std::fs::rename(&temporary, path).is_err() {
            let _ = std::fs::remove_file(&temporary);
            self.cache_changed.store(true, Ordering::Relaxed);
        }
    }

    /// `{api}/repos/{repo}/{segments…}?{query}`, each segment escaped (a branch may hold `/`).
    fn url(&self, segments: &[&str], query: &[(&str, &str)]) -> Url {
        let mut url = self.api.clone();
        if let Ok(mut path) = url.path_segments_mut() {
            path.pop_if_empty().push("repos");
            path.extend(self.repo.split('/'));
            path.extend(segments);
        }
        if !query.is_empty() {
            url.query_pairs_mut().extend_pairs(query);
        }
        url
    }

    async fn send(
        &self,
        what: &'static str,
        url: &Url,
        accept: &'static str,
        etag: Option<&HeaderValue>,
    ) -> Result<Reply, SourceError> {
        let mut request = self.client.get(url.clone()).header(header::ACCEPT, accept);
        if let Some(etag) = etag {
            request = request.header(header::IF_NONE_MATCH, etag.clone());
        }
        // the error's own text names the URL, so only its kind leaves
        let mut response = request.send().await.map_err(|err| failure(what, &err))?;
        let status = response.status();
        if status == StatusCode::NOT_MODIFIED {
            return Ok(Reply::NotModified);
        }
        if rate_limited(status, response.headers()) {
            return Err(SourceError::RateLimited(what));
        }
        if !status.is_success() {
            return Ok(Reply::Status(status));
        }
        let etag = response.headers().get(header::ETAG).cloned();
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|err| failure(what, &err))? {
            if bytes.len() + chunk.len() > MAX_REPLY_BYTES {
                return Err(SourceError::TooLarge(what));
            }
            bytes.extend_from_slice(&chunk);
        }
        Ok(Reply::Body { etag, bytes })
    }

    /// One JSON page, trimmed by `items` into a count and what is kept, and served from the ETag
    /// cache on a `304`. Returns the two, or the status the host refused with.
    async fn page<P, T>(
        &self,
        what: &'static str,
        url: &Url,
        items: impl Fn(P) -> (usize, T),
    ) -> Result<Result<(usize, T), StatusCode>, SourceError>
    where
        P: DeserializeOwned,
        T: Serialize + DeserializeOwned,
    {
        let key = url.as_str().to_owned();
        let etag = self.cached(&key, |c| c.etag.clone());
        match self.send(what, url, JSON, etag.as_ref()).await? {
            Reply::Body { etag, bytes } => {
                let page: P = serde_json::from_slice(&bytes).map_err(|_| SourceError::Invalid(what))?;
                let (raw_count, kept) = items(page);
                if let Some(etag) = etag {
                    let value = serde_json::to_value(&kept).map_err(|_| SourceError::Invalid(what))?;
                    self.store(
                        key,
                        Cached {
                            etag,
                            raw_count,
                            value,
                            keep: !CODE_REPLIES.contains(&what),
                        },
                    );
                }
                Ok(Ok((raw_count, kept)))
            }
            Reply::NotModified => {
                let cached = self
                    .cached(&key, |c| (c.raw_count, c.value.clone()))
                    .ok_or(SourceError::Invalid(what))?;
                let kept = serde_json::from_value(cached.1).map_err(|_| SourceError::Invalid(what))?;
                Ok(Ok((cached.0, kept)))
            }
            Reply::Status(status) => Ok(Err(status)),
        }
    }

    /// Every page of a list, up to `max_pages`: a page shorter than [`PER_PAGE`] is the last.
    async fn list<P, T>(
        &self,
        what: &'static str,
        segments: &[&str],
        query: &[(&str, &str)],
        max_pages: u32,
        items: impl Fn(P) -> (usize, Vec<T>),
    ) -> Result<Vec<T>, SourceError>
    where
        P: DeserializeOwned,
        T: Serialize + DeserializeOwned,
    {
        let per_page = PER_PAGE.to_string();
        let mut all = Vec::new();
        for page in 1..=max_pages {
            let number = page.to_string();
            let mut pairs = query.to_vec();
            pairs.extend([("per_page", per_page.as_str()), ("page", number.as_str())]);
            let url = self.url(segments, &pairs);
            let (raw_count, kept) = self
                .page(what, &url, &items)
                .await?
                .map_err(|status| refused(what, status))?;
            all.extend(kept);
            if raw_count < PER_PAGE {
                break;
            }
        }
        Ok(all)
    }

    fn cached<R>(&self, key: &str, read: impl Fn(&Cached) -> R) -> Option<R> {
        self.cache.lock().ok()?.get(key).map(read)
    }

    fn store(&self, key: String, entry: Cached) {
        if let Ok(mut cache) = self.cache.lock() {
            cache.insert(key, entry);
            self.cache_changed.store(true, Ordering::Relaxed);
        }
    }
}

/// A request that failed before the host answered: its kind, never its text.
fn failure(what: &'static str, err: &reqwest::Error) -> SourceError {
    if err.is_timeout() {
        SourceError::Timeout(what)
    } else if err.is_decode() || err.is_body() {
        SourceError::Invalid(what)
    } else {
        SourceError::Unreachable(what)
    }
}

/// A spent rate limit: `429`, or a `403` that says so, which is not a missing permission.
fn rate_limited(status: StatusCode, headers: &HeaderMap) -> bool {
    status == StatusCode::TOO_MANY_REQUESTS
        || (status == StatusCode::FORBIDDEN
            && (headers.get("x-ratelimit-remaining").is_some_and(|v| v == "0")
                || headers.contains_key(header::RETRY_AFTER)))
}

fn refused(what: &'static str, status: StatusCode) -> SourceError {
    match status {
        StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => SourceError::Forbidden(what),
        StatusCode::NOT_FOUND => SourceError::NotFound(what),
        status => SourceError::Status {
            what,
            status: status.as_u16(),
        },
    }
}

fn timestamp(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Secs, true)
}

/// A page as it is: its items and their number.
/// A page of pull requests, each body cut down to its links before anything keeps it, the cache
/// included.
fn links_only(page: Vec<Pull>) -> (usize, Vec<Pull>) {
    let raw = page.len();
    let pulls = page
        .into_iter()
        .map(|mut pull| {
            pull.keep_links_only();
            pull
        })
        .collect();
    (raw, pulls)
}

fn whole<T>(items: Vec<T>) -> (usize, Vec<T>) {
    (items.len(), items)
}

/// An entry of `/issues`, which lists pull requests too.
#[derive(Deserialize)]
struct IssueEntry {
    #[serde(flatten)]
    issue: Issue,
    pull_request: Option<IgnoredAny>,
}

#[derive(Deserialize)]
struct RunsPage {
    #[serde(default)]
    total_count: usize,
    #[serde(default)]
    workflow_runs: Vec<WorkflowRun>,
}

#[derive(Deserialize)]
struct PullDetail {
    commits: Option<u32>,
}

/// A commit in a list or a compare: its id and its committer's date, nothing else kept.
#[derive(Deserialize)]
struct CommitEntry {
    sha: String,
    commit: CommitBody,
}

#[derive(Deserialize)]
struct CommitBody {
    committer: Option<Committer>,
}

#[derive(Deserialize)]
struct Committer {
    date: DateTime<Utc>,
}

impl CommitEntry {
    fn into_ref(self) -> Option<CommitRef> {
        Some(CommitRef {
            sha: self.sha.to_ascii_lowercase(),
            committed_at: self.commit.committer?.date,
        })
    }
}

/// A commit's detail: the file names only; `patch` and the rest are never parsed.
#[derive(Deserialize)]
struct CommitDetail {
    #[serde(default)]
    files: Vec<PullFile>,
}

#[derive(Deserialize)]
struct Comparison {
    status: String,
    #[serde(default)]
    total_commits: usize,
    #[serde(default)]
    commits: Vec<CommitEntry>,
}

impl SourceAdapter for GitHub {
    async fn repository(&self) -> Result<Repository, SourceError> {
        const WHAT: &str = "the repository";
        self.page(WHAT, &self.url(&[], &[]), |repository: Repository| {
            (1, repository)
        })
        .await?
        .map(|(_, repository)| repository)
        .map_err(|status| refused(WHAT, status))
    }

    async fn head(&self, branch: &str) -> Result<Option<String>, SourceError> {
        const WHAT: &str = "the head";
        let url = self.url(&["commits", branch], &[]);
        let key = url.as_str().to_owned();
        let etag = self.cached(&key, |c| c.etag.clone());
        let sha = match self.send(WHAT, &url, SHA, etag.as_ref()).await? {
            Reply::Body { etag, bytes } => {
                let sha = std::str::from_utf8(&bytes)
                    .ok()
                    .map(str::trim)
                    .filter(|s| s.len() == 40 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                    .ok_or(SourceError::Invalid(WHAT))?
                    .to_ascii_lowercase();
                if let Some(etag) = etag {
                    self.store(
                        key,
                        Cached {
                            etag,
                            raw_count: 1,
                            value: serde_json::Value::String(sha.clone()),
                            keep: true,
                        },
                    );
                }
                sha
            }
            Reply::NotModified => self
                .cached(&key, |c| c.value.as_str().map(str::to_owned))
                .flatten()
                .ok_or(SourceError::Invalid(WHAT))?,
            // a token without Contents: GitHub refuses, or hides the code behind a 404
            Reply::Status(StatusCode::FORBIDDEN | StatusCode::NOT_FOUND) => return Ok(None),
            Reply::Status(status) => return Err(refused(WHAT, status)),
        };
        Ok(Some(sha))
    }

    async fn manifest(&self, commit: &str) -> Result<Fetched<Vec<u8>>, SourceError> {
        const WHAT: &str = "the manifest";
        let url = self.url(&["contents", ".mercury", "probe.yaml"], &[("ref", commit)]);
        // at one commit the file can't change, so there is no ETag to keep
        Ok(match self.send(WHAT, &url, RAW, None).await? {
            Reply::Body { bytes, .. } => Fetched::Read(bytes),
            Reply::Status(StatusCode::NOT_FOUND) => Fetched::Missing,
            Reply::Status(StatusCode::FORBIDDEN) => Fetched::Refused,
            Reply::Status(status) => return Err(refused(WHAT, status)),
            Reply::NotModified => return Err(SourceError::Invalid(WHAT)),
        })
    }

    async fn pulls(&self, since: DateTime<Utc>) -> Result<Vec<Pull>, SourceError> {
        const WHAT: &str = "pull requests";
        let open = [("state", "open"), ("sort", "updated"), ("direction", "desc")];
        let mut pulls = self
            .list(WHAT, &["pulls"], &open, MAX_LIST_PAGES, links_only)
            .await?;
        // the closed ones newest first, until the window ends
        let per_page = PER_PAGE.to_string();
        for page in 1..=MAX_LIST_PAGES {
            let number = page.to_string();
            let closed = [
                ("state", "closed"),
                ("sort", "updated"),
                ("direction", "desc"),
                ("per_page", per_page.as_str()),
                ("page", number.as_str()),
            ];
            let (raw_count, page) = self
                .page(WHAT, &self.url(&["pulls"], &closed), links_only)
                .await?
                .map_err(|status| refused(WHAT, status))?;
            let older = page.iter().any(|p| p.updated_at < since);
            pulls.extend(page.into_iter().filter(|p| p.updated_at >= since));
            if raw_count < PER_PAGE || older {
                break;
            }
        }
        pulls.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(b.number.cmp(&a.number)));
        Ok(pulls)
    }

    async fn issues(&self, since: DateTime<Utc>) -> Result<Vec<Issue>, SourceError> {
        const WHAT: &str = "issues";
        let only_issues = |page: Vec<IssueEntry>| {
            let raw = page.len();
            let issues = page
                .into_iter()
                .filter(|e| e.pull_request.is_none())
                .map(|e| e.issue)
                .collect();
            (raw, issues)
        };
        let since = timestamp(since);
        let open = [("state", "open"), ("sort", "updated"), ("direction", "desc")];
        let closed = [
            ("state", "closed"),
            ("since", since.as_str()),
            ("sort", "updated"),
            ("direction", "desc"),
        ];
        let mut issues = self
            .list(WHAT, &["issues"], &open, MAX_LIST_PAGES, only_issues)
            .await?;
        issues.extend(
            self.list(WHAT, &["issues"], &closed, MAX_LIST_PAGES, only_issues)
                .await?,
        );
        issues.sort_by(|a, b| b.updated_at.cmp(&a.updated_at).then(b.number.cmp(&a.number)));
        Ok(issues)
    }

    async fn workflow_runs(
        &self,
        branch: &str,
        since: DateTime<Utc>,
    ) -> Result<Vec<WorkflowRun>, SourceError> {
        const WHAT: &str = "workflow runs";
        let created = format!(">={}", since.format("%Y-%m-%d"));
        let per_page = PER_PAGE.to_string();
        let mut runs: Vec<WorkflowRun> = Vec::new();
        for page in 1..=MAX_LIST_PAGES {
            let number = page.to_string();
            let query = [
                ("branch", branch),
                ("created", created.as_str()),
                ("per_page", per_page.as_str()),
                ("page", number.as_str()),
            ];
            let (total, page) = self
                .page(WHAT, &self.url(&["actions", "runs"], &query), |page: RunsPage| {
                    (page.total_count, page.workflow_runs)
                })
                .await?
                .map_err(|status| refused(WHAT, status))?;
            let empty = page.is_empty();
            runs.extend(page);
            // a page of runs can come back short before the last one, so the total says when to stop
            if empty || runs.len() >= total {
                break;
            }
        }
        // a run that arrives between two pages shifts the next one: the same run twice
        runs.sort_by(|a, b| b.created_at.cmp(&a.created_at).then(b.id.cmp(&a.id)));
        runs.dedup_by_key(|run| run.id);
        Ok(runs)
    }

    async fn releases(&self) -> Result<Vec<Release>, SourceError> {
        match self.list("releases", &["releases"], &[], 1, whole).await {
            // releases need Contents: a metadata token reads none
            Err(SourceError::Forbidden(_) | SourceError::NotFound(_)) => Ok(Vec::new()),
            other => other,
        }
    }

    async fn pull_files(&self, number: u32) -> Result<Vec<PullFile>, SourceError> {
        let number = number.to_string();
        self.list(
            "a pull request's files",
            &["pulls", &number, "files"],
            &[],
            MAX_FILE_PAGES,
            whole,
        )
        .await
    }

    async fn pull_commits(&self, number: u32) -> Result<Option<u32>, SourceError> {
        const WHAT: &str = "a pull request";
        let number = number.to_string();
        self.page(WHAT, &self.url(&["pulls", &number], &[]), |detail: PullDetail| {
            (1, detail.commits)
        })
        .await?
        .map(|(_, commits)| commits)
        .map_err(|status| refused(WHAT, status))
    }

    async fn tarball(&self, commit: &str) -> Result<Fetched<Tarball>, SourceError> {
        const WHAT: &str = "the snapshot";
        // the API redirects to codeload.github.com, and reqwest leaves the token off another host
        let request = self
            .client
            .get(self.url(&["tarball", commit], &[]))
            .header(header::ACCEPT, JSON)
            .timeout(TARBALL_TIMEOUT);
        let response = request.send().await.map_err(|err| failure(WHAT, &err))?;
        let status = response.status();
        if rate_limited(status, response.headers()) {
            return Err(SourceError::RateLimited(WHAT));
        }
        match status {
            StatusCode::FORBIDDEN | StatusCode::UNAUTHORIZED => return Ok(Fetched::Refused),
            StatusCode::NOT_FOUND => return Ok(Fetched::Missing),
            status if !status.is_success() => return Err(refused(WHAT, status)),
            _ => {}
        }
        let chunks = futures_util::stream::unfold(Some(response), |response| async move {
            let mut response = response?;
            match response.chunk().await {
                Ok(Some(chunk)) => Some((Ok(chunk.to_vec()), Some(response))),
                Ok(None) => None,
                Err(err) => Some((Err(failure(WHAT, &err)), None)),
            }
        });
        Ok(Fetched::Read(Box::pin(chunks)))
    }

    async fn commits(&self, branch: &str, since: DateTime<Utc>) -> Result<Vec<CommitRef>, SourceError> {
        let since = timestamp(since);
        let query = [("sha", branch), ("since", since.as_str())];
        self.list(
            "commits",
            &["commits"],
            &query,
            MAX_LIST_PAGES,
            |page: Vec<CommitEntry>| {
                let raw = page.len();
                (raw, page.into_iter().filter_map(CommitEntry::into_ref).collect())
            },
        )
        .await
    }

    async fn commit_files(&self, sha: &str) -> Result<Vec<String>, SourceError> {
        const WHAT: &str = "a commit";
        self.page(WHAT, &self.url(&["commits", sha], &[]), |detail: CommitDetail| {
            let files: Vec<String> = detail.files.into_iter().map(|f| f.filename).collect();
            (1, files)
        })
        .await?
        .map(|(_, files)| files)
        .map_err(|status| refused(WHAT, status))
    }

    fn save_cache(&self) {
        self.save_cache_file();
    }

    async fn compare(&self, base: &str, head: &str) -> Result<Option<Vec<CommitRef>>, SourceError> {
        const WHAT: &str = "a compare";
        let range = format!("{base}...{head}");
        let url = self.url(&["compare", &range], &[]);
        // a compare can't change, so there is no ETag to keep; its files' patches can be big
        let comparison = match self.send(WHAT, &url, JSON, None).await {
            Ok(Reply::Body { bytes, .. }) => {
                serde_json::from_slice::<Comparison>(&bytes).map_err(|_| SourceError::Invalid(WHAT))?
            }
            Ok(Reply::Status(StatusCode::NOT_FOUND)) | Err(SourceError::TooLarge(_)) => return Ok(None),
            Ok(Reply::Status(status)) => return Err(refused(WHAT, status)),
            Ok(Reply::NotModified) => return Err(SourceError::Invalid(WHAT)),
            Err(err) => return Err(err),
        };
        let straight = matches!(comparison.status.as_str(), "ahead" | "identical");
        if !straight || comparison.total_commits > comparison.commits.len() {
            return Ok(None);
        }
        // the API lists them oldest first
        let mut commits: Vec<CommitRef> = comparison
            .commits
            .into_iter()
            .filter_map(CommitEntry::into_ref)
            .collect();
        commits.reverse();
        Ok(Some(commits))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_repository_is_owner_and_name() {
        for good in ["your-org/delta", "a/b", "example-org/Delta.Api_2"] {
            assert!(REPOSITORY.is_match(good), "{good}");
        }
        for bad in [
            "delta",
            "org/",
            "/delta",
            "org/delta/extra",
            "org/de lta",
            "-org/delta",
            "../x",
        ] {
            assert!(!REPOSITORY.is_match(bad), "{bad}");
        }
    }

    #[test]
    fn the_token_is_never_shown() {
        let token = GitHubToken::new("github_pat_example_0123").unwrap();
        assert_eq!(format!("{token:?}"), "GitHubToken(<set>)");
        assert!(token.0.is_sensitive());
        assert!(GitHubToken::new("").is_err());
        let err = GitHubToken::new("two words").unwrap_err();
        assert!(!err.contains("two"), "{err}");
    }

    #[test]
    fn a_branch_with_a_slash_stays_one_segment() {
        let token = GitHubToken::new("t").unwrap();
        let github = GitHub::new("https://api.example.com/", "your-org/delta", &token).unwrap();
        let url = github.url(&["commits", "release/1.0"], &[("ref", "a b")]);
        assert_eq!(
            url.as_str(),
            "https://api.example.com/repos/your-org/delta/commits/release%2F1.0?ref=a+b"
        );
    }

    #[test]
    fn a_spent_rate_limit_is_not_a_missing_permission() {
        let mut headers = HeaderMap::new();
        assert!(!rate_limited(StatusCode::FORBIDDEN, &headers));
        assert!(rate_limited(StatusCode::TOO_MANY_REQUESTS, &headers));
        headers.insert("x-ratelimit-remaining", HeaderValue::from_static("0"));
        assert!(rate_limited(StatusCode::FORBIDDEN, &headers));
        assert!(!rate_limited(StatusCode::NOT_FOUND, &headers));
    }
}
