//! The probe's questions to the core's judge: whether a title the rules let through is safe to
//! show, which status fits a status conflict, and an item's kind when its labels don't say.
//!
//! The probe never calls a model in the cloud: it asks the core (`POST /internal/judge`), which
//! redacts, budgets and audits. Only the texts the manifest's `judge_context` names are sent, each
//! already redacted with the manifest's rules. An answer is taken only above its threshold; no
//! answer is never an answer, so a conflict stays `unclear`, a kind `unknown`, and a title that
//! waited for the judge is withheld. When no judge answers, the core answers with its rules, which
//! see only patterns: for a title that is no answer either, so it is withheld and asked again, and
//! for a status or a kind it is none whatever its number. A core that answers `Retry-After` is not
//! asked again before then.

use std::collections::{HashMap, HashSet};
use std::time::Duration;

use mercury_schema::JudgeMode;
use mercury_schema::facts::{Kind, Status};
use mercury_schema::ids::JudgeItemId;
use mercury_schema::manifest::{JudgeContext, ProbeManifest};
use mercury_schema::probe_judge::{
    JudgeReply, JudgeRequest, JudgeText, KindItem, MAX_ITEMS, Purpose, StatusItem, TitleItem,
};
use mercury_schema::redact::withheld_title;
use serde::Serialize;
use sha2::{Digest, Sha256};

use crate::facts::{Item, Rows};

/// A choice below this is no answer: the status stays `unclear`, the kind `unknown`.
pub const CHOICE_AT: f64 = 0.6;
/// A title at or above this is withheld.
pub const WITHHOLD_AT: f64 = 0.5;
/// How much stricter each threshold is when the judge that answered isn't Jev.
pub const TIGHTER: f64 = 0.2;
/// The most a request's items weigh, as JSON, well under the core's 1 MB.
pub const MAX_REQUEST_BYTES: usize = 256 * 1024;
/// How long a request may take: the core's chain, a fallback and the rules included.
pub const TIMEOUT: Duration = Duration::from_secs(30);
/// The longest the probe waits on a `Retry-After`, whatever the core names: a word from the core is
/// no reason to leave the titles withheld for hours.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(10 * 60);

/// The choice threshold for an answer of `mode`. In mode `rules` no number reaches it: the rules see
/// only patterns and answer a status or a kind with "can't tell", so whatever p one carries, it
/// settles nothing.
pub fn choice_at(mode: JudgeMode) -> f64 {
    match mode {
        JudgeMode::Jev => CHOICE_AT,
        JudgeMode::Chat => CHOICE_AT + TIGHTER,
        JudgeMode::Rules => f64::INFINITY,
    }
}

/// The title threshold for an answer of `mode`. In mode `rules` it is 0, so every title is
/// withheld whatever its p: the rules saw only patterns, and while no judge answers a title is
/// never shown on less than normal operation would check.
pub fn withhold_at(mode: JudgeMode) -> f64 {
    match mode {
        JudgeMode::Jev => WITHHOLD_AT,
        JudgeMode::Chat => WITHHOLD_AT - TIGHTER,
        JudgeMode::Rules => 0.0,
    }
}

/// Why a request got no answer. It names a status, never a body.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AskError {
    /// An error status; `retry_after` is the seconds a `Retry-After` named, if it named any.
    #[error("the core answered {code}")]
    Status {
        code: u16,
        retry_after: Option<Duration>,
    },
    #[error("the core gave no answer in time")]
    Timeout,
    #[error("the core can't be reached")]
    Unreachable,
    #[error("the core's reply doesn't answer the request")]
    NotTheReply,
}

/// The core's judge endpoint, with the probe token.
#[derive(Clone)]
pub struct JudgeClient {
    url: String,
    token: Option<String>,
    http: reqwest::Client,
}

impl std::fmt::Debug for JudgeClient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let token = if self.token.is_some() { "<set>" } else { "<none>" };
        f.debug_struct("JudgeClient")
            .field("url", &self.url)
            .field("token", &format_args!("{token}"))
            .finish_non_exhaustive()
    }
}

