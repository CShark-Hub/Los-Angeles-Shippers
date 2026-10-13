//! `POST /internal/judge`: what a probe asks the core's judge, and what the core answers. A probe
//! never calls a model of its own in the cloud, so the questions its rules can't settle come here,
//! batched by purpose.
//!
//! The request is the one body that carries the repository's own text to the core: titles and
//! labels as [`JudgeText`], bounded, never shown to a person, and redacted by the core with the
//! project's rules before the judge reads them. The reply carries numbers and enums only, so
//! nothing the probe sent comes back.

use std::borrow::Cow;
use std::collections::HashSet;
use std::fmt;

use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::de::Error as _;
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::ToSchema;

use crate::enums::JudgeMode;
use crate::facts::{BuildStatus, Kind, Ratio, Status};
use crate::ids::JudgeItemId;

/// The most items one request holds.
pub const MAX_ITEMS: usize = 200;
/// The most characters one text has.
pub const MAX_TEXT_CHARS: usize = 500;
/// The most labels, and the most linked issues, one item lists.
pub const MAX_PER_ITEM: usize = 20;

/// A repository text for the judge: a title or a label as the probe read it, 1 to 500 characters
/// without control characters. Unlike [`SafeText`](crate::SafeText) it is never shown to anyone:
/// the core redacts it and only the judge reads it. `Debug` prints its length, never the text.
#[derive(Clone, PartialEq, Eq)]
pub struct JudgeText(String);

/// A text the judge can't be given. It never repeats the text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum JudgeTextError {
    #[error("a text for the judge is empty")]
    Empty,
    #[error("a text for the judge is over 500 characters")]
    TooLong,
    #[error("a text for the judge holds a control character")]
    Control,
}

impl JudgeText {
    pub fn new(text: impl Into<String>) -> Result<Self, JudgeTextError> {
        let text = text.into();
        if text.is_empty() {
            Err(JudgeTextError::Empty)
        } else if text.chars().count() > MAX_TEXT_CHARS {
            Err(JudgeTextError::TooLong)
        } else if text.chars().any(char::is_control) {
            Err(JudgeTextError::Control)
        } else {
            Ok(Self(text))
        }
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for JudgeText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "JudgeText(<{} characters>)", self.0.chars().count())
    }
}

impl Serialize for JudgeText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for JudgeText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::new(String::deserialize(deserializer)?).map_err(D::Error::custom)
    }
}

impl JsonSchema for JudgeText {
    fn schema_name() -> Cow<'static, str> {
        "JudgeText".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "format": "judge-text",
            "minLength": 1,
            "maxLength": MAX_TEXT_CHARS,
            "pattern": "^[^\\u0000-\\u001f\\u007f-\\u009f]+$",
            "description": "A repository text for the judge; the core redacts it before the judge reads it."
        })
    }
}

/// Which question the probe asks about every item of a request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum Purpose {
    /// Which of the statuses the rules point to fits the item.
    StatusFromCandidates,
    /// Whether the item is a feature, a fix or a chore.
    Kind,
    /// Whether the title holds code, an identifier, a path, a secret or a client name.
    TitleSafety,
}

/// `POST /internal/judge`: one purpose, 1 to 200 items, each id once.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(tag = "purpose", rename_all = "snake_case", deny_unknown_fields)]
pub enum JudgeRequest {
    StatusFromCandidates {
        #[serde(deserialize_with = "items")]
        #[schemars(length(min = 1, max = MAX_ITEMS))]
        #[schema(min_items = 1, max_items = 200)]
        items: Vec<StatusItem>,
    },
    Kind {
        #[serde(deserialize_with = "items")]
        #[schemars(length(min = 1, max = MAX_ITEMS))]
        #[schema(min_items = 1, max_items = 200)]
        items: Vec<KindItem>,
    },
    TitleSafety {
        #[serde(deserialize_with = "items")]
        #[schemars(length(min = 1, max = MAX_ITEMS))]
        #[schema(min_items = 1, max_items = 200)]
        items: Vec<TitleItem>,
    },
}

impl JudgeRequest {
    pub fn purpose(&self) -> Purpose {
        match self {
            JudgeRequest::StatusFromCandidates { .. } => Purpose::StatusFromCandidates,
            JudgeRequest::Kind { .. } => Purpose::Kind,
            JudgeRequest::TitleSafety { .. } => Purpose::TitleSafety,
        }
    }

    /// How many items it holds, 1 to 200.
    pub fn len(&self) -> usize {
        match self {
            JudgeRequest::StatusFromCandidates { items } => items.len(),
            JudgeRequest::Kind { items } => items.len(),
            JudgeRequest::TitleSafety { items } => items.len(),
        }
    }

