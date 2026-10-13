//! The OpenAPI document of the core's API: a schema for every request and response body, every
//! stream event, and the probe's registration and judge requests. The core adds the paths; `just openapi` writes the
//! document to `openapi.json`, and `just types` turns it into the web client's TypeScript.
//!
//! The leaf types (identifiers, [`SafeText`], [`DisplayName`], [`RedactedQuestion`], ratios and
//! limits) write their JSON Schema by hand. Their OpenAPI schema is that same JSON, read as an
//! OpenAPI 3.1 schema, so the two descriptions can't drift apart.

use std::borrow::Cow;

use schemars::{JsonSchema, SchemaGenerator};
use serde_json::Value;
use utoipa::openapi::schema::Schema;
use utoipa::openapi::{OpenApi, RefOr};

use crate::admin::{AdminOverview, AdminPeople, AdminRepositories};
use crate::api::{
    Accepted, Answer, Approval, ApprovalDecision, ApprovalList, AuditItem, AuditPage, ErrorBody, Feedback,
    Me, NewQuestion, ProbeRegistration, ProjectList, QuestionState, ServiceHealth, SessionRequest,
    TokenSecret, Usd,
};
use crate::events::Event;
use crate::facts::{Ratio, SchemaId, Score};
use crate::ids::{
    ApprovalId, CheckId, CommitShort, Email, EventType, FactId, FieldPointer, IssueNumber, JudgeItemId,
    ModelSlug, ModuleName, ProjectId, QuestionId, Sha256Hex, TokenPublicId, UserId,
};
use crate::manifest::ManifestVersion;
use crate::probe_judge::{JudgeReply, JudgeRequest, JudgeText};
use crate::redact::{DisplayName, RedactedQuestion, SafeText};
use crate::tools::Limit;

/// The OpenAPI schema of a type whose JSON Schema is written by hand. A `const` becomes a
/// one-value `enum`, which OpenAPI tools read more widely.
pub(crate) fn leaf_schema<T: JsonSchema>() -> RefOr<Schema> {
    let mut json = T::json_schema(&mut SchemaGenerator::default()).to_value();
    if let Some(map) = json.as_object_mut()
        && let Some(value) = map.remove("const")
    {
        map.insert("enum".to_owned(), Value::Array(vec![value]));
    }
    #[allow(
        clippy::expect_used,
        reason = "the leaf schemas are literals of this crate; a test reads every one of them"
    )]
    RefOr::T(serde_json::from_value(json).expect("a leaf JSON Schema reads as an OpenAPI schema"))
}

macro_rules! leaf_schemas {
    ($($ty:ty),* $(,)?) => {
        $(
            impl utoipa::PartialSchema for $ty {
                fn schema() -> RefOr<Schema> {
                    leaf_schema::<$ty>()
                }
            }

            impl utoipa::ToSchema for $ty {
                fn name() -> Cow<'static, str> {
                    <$ty as JsonSchema>::schema_name()
                }
            }
        )*

        /// Every leaf type's name with its JSON Schema and its OpenAPI schema, for the test that
        /// the two agree.
        #[cfg(test)]
        pub(crate) fn leaf_pairs() -> Vec<(Cow<'static, str>, Value, Value)> {
            vec![$(
                (
                    <$ty as JsonSchema>::schema_name(),
                    <$ty as JsonSchema>::json_schema(&mut SchemaGenerator::default()).to_value(),
                    serde_json::to_value(<$ty as utoipa::PartialSchema>::schema()).unwrap_or_default(),
                ),
            )*]
        }
    };
}

leaf_schemas!(
    SafeText,
    DisplayName,
    RedactedQuestion,
    ProjectId,
    FactId,
    CommitShort,
    ModuleName,
    IssueNumber,
    QuestionId,
    UserId,
    ApprovalId,
    TokenPublicId,
    CheckId,
    EventType,
    JudgeItemId,
    JudgeText,
    Sha256Hex,
    FieldPointer,
    Email,
    ModelSlug,
    Ratio,
    Score,
    Limit,
    SchemaId,
    ManifestVersion,
    TokenSecret,
    Usd,
);