impl JudgeClient {
    /// `core` is the core's internal address (`PROBE_CORE_INTERNAL_URL`, e.g. `http://core:8081`).
    pub fn new(core: &str, token: Option<String>) -> Result<Self, String> {
        let http = reqwest::Client::builder()
            .timeout(TIMEOUT)
            .build()
            .map_err(|_| "the judge client can't be built".to_owned())?;
        Ok(Self {
            url: format!("{}/internal/judge", core.trim_end_matches('/')),
            token,
            http,
        })
    }

    /// One request, and the core's reply to it: the same purpose, one answer per item in the
    /// request's order, a status only among the item's candidates. Anything else is no answer.
    pub async fn ask(&self, request: &JudgeRequest) -> Result<JudgeReply, AskError> {
        let mut post = self.http.post(&self.url).json(request);
        if let Some(token) = &self.token {
            post = post.bearer_auth(token);
        }
        let response = post.send().await.map_err(|err| {
            if err.is_timeout() {
                AskError::Timeout
            } else {
                AskError::Unreachable
            }
        })?;
        if !response.status().is_success() {
            return Err(AskError::Status {
                code: response.status().as_u16(),
                retry_after: retry_after(response.headers()),
            });
        }
        let bytes = response.bytes().await.map_err(|err| {
            if err.is_timeout() {
                AskError::Timeout
            } else {
                AskError::Unreachable
            }
        })?;
        let reply: JudgeReply = serde_json::from_slice(&bytes).map_err(|_| AskError::NotTheReply)?;
        if answers(request, &reply) {
            Ok(reply)
        } else {
            Err(AskError::NotTheReply)
        }
    }
}

/// The wait a `Retry-After` names, when it names it in seconds. A date, or anything else, names none.
fn retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Whether `reply` answers `request`: its purpose, its items' ids in order, and each status one of
/// its item's candidates.
fn answers(request: &JudgeRequest, reply: &JudgeReply) -> bool {
    match (request, reply) {
        (
            JudgeRequest::StatusFromCandidates { items: asked },
            JudgeReply::StatusFromCandidates { items: answered, .. },
        ) => {
            asked.len() == answered.len()
                && asked
                    .iter()
                    .zip(answered)
                    .all(|(a, b)| a.id == b.id && a.candidates.contains(&b.status))
        }
        (JudgeRequest::Kind { items: asked }, JudgeReply::Kind { items: answered, .. }) => {
            asked.len() == answered.len()
                && asked
                    .iter()
                    .zip(answered)
                    .all(|(a, b)| a.id == b.id && b.kind != Kind::Unknown)
        }
        (JudgeRequest::TitleSafety { items: asked }, JudgeReply::TitleSafety { items: answered, .. }) => {
            asked.len() == answered.len() && asked.iter().zip(answered).all(|(a, b)| a.id == b.id)
        }
        _ => false,
    }
}

/// One item of a request, of any purpose.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(untagged)]
enum Asked {
    Status(StatusItem),
    Kind(KindItem),
    Title(TitleItem),
}

impl Asked {
    fn id(&self) -> &JudgeItemId {
        match self {
            Asked::Status(item) => &item.id,
            Asked::Kind(item) => &item.id,
            Asked::Title(item) => &item.id,
        }
    }

    /// A digest of everything sent about the item, which the cache keys its answer by.
    fn digest(&self) -> [u8; 32] {
        let bytes = serde_json::to_vec(self).unwrap_or_default();
        Sha256::digest(bytes).into()
    }
}

/// One item's answer, with the mode of the judge that gave it.
#[derive(Debug, Clone, Copy, PartialEq)]
enum Got {
    Status(Status, f64),
    Kind(Kind, f64),
    Title(f64),
}

/// A request of one purpose from its items.
fn request(purpose: Purpose, items: &[Asked]) -> Option<JudgeRequest> {
    Some(match purpose {
        Purpose::StatusFromCandidates => JudgeRequest::StatusFromCandidates {
            items: items
                .iter()
                .map(|item| match item {
                    Asked::Status(item) => Some(item.clone()),
                    _ => None,
                })
                .collect::<Option<_>>()?,
        },
        Purpose::Kind => JudgeRequest::Kind {
            items: items
                .iter()
                .map(|item| match item {
                    Asked::Kind(item) => Some(item.clone()),
                    _ => None,
                })
                .collect::<Option<_>>()?,
        },
        Purpose::TitleSafety => JudgeRequest::TitleSafety {
            items: items
                .iter()
                .map(|item| match item {
                    Asked::Title(item) => Some(item.clone()),
                    _ => None,
                })
                .collect::<Option<_>>()?,
        },
    })
}