    /// Never true for a request that was read: it holds at least one item.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

/// What a request checks on each item beyond its fields' own shapes.
trait Item {
    fn id(&self) -> &JudgeItemId;

    fn check(&self) -> Result<(), &'static str> {
        Ok(())
    }
}

/// A status the rules couldn't settle: the statuses they point to, and what the judge may read.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct StatusItem {
    pub id: JudgeItemId,
    /// 2 to 5 statuses, each once, never `unclear`.
    #[serde(deserialize_with = "candidates")]
    #[schemars(length(min = 2, max = 5), extend("uniqueItems" = true))]
    #[schema(min_items = 2, max_items = 5)]
    pub candidates: Vec<Status>,
    /// `null` when the manifest's `judge_context` leaves titles out.
    #[serde(deserialize_with = "present")]
    #[schema(required = true)]
    pub title: Option<JudgeText>,
    /// Empty when the manifest's `judge_context` leaves labels out; at most 20.
    #[serde(deserialize_with = "at_most_20")]
    #[schemars(length(max = MAX_PER_ITEM))]
    #[schema(max_items = 20)]
    pub labels: Vec<JudgeText>,
    pub signals: ItemSignals,
}

impl Item for StatusItem {
    fn id(&self) -> &JudgeItemId {
        &self.id
    }
}

/// The states the status rules read.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ItemSignals {
    /// `null` for an issue on its own.
    #[serde(deserialize_with = "present")]
    #[schema(required = true)]
    pub pr: Option<PullSignals>,
    /// The linked issues, or the issue itself; at most 20.
    #[serde(deserialize_with = "at_most_20")]
    #[schemars(length(max = MAX_PER_ITEM))]
    #[schema(max_items = 20)]
    pub issues: Vec<IssueSignals>,
    /// The latest CI run's status.
    pub ci: BuildStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct PullSignals {
    pub state: PullState,
    pub review_requested: bool,
    /// Updated in the last 14 days.
    pub updated_14d: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum PullState {
    Open,
    Draft,
    Merged,
    Closed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct IssueSignals {
    pub open: bool,
    /// It carries one of the manifest's blocking labels.
    pub blocked: bool,
}

/// An item whose kind the labels didn't settle: a title, at least one label, or both.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct KindItem {
    pub id: JudgeItemId,
    /// `null` when the manifest's `judge_context` leaves titles out.
    #[serde(deserialize_with = "present")]
    #[schema(required = true)]
    pub title: Option<JudgeText>,
    /// Empty when the manifest's `judge_context` leaves labels out; at most 20.
    #[serde(deserialize_with = "at_most_20")]
    #[schemars(length(max = MAX_PER_ITEM))]
    #[schema(max_items = 20)]
    pub labels: Vec<JudgeText>,
}

impl Item for KindItem {
    fn id(&self) -> &JudgeItemId {
        &self.id
    }

    fn check(&self) -> Result<(), &'static str> {
        if self.title.is_none() && self.labels.is_empty() {
            return Err("a kind item needs a title or a label");
        }
        Ok(())
    }
}

/// A title the rules let through, for the judge to look at again.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TitleItem {
    pub id: JudgeItemId,
    pub title: JudgeText,
}

impl Item for TitleItem {
    fn id(&self) -> &JudgeItemId {
        &self.id
    }
}

/// The core's reply: the request's purpose, the judge that answered, and one answer per item in
/// the request's order. Numbers and enums only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(tag = "purpose", rename_all = "snake_case")]
pub enum JudgeReply {
    StatusFromCandidates {
        judge_mode: JudgeMode,
        items: Vec<StatusAnswer>,
    },
    Kind {
        judge_mode: JudgeMode,
        items: Vec<KindAnswer>,
    },
    TitleSafety {
        judge_mode: JudgeMode,
        items: Vec<TitleAnswer>,
    },
}

impl JudgeReply {
    pub fn purpose(&self) -> Purpose {
        match self {
            JudgeReply::StatusFromCandidates { .. } => Purpose::StatusFromCandidates,
            JudgeReply::Kind { .. } => Purpose::Kind,
            JudgeReply::TitleSafety { .. } => Purpose::TitleSafety,
        }
    }

    pub fn judge_mode(&self) -> JudgeMode {
        match self {
            JudgeReply::StatusFromCandidates { judge_mode, .. }
            | JudgeReply::Kind { judge_mode, .. }
            | JudgeReply::TitleSafety { judge_mode, .. } => *judge_mode,
        }
    }
}