/// The paths the core serves, each described here so `just openapi` writes the same document the
/// core serves. The functions are never called; `utoipa` reads their attributes.
#[allow(dead_code, reason = "descriptions of the core's routes, read by utoipa only")]
mod paths {
    use crate::admin::{AdminOverview, AdminPeople, AdminRepositories};
    use crate::api::{
        Accepted, ApprovalDecision, ApprovalList, AuditPage, ErrorBody, Feedback, Me, NewQuestion,
        ProjectList, QuestionState, ServiceHealth, SessionRequest,
    };

    #[utoipa::path(get, path = "/v1/health", tag = "service",
        responses((status = 200, description = "The service is up.", body = ServiceHealth)))]
    fn health() {}

    #[utoipa::path(get, path = "/v1/openapi.json", tag = "service",
        responses((status = 200, description = "This document.", content_type = "application/json")))]
    fn openapi() {}

    #[utoipa::path(post, path = "/v1/session", tag = "people",
        params(
            ("Origin" = String, Header, description = "The web client's own origin, as the server is set up with it (MERCURY_PUBLIC_ORIGIN)."),
            ("X-Mercury-Client" = String, Header, description = "`web`."),
        ),
        request_body = SessionRequest,
        responses(
            (status = 204, description = "Signed in: the cookie `__Host-mercury_session` is set (HttpOnly, Secure, SameSite=Strict, Path=/), for the server's session time and never beyond the token's expiry."),
            (status = 400, description = "A malformed body.", body = ErrorBody),
            (status = 401, description = "No valid token: unauthenticated, or token_expired.", body = ErrorBody),
            (status = 403, description = "Not from the web client's own page: the Origin or the X-Mercury-Client header is wrong or missing.", body = ErrorBody),
            (status = 413, description = "A body over 64 KB.", body = ErrorBody),
            (status = 429, description = "Too many sign-ins from this address; see Retry-After.", body = ErrorBody),
        ))]
    fn sign_in() {}

    #[utoipa::path(delete, path = "/v1/session", tag = "people", security(("session" = [])),
        params(
            ("Origin" = String, Header, description = "The web client's own origin, as the server is set up with it (MERCURY_PUBLIC_ORIGIN)."),
            ("X-Mercury-Client" = String, Header, description = "`web`."),
        ),
        responses(
            (status = 204, description = "Signed out: the session is ended and the cookie cleared."),
            (status = 401, description = "No valid session.", body = ErrorBody),
            (status = 403, description = "Not from the web client's own page.", body = ErrorBody),
        ))]
    fn sign_out() {}

    #[utoipa::path(get, path = "/v1/me", tag = "people", security(("token" = []), ("session" = [])),
        responses(
            (status = 200, description = "Who the token belongs to.", body = Me),
            (status = 401, description = "No valid token.", body = ErrorBody),
        ))]
    fn me() {}

    #[utoipa::path(get, path = "/v1/projects", tag = "people", security(("token" = []), ("session" = [])),
        responses(
            (status = 200, description = "The projects the person may ask about.", body = ProjectList),
            (status = 401, description = "No valid token.", body = ErrorBody),
        ))]
    fn projects() {}

    #[utoipa::path(post, path = "/v1/questions", tag = "questions", security(("token" = []), ("session" = [])),
        params(("Idempotency-Key" = String, Header, description = "1 to 128 printable ASCII characters; a repeat returns the first question's id.")),
        request_body = NewQuestion,
        responses(
            (status = 202, description = "Accepted; follow its events.", body = Accepted),
            (status = 400, description = "A malformed body or key; a clarify of a question that isn't the caller's or didn't end with a clarify; a follows that isn't the caller's own question rejected by an approver (the same message whatever the reason).", body = ErrorBody),
            (status = 401, description = "No valid token.", body = ErrorBody),
            (status = 403, description = "With the session cookie but not from the web client's own page (Origin, X-Mercury-Client).", body = ErrorBody),
            (status = 413, description = "A body over 64 KB.", body = ErrorBody),
            (status = 429, description = "Over the token's rate limit; see Retry-After.", body = ErrorBody),
        ))]
    fn ask() {}

    #[utoipa::path(get, path = "/v1/questions/{id}/events", tag = "questions", security(("token" = []), ("session" = [])),
        params(
            ("id" = String, Path, description = "The question's id."),
            ("Last-Event-ID" = Option<u64>, Header, description = "Replay the events after this one."),
        ),
        responses(
            (status = 200, description = "Server-sent events: `event:` an event's name, `id:` its audit seq, `data:` its data (see the Event schema). Ends after a terminal event.", content_type = "text/event-stream"),
            (status = 404, description = "Not the caller's question, or no such question.", body = ErrorBody),
        ))]
    fn events() {}

    #[utoipa::path(get, path = "/v1/questions/{id}", tag = "questions", security(("token" = []), ("session" = [])),
        params(("id" = String, Path, description = "The question's id.")),
        responses(
            (status = 200, description = "Where the question stands.", body = QuestionState),
            (status = 404, description = "Not the caller's question, or no such question.", body = ErrorBody),
        ))]
    fn question() {}

    #[utoipa::path(post, path = "/v1/questions/{id}/feedback", tag = "questions", security(("token" = []), ("session" = [])),
        params(("id" = String, Path, description = "The question's id.")),
        request_body = Feedback,
        responses(
            (status = 204, description = "Recorded; a second one replaces the first."),
            (status = 403, description = "With the session cookie but not from the web client's own page (Origin, X-Mercury-Client).", body = ErrorBody),
            (status = 404, description = "Not the caller's question, or no such question.", body = ErrorBody),
        ))]
    fn feedback() {}

    #[utoipa::path(get, path = "/v1/approvals", tag = "approvals", security(("token" = []), ("session" = [])),
        params(("status" = Option<String>, Query, description = "`pending`, the only one; it may be left out.")),
        responses(
            (status = 200, description = "The held answers, pending and unexpired, of every project the caller approves, oldest first; the caller's own questions come with asked_by_me and without draft and flags, since the asker reads nothing of a held answer before the decision. Empty for someone who approves nothing.", body = ApprovalList),
            (status = 400, description = "A status other than pending, or another parameter.", body = ErrorBody),
            (status = 401, description = "No valid token.", body = ErrorBody),
        ))]
    fn approvals() {}

    #[utoipa::path(post, path = "/v1/approvals/{id}", tag = "approvals", security(("token" = []), ("session" = [])),
        params(("id" = String, Path, description = "The approval's id.")),
        request_body = ApprovalDecision,
        responses(
            (status = 204, description = "Decided: the question goes on with the draft unchanged (answer, done) or with refused rejected_by_approver, the note as its hint."),
            (status = 400, description = "A malformed body, or a rejection without a note of 1 to 500 characters on one line.", body = ErrorBody),
            (status = 401, description = "No valid token.", body = ErrorBody),
            (status = 403, description = "The caller's own question; or with the session cookie but not from the web client's own page.", body = ErrorBody),
            (status = 404, description = "No such approval, or one of a project the caller doesn't approve.", body = ErrorBody),
            (status = 409, description = "Already decided, or expired after 24 hours.", body = ErrorBody),
            (status = 413, description = "A body over 64 KB.", body = ErrorBody),
        ))]
    fn decide() {}

    #[utoipa::path(get, path = "/v1/audit", tag = "audit", security(("token" = []), ("session" = [])),
        params(
            ("project" = Option<String>, Query, description = "The row's project, else its question's."),
            ("user" = Option<String>, Query, description = "The asker's e-mail address, or for a row without a question the one its payload names; case ignored."),
            ("token" = Option<String>, Query, description = "A token's public id: its questions, and the rows without a question that name it (sign-ins, refusals, its creation and revocation)."),
            ("question" = Option<String>, Query, description = "A question's id."),
            ("class" = Option<String>, Query, description = "The question's class; rows without a question don't match."),
            ("outcome" = Option<String>, Query, description = "The question's status: running, pending_signoff, delivered, refused, clarify or failed; rows without a question don't match."),
            ("from" = Option<String>, Query, description = "From this time on, included: RFC 3339, or YYYY-MM-DD for its midnight UTC."),
            ("to" = Option<String>, Query, description = "Before this time, not included: RFC 3339, or YYYY-MM-DD for its midnight UTC."),
            ("limit" = Option<u32>, Query, description = "Rows per page, 1 to 500; 100 by default."),
            ("cursor" = Option<String>, Query, description = "The `next` of the page before."),
        ),
        responses(
            (status = 200, description = "A page of rows in seq order: every row, for an admin, the only one who reads the audit.", body = AuditPage),
            (status = 400, description = "A filter, the limit or the cursor doesn't parse.", body = ErrorBody),
            (status = 401, description = "No valid token.", body = ErrorBody),
            (status = 403, description = "Not an admin. What Mercury did with a person's own question is in its events.", body = ErrorBody),
        ))]
    fn audit() {}

    #[utoipa::path(get, path = "/v1/audit/export", tag = "audit", security(("token" = []), ("session" = [])),
        params(
            ("format" = String, Query, description = "`jsonl`."),
            ("project" = Option<String>, Query, description = "As for /v1/audit."),
            ("user" = Option<String>, Query, description = "As for /v1/audit."),
            ("token" = Option<String>, Query, description = "As for /v1/audit."),
            ("question" = Option<String>, Query, description = "As for /v1/audit."),
            ("class" = Option<String>, Query, description = "As for /v1/audit."),
            ("outcome" = Option<String>, Query, description = "As for /v1/audit."),
            ("from" = Option<String>, Query, description = "As for /v1/audit."),
            ("to" = Option<String>, Query, description = "As for /v1/audit."),
        ),
        responses(
            (status = 200, description = "Every matching row up to the last one at the request's start, one AuditItem per line in seq order, streamed. The export is itself an audit row, `audit_exported`, written first.", content_type = "application/jsonl"),
            (status = 400, description = "`format` isn't jsonl, or a filter doesn't parse.", body = ErrorBody),
            (status = 401, description = "No valid token.", body = ErrorBody),
            (status = 403, description = "Not an admin.", body = ErrorBody),
        ))]
    fn audit_export() {}

    #[utoipa::path(get, path = "/v1/admin/overview", tag = "admin", security(("token" = []), ("session" = [])),
        params(("window" = Option<String>, Query, description = "`24h` (in hours), `7d` or `30d` (in days); `7d` by default.")),
        responses(
            (status = 200, description = "The questions of the window by outcome, channel, project, class, refusal and judge, their durations and cost, the sign-offs, today's spend against the budget, the alerts that hold and the settings the core runs with. Counts and metadata only: no question's text, answer or fact. The read is itself an audit row, `admin_viewed`.", body = AdminOverview),
            (status = 400, description = "A window other than 24h, 7d or 30d, or another parameter.", body = ErrorBody),
            (status = 401, description = "No valid token.", body = ErrorBody),
            (status = 403, description = "Not an admin.", body = ErrorBody),
        ))]
    fn admin_overview() {}

    #[utoipa::path(get, path = "/v1/admin/people", tag = "admin", security(("token" = []), ("session" = [])),
        responses(
            (status = 200, description = "Everyone, disabled people too: role, grants with approver flags and what each lets them ask, tokens by public id, label and dates (never a secret), their questions of the last 30 days. Read-only: people, roles, grants and tokens change through the admin commands on the server. The read is itself an audit row, `admin_viewed`.", body = AdminPeople),
            (status = 400, description = "A parameter; the route takes none.", body = ErrorBody),
            (status = 401, description = "No valid token.", body = ErrorBody),
            (status = 403, description = "Not an admin.", body = ErrorBody),
        ))]
    fn admin_people() {}

    #[utoipa::path(get, path = "/v1/admin/repositories", tag = "admin", security(("token" = []), ("session" = [])),
        responses(
            (status = 200, description = "Every project projects.yaml lists or a probe registered: its probe's state, freshness, access mode and what limited it, its quarantine, and the people granted it. Read-only: a quarantine changes through the admin commands on the server, an access mode through the files there. The read is itself an audit row, `admin_viewed`.", body = AdminRepositories),
            (status = 400, description = "A parameter; the route takes none.", body = ErrorBody),
            (status = 401, description = "No valid token.", body = ErrorBody),
            (status = 403, description = "Not an admin.", body = ErrorBody),
        ))]
    fn admin_repositories() {}
}