/// The answers of a reply, in its order, and the mode that gave them.
fn got(reply: &JudgeReply) -> (JudgeMode, Vec<Got>) {
    let mode = reply.judge_mode();
    let answers = match reply {
        JudgeReply::StatusFromCandidates { items, .. } => {
            items.iter().map(|a| Got::Status(a.status, a.p.get())).collect()
        }
        JudgeReply::Kind { items, .. } => items.iter().map(|a| Got::Kind(a.kind, a.p.get())).collect(),
        JudgeReply::TitleSafety { items, .. } => items.iter().map(|a| Got::Title(a.p.get())).collect(),
    };
    (mode, answers)
}

/// What one round of questions came to: counts only, for the log.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Report {
    /// Requests sent.
    pub requests: usize,
    /// Items the judge answered in this round.
    pub answered: usize,
    /// Items answered from the cache.
    pub cached: usize,
    /// Items whose request got no answer.
    pub unanswered: usize,
    /// Titles the core answered with its rules alone (mode `rules`), counted in `answered` too:
    /// no answer for a title, so they are withheld and asked again, though not at every tick.
    pub by_rules: usize,
    /// Why the last request that failed got no answer.
    pub error: Option<AskError>,
    /// The longest wait a failed request was told to make before it is asked again.
    pub retry_after: Option<Duration>,
}

impl Report {
    /// Whether every question of the round got its answer, and every title a judge's.
    pub fn complete(&self) -> bool {
        self.unanswered == 0 && self.by_rules == 0
    }

    /// Whether a request of the round got no answer at all: an error status, a timeout, no core,
    /// or a reply that isn't the request's. Titles answered by the rules alone are a gap, not this.
    pub fn failed(&self) -> bool {
        self.unanswered > 0
    }
}

/// An answer kept for an item, by a digest of what was sent about it.
#[derive(Debug, Clone)]
struct Kept {
    digest: [u8; 32],
    got: Got,
}

/// The judge's side of a rebuild: the client and the answers kept from earlier rounds.
#[derive(Debug)]
pub struct Judging {
    client: JudgeClient,
    cache: HashMap<(Purpose, String), Kept>,
}

impl Judging {
    pub fn new(client: JudgeClient) -> Self {
        Self {
            client,
            cache: HashMap::new(),
        }
    }