/// The candidate the judge chose, and its probability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct StatusAnswer {
    pub id: JudgeItemId,
    pub status: Status,
    pub p: Ratio,
}

/// `feature`, `fix` or `chore`, and its probability.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct KindAnswer {
    pub id: JudgeItemId,
    pub kind: Kind,
    pub p: Ratio,
}

/// The probability that the title holds code, an identifier, a path, a secret or a client name.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, JsonSchema, ToSchema)]
pub struct TitleAnswer {
    pub id: JudgeItemId,
    pub p: Ratio,
}

/// Deserializes a field that must be present, even when it is `null`.
fn present<'de, D: Deserializer<'de>, T: Deserialize<'de>>(deserializer: D) -> Result<Option<T>, D::Error> {
    Option::deserialize(deserializer)
}

fn items<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de> + Item,
{
    let items = Vec::<T>::deserialize(deserializer)?;
    if !(1..=MAX_ITEMS).contains(&items.len()) {
        return Err(D::Error::custom("a judge request holds 1 to 200 items"));
    }
    let mut ids = HashSet::new();
    if !items.iter().all(|item| ids.insert(item.id())) {
        return Err(D::Error::custom("an item's id is used twice"));
    }
    for item in &items {
        item.check().map_err(D::Error::custom)?;
    }
    Ok(items)
}

fn at_most_20<'de, D, T>(deserializer: D) -> Result<Vec<T>, D::Error>
where
    D: Deserializer<'de>,
    T: Deserialize<'de>,
{
    let list = Vec::<T>::deserialize(deserializer)?;
    if list.len() > MAX_PER_ITEM {
        return Err(D::Error::custom("an item lists at most 20 labels or issues"));
    }
    Ok(list)
}

fn candidates<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Vec<Status>, D::Error> {
    let candidates = Vec::<Status>::deserialize(deserializer)?;
    let distinct: HashSet<_> = candidates.iter().collect();
    if !(2..=5).contains(&candidates.len())
        || distinct.len() != candidates.len()
        || distinct.contains(&Status::Unclear)
    {
        return Err(D::Error::custom(
            "the candidates are 2 to 5 distinct statuses, never unclear",
        ));
    }
    Ok(candidates)
}

#[cfg(test)]
mod tests {
    use serde_json::{Value, json};

    use super::*;

    fn status_item(id: &str) -> Value {
        json!({
            "id": id, "candidates": ["in_review", "blocked"], "title": "Invoice export in the new format",
            "labels": ["export", "blocked"],
            "signals": { "pr": { "state": "open", "review_requested": true, "updated_14d": true },
                         "issues": [{ "open": true, "blocked": true }], "ci": "passed" }
        })
    }

    fn read(body: Value) -> Result<JudgeRequest, serde_json::Error> {
        serde_json::from_value(body)
    }

    #[test]
    fn a_request_is_its_purpose_and_items() {
        let request =
            read(json!({ "purpose": "status_from_candidates", "items": [status_item("f_pull412")] }))
                .unwrap();
        assert_eq!(request.purpose(), Purpose::StatusFromCandidates);
        assert_eq!(request.len(), 1);
        let JudgeRequest::StatusFromCandidates { items } = &request else {
            panic!("not a status request: {request:?}");
        };
        assert_eq!(items[0].candidates, [Status::InReview, Status::Blocked]);
        assert_eq!(items[0].signals.ci, BuildStatus::Passed);
    }

    #[test]
    fn the_purpose_fixes_the_items_shape() {
        // a status item is not a title item, and no purpose takes another's fields
        assert!(read(json!({ "purpose": "title_safety", "items": [status_item("a")] })).is_err());
        assert!(read(json!({ "purpose": "kind", "items": [{ "id": "a", "title": "Fix totals" }] })).is_err());
        assert!(
            read(json!({ "purpose": "kind", "items": [{ "id": "a", "title": null, "labels": ["bug"] }] }))
                .is_ok()
        );
        assert!(read(json!({ "purpose": "read_file", "items": [{ "id": "a", "title": "x" }] })).is_err());
        assert!(read(json!({ "items": [{ "id": "a", "title": "x" }] })).is_err());
    }