/// The personal token, `Authorization: Bearer mcy_<public_id>_<secret>`, or the web client's session
/// cookie, which `POST /v1/session` sets in exchange for the token.
struct TokenAuth;

impl utoipa::Modify for TokenAuth {
    fn modify(&self, doc: &mut OpenApi) {
        use utoipa::openapi::security::{ApiKey, ApiKeyValue, Http, HttpAuthScheme, SecurityScheme};
        let components = doc.components.get_or_insert_with(Default::default);
        components.add_security_scheme("token", SecurityScheme::Http(Http::new(HttpAuthScheme::Bearer)));
        components.add_security_scheme(
            "session",
            SecurityScheme::ApiKey(ApiKey::Cookie(ApiKeyValue::with_description(
                "__Host-mercury_session",
                "The web client's session. A request that changes state with it must send Origin (the server's own origin) and X-Mercury-Client: web, or it gets 403. Never on /mcp.",
            ))),
        );
    }
}

#[derive(utoipa::OpenApi)]
#[openapi(
    paths(
        paths::health,
        paths::openapi,
        paths::sign_in,
        paths::sign_out,
        paths::me,
        paths::projects,
        paths::ask,
        paths::events,
        paths::question,
        paths::feedback,
        paths::approvals,
        paths::decide,
        paths::audit,
        paths::audit_export,
        paths::admin_overview,
        paths::admin_people,
        paths::admin_repositories,
    ),
    modifiers(&TokenAuth),
    info(
        title = "Mercury core API",
        description = "The core's HTTP API: the bodies of every request and response, and the events of a question's stream.",
        version = "0.0.0",
    ),
    components(schemas(
        ServiceHealth,
        SessionRequest,
        Me,
        ProjectList,
        NewQuestion,
        Accepted,
        QuestionState,
        Answer,
        Feedback,
        ApprovalList,
        Approval,
        ApprovalDecision,
        AuditPage,
        AuditItem,
        AdminOverview,
        AdminPeople,
        AdminRepositories,
        ErrorBody,
        Event,
        ProbeRegistration,
        JudgeRequest,
        JudgeReply,
    ))
)]
struct ApiDoc;

/// The OpenAPI document with every body's schema. The core merges its paths into it.
pub fn document() -> OpenApi {
    let mut doc = <ApiDoc as utoipa::OpenApi>::openapi();
    // The crate has no licence of its own to state; leave the field out rather than empty.
    doc.info.license = None;
    doc
}

/// The document as `openapi.json` holds it: pretty-printed, ending with a newline.
pub fn document_json() -> String {
    #[allow(
        clippy::expect_used,
        reason = "the document is plain data; serializing it to JSON can't fail"
    )]
    let mut json = document()
        .to_pretty_json()
        .expect("the OpenAPI document serializes");
    json.push('\n');
    json
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn openapi_and_json_schema_agree_on_every_leaf_type() {
        for (name, json_schema, openapi) in leaf_pairs() {
            let mut expected = json_schema.clone();
            if let Some(map) = expected.as_object_mut()
                && let Some(value) = map.remove("const")
            {
                map.insert("enum".to_owned(), Value::Array(vec![value]));
            }
            assert_eq!(openapi, expected, "{name}: the OpenAPI schema lost a keyword");
        }
    }

    #[test]
    fn the_version_is_the_product_version() {
        assert_eq!(document().info.version, env!("CARGO_PKG_VERSION"));
    }
}