    /// Asks about `rows`' items what their rules left open, and applies what the judge answered:
    /// the titles first, so a title the judge withholds isn't sent again, then the conflicts, then
    /// the kinds.
    pub async fn apply(&mut self, rows: &mut Rows, manifest: &ProbeManifest) -> Report {
        let titles = manifest.judge_context.contains(&JudgeContext::Titles);
        let labels = manifest.judge_context.contains(&JudgeContext::Labels);
        let mut report = Report::default();
        let mut used = HashSet::new();

        if titles {
            let asked: Vec<(usize, Option<Asked>)> = rows
                .items
                .iter()
                .enumerate()
                .filter(|(_, item)| !item.title_withheld)
                .map(|(n, item)| (n, title_item(item)))
                .collect();
            let answers = self
                .ask_all(Purpose::TitleSafety, &asked, &mut report, &mut used)
                .await;
            for ((n, _), answer) in asked.iter().zip(answers) {
                let item = &mut rows.items[*n];
                // the rules' answer withholds whatever its p, and the title is asked again
                if matches!(answer, Some((JudgeMode::Rules, _))) {
                    report.by_rules += 1;
                }
                let safe = matches!(answer, Some((mode, Got::Title(p))) if p < withhold_at(mode));
                if !safe {
                    item.title = withheld_title(&item.module);
                    item.title_withheld = true;
                }
            }
        }

        let shown = |item: &Item| -> Option<JudgeText> {
            (titles && !item.title_withheld)
                .then(|| JudgeText::new(item.title.as_str()).ok())
                .flatten()
        };
        let sent_labels = |item: &Item| {
            if labels {
                item.ask.labels.clone()
            } else {
                Vec::new()
            }
        };

        let asked: Vec<(usize, Option<Asked>)> = rows
            .items
            .iter()
            .enumerate()
            .filter_map(|(n, item)| {
                let conflict = item.ask.conflict.as_ref()?;
                let id = JudgeItemId::parse(item.id.as_str()).ok();
                Some((
                    n,
                    id.map(|id| {
                        Asked::Status(StatusItem {
                            id,
                            candidates: conflict.candidates.clone(),
                            title: shown(item),
                            labels: sent_labels(item),
                            signals: conflict.signals.clone(),
                        })
                    }),
                ))
            })
            .collect();
        let answers = self
            .ask_all(Purpose::StatusFromCandidates, &asked, &mut report, &mut used)
            .await;
        for ((n, _), answer) in asked.iter().zip(answers) {
            let item = &mut rows.items[*n];
            if let Some((mode, Got::Status(status, p))) = answer
                && p >= choice_at(mode)
            {
                item.status = status;
                item.confidence = p;
                item.waiting_on = (status == Status::Blocked)
                    .then(|| item.ask.conflict.as_ref().map(|c| c.waiting_on))
                    .flatten();
            }
        }

        let asked: Vec<(usize, Option<Asked>)> = rows
            .items
            .iter()
            .enumerate()
            .filter(|(_, item)| item.ask.kind)
            .filter_map(|(n, item)| {
                let (title, labels) = (shown(item), sent_labels(item));
                // nothing the judge may read: the kind stays unknown, unasked
                if title.is_none() && labels.is_empty() {
                    return None;
                }
                let id = JudgeItemId::parse(item.id.as_str()).ok();
                Some((n, id.map(|id| Asked::Kind(KindItem { id, title, labels }))))
            })
            .collect();
        let answers = self.ask_all(Purpose::Kind, &asked, &mut report, &mut used).await;
        for ((n, _), answer) in asked.iter().zip(answers) {
            if let Some((mode, Got::Kind(kind, p))) = answer
                && p >= choice_at(mode)
            {
                rows.items[*n].kind = kind;
            }
        }

        // only the current items' answers are kept
        self.cache.retain(|key, _| used.contains(key));
        report
    }

    /// The answer for each item, from the cache or from the core, in the items' order; `None` for
    /// an item that got none (or that couldn't be asked: it counts as unanswered).
    async fn ask_all(
        &mut self,
        purpose: Purpose,
        asked: &[(usize, Option<Asked>)],
        report: &mut Report,
        used: &mut HashSet<(Purpose, String)>,
    ) -> Vec<Option<(JudgeMode, Got)>> {
        let mut answers = vec![None; asked.len()];
        let mut pending: Vec<(usize, Asked)> = Vec::new();
        for (at, (_, item)) in asked.iter().enumerate() {
            let Some(item) = item else {
                report.unanswered += 1;
                continue;
            };
            let key = (purpose, item.id().as_str().to_owned());
            used.insert(key.clone());
            match self.cache.get(&key) {
                Some(kept) if kept.digest == item.digest() => {
                    answers[at] = Some((JudgeMode::Jev, kept.got));
                    report.cached += 1;
                }
                _ => pending.push((at, item.clone())),
            }
        }
        for chunk in chunks(&pending) {
            let items: Vec<Asked> = chunk.iter().map(|(_, item)| item.clone()).collect();
            let Some(request) = request(purpose, &items) else {
                report.unanswered += chunk.len();
                continue;
            };
            report.requests += 1;
            match self.client.ask(&request).await {
                Ok(reply) => {
                    let (mode, got) = got(&reply);
                    for ((at, item), got) in chunk.iter().zip(got) {
                        answers[*at] = Some((mode, got));
                        report.answered += 1;
                        // a fallback's answer is used once and asked again at the next rebuild
                        if mode == JudgeMode::Jev {
                            self.cache.insert(
                                (purpose, item.id().as_str().to_owned()),
                                Kept {
                                    digest: item.digest(),
                                    got,
                                },
                            );
                        }
                    }
                }
                Err(err) => {
                    report.unanswered += chunk.len();
                    if let AskError::Status {
                        retry_after: Some(wait),
                        ..
                    } = &err
                    {
                        report.retry_after = Some(report.retry_after.map_or(*wait, |had| had.max(*wait)));
                    }
                    report.error = Some(err);
                }
            }
        }
        answers
    }
}