    #[test]
    fn unknown_fields_are_refused_at_every_level() {
        let title = |item: Value| read(json!({ "purpose": "title_safety", "items": [item] }));
        assert!(title(json!({ "id": "a", "title": "Fix totals" })).is_ok());
        assert!(title(json!({ "id": "a", "title": "Fix totals", "body": "x" })).is_err());
        assert!(
            read(json!({ "purpose": "title_safety", "items": [{ "id": "a", "title": "x" }], "project": "delta" }))
                .is_err()
        );
        let mut item = status_item("a");
        item["signals"]["pr"]["branch"] = json!("feature/x");
        assert!(read(json!({ "purpose": "status_from_candidates", "items": [item] })).is_err());
    }

    #[test]
    fn a_request_holds_1_to_200_items_each_id_once() {
        let items = |n: usize| {
            (0..n)
                .map(|i| json!({ "id": format!("t{i}"), "title": "x" }))
                .collect::<Vec<_>>()
        };
        assert!(read(json!({ "purpose": "title_safety", "items": items(0) })).is_err());
        assert!(read(json!({ "purpose": "title_safety", "items": items(200) })).is_ok());
        assert!(read(json!({ "purpose": "title_safety", "items": items(201) })).is_err());
        let twice = [
            json!({ "id": "t", "title": "x" }),
            json!({ "id": "t", "title": "y" }),
        ];
        assert!(read(json!({ "purpose": "title_safety", "items": twice })).is_err());
    }

    #[test]
    fn a_text_is_1_to_500_characters_on_one_line() {
        let title =
            |text: &str| read(json!({ "purpose": "title_safety", "items": [{ "id": "a", "title": text }] }));
        assert!(title(&"é".repeat(500)).is_ok(), "500 characters, more bytes");
        assert!(title(&"a".repeat(501)).is_err());
        assert!(title("").is_err());
        assert!(title("two\nlines").is_err());
        assert!(title("a\u{7}bell").is_err());
        assert_eq!(JudgeText::new("x".repeat(501)), Err(JudgeTextError::TooLong));
    }

    #[test]
    fn a_text_never_shows_in_debug() {
        let text = JudgeText::new("Export for the client portal").unwrap();
        let printed = format!("{text:?}");
        assert!(!printed.contains("client"), "{printed}");
        assert!(printed.contains("28 characters"), "{printed}");
    }

    #[test]
    fn candidates_are_2_to_5_distinct_and_never_unclear() {
        let with = |candidates: Value| {
            let mut item = status_item("a");
            item["candidates"] = candidates;
            read(json!({ "purpose": "status_from_candidates", "items": [item] }))
        };
        assert!(
            with(json!([
                "done",
                "in_review",
                "in_progress",
                "blocked",
                "not_started"
            ]))
            .is_ok()
        );
        assert!(with(json!(["blocked"])).is_err());
        assert!(with(json!(["blocked", "blocked"])).is_err());
        assert!(with(json!(["blocked", "unclear"])).is_err());
    }

    #[test]
    fn an_item_lists_at_most_20_labels_and_issues() {
        let labels: Vec<_> = (0..21).map(|i| format!("label{i}")).collect();
        let mut item = status_item("a");
        item["labels"] = json!(labels[..20]);
        assert!(read(json!({ "purpose": "status_from_candidates", "items": [item.clone()] })).is_ok());
        item["labels"] = json!(labels);
        assert!(read(json!({ "purpose": "status_from_candidates", "items": [item] })).is_err());
        let mut item = status_item("a");
        item["signals"]["issues"] = json!(vec![json!({ "open": true, "blocked": false }); 21]);
        assert!(read(json!({ "purpose": "status_from_candidates", "items": [item] })).is_err());
    }

    #[test]
    fn titles_and_pull_requests_are_stated_even_when_null() {
        let mut item = status_item("a");
        item["title"] = Value::Null;
        item["signals"]["pr"] = Value::Null;
        assert!(read(json!({ "purpose": "status_from_candidates", "items": [item.clone()] })).is_ok());
        item.as_object_mut().unwrap().remove("title");
        assert!(read(json!({ "purpose": "status_from_candidates", "items": [item] })).is_err());
    }

    #[test]
    fn a_reply_is_numbers_and_enums() {
        let reply: JudgeReply = serde_json::from_value(json!({
            "purpose": "kind", "judge_mode": "jev", "items": [{ "id": "f_pull412", "kind": "fix", "p": 0.93 }]
        }))
        .unwrap();
        assert_eq!(reply.purpose(), Purpose::Kind);
        assert_eq!(reply.judge_mode(), JudgeMode::Jev);
        assert!(
            serde_json::from_value::<JudgeReply>(json!({
                "purpose": "title_safety", "judge_mode": "chat", "items": [{ "id": "a", "p": 1.5 }]
            }))
            .is_err()
        );
    }
}