/// A title the rules let through, as the judge may read it.
fn title_item(item: &Item) -> Option<Asked> {
    Some(Asked::Title(TitleItem {
        id: JudgeItemId::parse(item.id.as_str()).ok()?,
        title: JudgeText::new(item.title.as_str()).ok()?,
    }))
}

/// The items in requests of at most [`MAX_ITEMS`] items and about [`MAX_REQUEST_BYTES`].
fn chunks(items: &[(usize, Asked)]) -> Vec<&[(usize, Asked)]> {
    let mut chunks = Vec::new();
    let (mut start, mut bytes) = (0, 0);
    for (n, (_, item)) in items.iter().enumerate() {
        let size = serde_json::to_vec(item).map_or(0, |json| json.len());
        if n > start && (n - start == MAX_ITEMS || bytes + size > MAX_REQUEST_BYTES) {
            chunks.push(&items[start..n]);
            (start, bytes) = (n, 0);
        }
        bytes += size;
    }
    if start < items.len() {
        chunks.push(&items[start..]);
    }
    chunks
}

#[cfg(test)]
mod tests {
    use super::*;

    fn title(n: usize, chars: usize) -> (usize, Asked) {
        (
            n,
            Asked::Title(TitleItem {
                id: JudgeItemId::parse(&format!("t{n}")).unwrap(),
                title: JudgeText::new("x".repeat(chars)).unwrap(),
            }),
        )
    }

    #[test]
    fn a_request_holds_at_most_200_items_and_about_256_kb() {
        let many: Vec<_> = (0..450).map(|n| title(n, 10)).collect();
        let sizes: Vec<usize> = chunks(&many).iter().map(|c| c.len()).collect();
        assert_eq!(sizes, [200, 200, 50]);
        // about 520 bytes an item: 256 KB hold some 500, but 200 is the cap first
        let heavy: Vec<_> = (0..200).map(|n| title(n, 500)).collect();
        assert_eq!(chunks(&heavy).len(), 1);
        // a title and 20 labels of 500 characters: some 10 KB an item, so about 25 a request
        let label = JudgeText::new("l".repeat(500)).unwrap();
        let heaviest: Vec<_> = (0..60)
            .map(|n| {
                (
                    n,
                    Asked::Kind(KindItem {
                        id: JudgeItemId::parse(&format!("k{n}")).unwrap(),
                        title: Some(JudgeText::new("x".repeat(500)).unwrap()),
                        labels: vec![label.clone(); 20],
                    }),
                )
            })
            .collect();
        let split = chunks(&heaviest);
        assert!(split.len() >= 3, "{}", split.len());
        for chunk in &split {
            let bytes: usize = chunk
                .iter()
                .map(|(_, item)| serde_json::to_vec(item).unwrap().len())
                .sum();
            assert!(bytes <= MAX_REQUEST_BYTES, "{bytes}");
        }
        assert_eq!(split.iter().map(|c| c.len()).sum::<usize>(), 60);
        let none: Vec<(usize, Asked)> = Vec::new();
        assert!(chunks(&none).is_empty());
    }

    #[test]
    fn a_judge_that_isnt_jev_is_held_to_stricter_thresholds() {
        assert!((choice_at(JudgeMode::Jev) - 0.6).abs() < 1e-9);
        assert!((choice_at(JudgeMode::Chat) - 0.8).abs() < 1e-9);
        // the rules settle no status and no kind, whatever the number
        assert!(choice_at(JudgeMode::Rules).is_infinite());
        assert!(choice_at(JudgeMode::Rules) > 1.0);
        assert!((withhold_at(JudgeMode::Jev) - 0.5).abs() < 1e-9);
        assert!((withhold_at(JudgeMode::Chat) - 0.3).abs() < 1e-9);
        // the rules' answer is no answer: a title is withheld whatever its p
        assert!(withhold_at(JudgeMode::Rules).abs() < 1e-9);
    }

    #[test]
    fn the_client_never_prints_the_token() {
        let client = JudgeClient::new("http://core:8081/", Some("mcy_probe_secret".to_owned())).unwrap();
        let printed = format!("{client:?}");
        assert!(!printed.contains("secret"), "{printed}");
        assert!(printed.contains("<set>"), "{printed}");
        assert!(printed.contains("http://core:8081/internal/judge"), "{printed}");
        let without = JudgeClient::new("http://core:8081", None).unwrap();
        assert!(format!("{without:?}").contains("<none>"));
    }
}
