//! The audit chain: every event of a question, every admin action and every probe event as one
//! row of `audit_events`, each row's hash covering the one before, so an edited or deleted row is
//! found by `admin audit verify`.
//!
//! Only an admin reads it, through `GET /v1/audit`, its export or `admin audit export`; anyone else
//! is refused before a row is read.
//!
//! "No audit, no answer": the only way to get the [`Recorded`] an event needs before it is sent is
//! a successful append, so nothing reaches a client without its row, and a failed append is an
//! error the caller has to handle.

use chrono::{DateTime, Utc};
use mercury_schema::api::{AuditItem, QuestionStatus};
use mercury_schema::ids::{Email, Sha256Hex, TokenPublicId};
use mercury_schema::{Event, EventType, ProjectId, QuestionClass, QuestionId};
use serde_json::Value;
use serde_json::value::RawValue;
use sha2::{Digest, Sha256};
use sqlx::{Sqlite, Transaction};

use crate::db::{self, Db};

/// The `prev_hash` of the first row.
pub const GENESIS: [u8; 32] = [0; 32];

#[derive(Debug, thiserror::Error)]
pub enum AuditError {
    #[error("the audit row couldn't be written: {0}")]
    Write(#[from] sqlx::Error),
    #[error("the event's data can't be written as JSON")]
    Payload,
}

/// An event that is in the audit, with the `seq` of its row. Only [`record`] and
/// [`Pending::commit`] build one, once the row is committed; the event stream sends a
/// `Recorded<Event>`, with `seq` as the event's id.
#[derive(Debug, Clone, PartialEq)]
#[must_use = "an audit row was written for this; send what it records, or fail the question"]
pub struct Recorded<T> {
    seq: u64,
    item: T,
}

impl<T> Recorded<T> {
    pub fn seq(&self) -> u64 {
        self.seq
    }

    pub fn item(&self) -> &T {
        &self.item
    }

    pub fn into_item(self) -> T {
        self.item
    }
}

/// An event's row, written inside a transaction that has not committed yet, so that something else
/// can change in the same transaction (the question's own row). It becomes a [`Recorded`], the
/// only thing a stream sends, when [`Pending::commit`] has committed that transaction.
#[derive(Debug)]
#[must_use = "the row is rolled back unless the transaction commits through `Pending::commit`"]
pub struct Pending<T> {
    seq: u64,
    item: T,
}

impl<T> Pending<T> {
    pub fn item(&self) -> &T {
        &self.item
    }

    /// The row's `seq`, for a later row of the same transaction to name (a `done` names its
    /// `answer`). Nothing is sent for it before [`Pending::commit`].
    pub fn seq(&self) -> u64 {
        self.seq
    }

    /// Commits `tx`, which holds this row, and only then gives out the [`Recorded`]. A failed commit
    /// leaves no row and no `Recorded`.
    pub async fn commit(self, tx: Transaction<'_, Sqlite>) -> Result<Recorded<T>, AuditError> {
        tx.commit().await?;
        Ok(Recorded {
            seq: self.seq,
            item: self.item,
        })
    }
}

/// One row to append.
#[derive(Debug, Clone, Copy)]
pub struct Entry<'a> {
    /// `None` for admin and probe events.
    pub question: Option<&'a QuestionId>,
    pub project: Option<&'a ProjectId>,
    pub event_type: &'a EventType,
    /// What happened; never a secret.
    pub payload: &'a Value,
}

/// The row's hash, byte for byte as the data model writes it out.
pub fn chain_hash(
    prev_hash: &[u8; 32],
    seq: u64,
    event_type: &str,
    payload_sha256: &[u8; 32],
    created_at: &str,
) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(prev_hash);
    hasher.update(seq.to_be_bytes());
    // a type is a snake_case id of at most 64 characters, so its length fits one byte
    let length = u8::try_from(event_type.len()).unwrap_or(u8::MAX);
    hasher.update([length]);
    hasher.update(event_type.as_bytes());
    hasher.update(payload_sha256);
    hasher.update(created_at.as_bytes());
    hasher.finalize().into()
}

fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

fn hash_of(column: &[u8]) -> Option<[u8; 32]> {
    column.try_into().ok()
}

/// Appends one row and returns its `seq`. The write transaction is taken before the last row is read, so two writers,
/// in this process or another, never share a `seq` or a `prev_hash`.
pub async fn append(db: &Db, entry: Entry<'_>, at: DateTime<Utc>) -> Result<u64, AuditError> {
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let seq = append_in(&mut tx, entry, at).await?;
    tx.commit().await?;
    Ok(seq)
}

/// Appends one row inside `tx` and returns its `seq`. The row is committed or rolled back with
/// whatever else the transaction holds, so a change and its row stand or fall together.
///
/// `tx` must be begun with `BEGIN IMMEDIATE`, as [`append`] does: the write lock is then taken before
/// the last row is read, and two writers never share a `seq` or a `prev_hash`.
pub async fn append_in(
    tx: &mut Transaction<'_, Sqlite>,
    entry: Entry<'_>,
    at: DateTime<Utc>,
) -> Result<u64, AuditError> {
    let payload = serde_json::to_string(entry.payload).map_err(|_| AuditError::Payload)?;
    let payload_sha256 = digest(payload.as_bytes()).to_vec();
    let created_at = db::time(at);
    let question = entry.question.map(QuestionId::as_str);
    let project = entry.project.map(ProjectId::as_str);
    let event_type = entry.event_type.as_str();
    let placeholder = GENESIS.to_vec();
    let prev: Option<Vec<u8>> =
        sqlx::query_scalar!("SELECT hash FROM audit_events ORDER BY seq DESC LIMIT 1")
            .fetch_optional(&mut **tx)
            .await?;
    let prev_hash = prev.as_deref().and_then(hash_of).unwrap_or(GENESIS);
    let prev_column = prev_hash.to_vec();
    // `seq` comes from AUTOINCREMENT, which never hands out a number twice, even after a row
    // is deleted; the hash covers it, so it is written once the number is known
    let seq = sqlx::query!(
        "INSERT INTO audit_events \
         (question_id, project, type, payload_json, payload_sha256, prev_hash, hash, created_at) \
         VALUES (?, ?, ?, ?, ?, ?, ?, ?)",
        question,
        project,
        event_type,
        payload,
        payload_sha256,
        prev_column,
        placeholder,
        created_at
    )
    .execute(&mut **tx)
    .await?
    .last_insert_rowid();
    let seq = u64::try_from(seq).map_err(|_| AuditError::Write(sqlx::Error::RowNotFound))?;
    let payload_digest = hash_of(&payload_sha256).unwrap_or(GENESIS);
    let hash = chain_hash(&prev_hash, seq, event_type, &payload_digest, &created_at).to_vec();
    let seq_column = i64::try_from(seq).unwrap_or(i64::MAX);
    sqlx::query!("UPDATE audit_events SET hash = ? WHERE seq = ?", hash, seq_column)
        .execute(&mut **tx)
        .await?;
    Ok(seq)
}

/// A stream event as its row: the name as the type, exactly its `data` as the payload.
fn wire(event: &Event) -> Result<(EventType, Value), AuditError> {
    let event_type = EventType::parse(event.name()).map_err(|_| AuditError::Payload)?;
    let mut wire = serde_json::to_value(event).map_err(|_| AuditError::Payload)?;
    let data = wire.get_mut("data").map(Value::take).ok_or(AuditError::Payload)?;
    Ok((event_type, data))
}

/// Records one event of a question's stream: its name as the type, exactly its `data` as the
/// payload, so a replay rebuilds it from the row.
pub async fn record(
    db: &Db,
    question: &QuestionId,
    project: Option<&ProjectId>,
    event: Event,
    at: DateTime<Utc>,
) -> Result<Recorded<Event>, AuditError> {
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let pending = record_in(&mut tx, question, project, event, at).await?;
    pending.commit(tx).await
}

/// Writes one event of a question's stream inside `tx`, which is begun like [`append_in`]'s. The
/// caller changes what must follow the event in the same transaction, then commits it through
/// [`Pending::commit`]: the event is `Recorded`, and so may be sent, only once that has committed.
pub async fn record_in(
    tx: &mut Transaction<'_, Sqlite>,
    question: &QuestionId,
    project: Option<&ProjectId>,
    event: Event,
    at: DateTime<Utc>,
) -> Result<Pending<Event>, AuditError> {
    let (event_type, data) = wire(&event)?;
    let entry = Entry {
        question: Some(question),
        project,
        event_type: &event_type,
        payload: &data,
    };
    let seq = append_in(tx, entry, at).await?;
    Ok(Pending { seq, item: event })
}

/// An admin or probe event, without a question; its `seq`. Nothing is sent for it, so it needs no
/// [`Recorded`], but the caller still fails when the row isn't written.
pub async fn action(
    db: &Db,
    event_type: &str,
    project: Option<&ProjectId>,
    payload: &Value,
    at: DateTime<Utc>,
) -> Result<u64, AuditError> {
    let event_type = EventType::parse(event_type).map_err(|_| AuditError::Payload)?;
    let entry = Entry {
        question: None,
        project,
        event_type: &event_type,
        payload,
    };
    append(db, entry, at).await
}

/// [`action`] inside `tx`, which is begun like [`append_in`]'s: an admin change and its row commit
/// together, so a row that can't be written undoes the change.
pub async fn action_in(
    tx: &mut Transaction<'_, Sqlite>,
    event_type: &str,
    project: Option<&ProjectId>,
    payload: &Value,
    at: DateTime<Utc>,
) -> Result<u64, AuditError> {
    let event_type = EventType::parse(event_type).map_err(|_| AuditError::Payload)?;
    let entry = Entry {
        question: None,
        project,
        event_type: &event_type,
        payload,
    };
    append_in(tx, entry, at).await
}

/// Which rows a reader asks for. Every field is optional; they combine with *and*.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Filter {
    /// The row's project, else its question's.
    pub project: Option<ProjectId>,
    /// The asker's e-mail address, or for a row without a question the one its payload names.
    pub user: Option<Email>,
    /// The question's token, or for a row without a question the one its payload names.
    pub token: Option<TokenPublicId>,
    pub question: Option<QuestionId>,
    /// The question's class; a row without a question never matches.
    pub class: Option<QuestionClass>,
    /// The question's status; a row without a question never matches.
    pub outcome: Option<QuestionStatus>,
    /// Included.
    pub from: Option<DateTime<Utc>>,
    /// Not included.
    pub to: Option<DateTime<Utc>>,
}

impl Filter {
    /// The filters as an export's audit row records them: only the ones given.
    pub fn recorded(&self) -> Value {
        let mut map = serde_json::Map::new();
        let mut put = |key: &str, value: Option<Value>| {
            if let Some(value) = value {
                map.insert(key.to_owned(), value);
            }
        };
        put("project", self.project.as_ref().map(|p| Value::from(p.as_str())));
        put("user", self.user.as_ref().map(|u| Value::from(u.as_str())));
        put("token", self.token.as_ref().map(|t| Value::from(t.as_str())));
        put(
            "question",
            self.question.as_ref().map(|q| Value::from(q.as_str())),
        );
        put("class", self.class.as_ref().and_then(wire_name).map(Value::from));
        put(
            "outcome",
            self.outcome.as_ref().and_then(wire_name).map(Value::from),
        );
        put("from", self.from.map(|at| Value::from(db::time(at))));
        put("to", self.to.map(|at| Value::from(db::time(at))));
        Value::Object(map)
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ReadError {
    #[error("the audit can't be read: {0}")]
    Db(#[from] sqlx::Error),
    #[error("audit row {0} doesn't have the shape the audit writes")]
    Shape(u64),
}

/// The name an enum has on the wire and in the database.
fn wire_name<T: serde::Serialize>(value: &T) -> Option<String> {
    match serde_json::to_value(value) {
        Ok(Value::String(name)) => Some(name),
        _ => None,
    }
}

fn hex(bytes: &[u8]) -> Option<Sha256Hex> {
    let text: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
    Sha256Hex::parse(&text).ok()
}

/// The rows that match `filter`, with a `seq` above `after` and, when `upto` is given, not above
/// it; at most `limit`, in `seq` order. Every row: the audit is an admin's alone, and its callers
/// refuse anyone else before they read it.
pub async fn rows(
    db: &Db,
    filter: &Filter,
    after: u64,
    upto: Option<u64>,
    limit: u32,
) -> Result<Vec<AuditItem>, ReadError> {
    Ok(stored(db, filter, after, upto, limit)
        .await?
        .into_iter()
        .map(|row| row.item)
        .collect())
}

/// A row as [`rows`] reads it, with its time and payload as the database holds them: the bytes the
/// hash and the payload's digest were computed from.
struct Stored {
    item: AuditItem,
    created_at: String,
    payload_json: Option<String>,
}

async fn stored(
    db: &Db,
    filter: &Filter,
    after: u64,
    upto: Option<u64>,
    limit: u32,
) -> Result<Vec<Stored>, ReadError> {
    let after = i64::try_from(after).unwrap_or(i64::MAX);
    let upto = upto.map_or(i64::MAX, |upto| i64::try_from(upto).unwrap_or(i64::MAX));
    let project = filter.project.as_ref().map(ProjectId::as_str);
    let user = filter.user.as_ref().map(|email| email.as_str().to_lowercase());
    let token = filter.token.as_ref().map(TokenPublicId::as_str);
    let question = filter.question.as_ref().map(QuestionId::as_str);
    let class = filter.class.as_ref().and_then(wire_name);
    let outcome = filter.outcome.as_ref().and_then(wire_name);
    let from = filter.from.map(db::time);
    let to = filter.to.map(db::time);
    let limit = i64::from(limit);
    // `user` and `token` of a row without a question come from its payload: an admin action names
    // its subject's `email`, a sign-in, a refusal and a token's own rows its `token` or `public_id`
    let found = sqlx::query!(
        r#"SELECT a.seq, a.question_id, a.type AS event_type, a.created_at, a.project,
                  a.payload_json, a.payload_sha256, a.prev_hash, a.hash,
                  COALESCE(u.email, json_extract(a.payload_json, '$.email')) AS "user_email?: String",
                  COALESCE(q.token_public_id, json_extract(a.payload_json, '$.token'),
                           json_extract(a.payload_json, '$.public_id')) AS "token?: String"
           FROM audit_events a
           LEFT JOIN questions q ON q.id = a.question_id
           LEFT JOIN users u ON u.id = q.user_id
           WHERE a.seq > ? AND a.seq <= ?
             AND (? IS NULL OR COALESCE(a.project, q.project) = ?)
             AND (? IS NULL OR lower(COALESCE(u.email, json_extract(a.payload_json, '$.email'))) = ?)
             AND (? IS NULL OR COALESCE(q.token_public_id, json_extract(a.payload_json, '$.token'),
                                        json_extract(a.payload_json, '$.public_id')) = ?)
             AND (? IS NULL OR a.question_id = ?)
             AND (? IS NULL OR q.class = ?)
             AND (? IS NULL OR q.status = ?)
             AND (? IS NULL OR a.created_at >= ?)
             AND (? IS NULL OR a.created_at < ?)
           ORDER BY a.seq
           LIMIT ?"#,
        after,
        upto,
        project,
        project,
        user,
        user,
        token,
        token,
        question,
        question,
        class,
        class,
        outcome,
        outcome,
        from,
        from,
        to,
        to,
        limit,
    )
    .fetch_all(db.pool())
    .await?;
    found
        .into_iter()
        .map(|row| {
            let seq = u64::try_from(row.seq).unwrap_or(0);
            let shape = || ReadError::Shape(seq);
            let item = AuditItem {
                seq,
                question_id: row
                    .question_id
                    .as_deref()
                    .map(QuestionId::parse)
                    .transpose()
                    .map_err(|_| shape())?,
                event_type: EventType::parse(&row.event_type).map_err(|_| shape())?,
                at: db::parse_time(&row.created_at).ok_or_else(shape)?,
                // a payload may name anything; only what has the shape of an address or a token id
                user: row.user_email.as_deref().and_then(|e| Email::parse(e).ok()),
                token: row.token.as_deref().and_then(|t| TokenPublicId::parse(t).ok()),
                project: row
                    .project
                    .as_deref()
                    .map(ProjectId::parse)
                    .transpose()
                    .map_err(|_| shape())?,
                payload: row
                    .payload_json
                    .as_deref()
                    .map(serde_json::from_str)
                    .transpose()
                    .map_err(|_| shape())?,
                payload_sha256: hex(&row.payload_sha256).ok_or_else(shape)?,
                prev_hash: hex(&row.prev_hash).ok_or_else(shape)?,
                hash: hex(&row.hash).ok_or_else(shape)?,
            };
            Ok(Stored {
                item,
                created_at: row.created_at,
                payload_json: row.payload_json,
            })
        })
        .collect()
}

/// One line of an export: an [`AuditItem`]'s fields in its order, with `at` as stored and the
/// payload as its stored JSON, so a row's hash and its payload's digest recompute from the file
/// alone, byte for byte, without the database.
#[derive(serde::Serialize)]
struct ExportRow<'a> {
    seq: u64,
    question_id: &'a Option<QuestionId>,
    #[serde(rename = "type")]
    event_type: &'a EventType,
    at: &'a str,
    user: &'a Option<Email>,
    token: &'a Option<TokenPublicId>,
    project: &'a Option<ProjectId>,
    payload: Option<&'a RawValue>,
    payload_sha256: &'a Sha256Hex,
    prev_hash: &'a Sha256Hex,
    hash: &'a Sha256Hex,
}

fn export_line(row: &Stored, out: &mut Vec<u8>) -> Result<(), ReadError> {
    let shape = || ReadError::Shape(row.item.seq);
    let payload: Option<&RawValue> = row
        .payload_json
        .as_deref()
        .map(serde_json::from_str)
        .transpose()
        .map_err(|_| shape())?;
    let item = &row.item;
    let line = ExportRow {
        seq: item.seq,
        question_id: &item.question_id,
        event_type: &item.event_type,
        at: &row.created_at,
        user: &item.user,
        token: &item.token,
        project: &item.project,
        payload,
        payload_sha256: &item.payload_sha256,
        prev_hash: &item.prev_hash,
        hash: &item.hash,
    };
    serde_json::to_writer(&mut *out, &line).map_err(|_| shape())?;
    out.push(b'\n');
    Ok(())
}

/// Rows an export reads at a time.
pub const EXPORT_BATCH: u32 = 500;

/// One batch of an export, read and written as lines.
pub(crate) struct Batch {
    /// One line per row, each ending in a newline.
    pub(crate) lines: Vec<u8>,
    /// How many rows the lines are.
    pub(crate) rows: u64,
    /// The `seq` to read the next batch after; `None` when this one was the last.
    pub(crate) next: Option<u64>,
}

/// The rows matching `filter` above `after` and up to `upto`, at most [`EXPORT_BATCH`] of them, as
/// export lines. `admin audit export` and `GET /v1/audit/export` both write through it, so their
/// files are the same lines, each row as stored.
pub(crate) async fn export_batch(
    db: &Db,
    filter: &Filter,
    after: u64,
    upto: u64,
) -> Result<Batch, ReadError> {
    let rows = stored(db, filter, after, Some(upto), EXPORT_BATCH).await?;
    let mut batch = Batch {
        lines: Vec::new(),
        rows: 0,
        next: None,
    };
    for row in &rows {
        export_line(row, &mut batch.lines)?;
        batch.rows += 1;
    }
    if rows.len() == EXPORT_BATCH as usize {
        batch.next = rows.last().map(|last| last.item.seq);
    }
    Ok(batch)
}

/// Writes every row matching `filter` up to `upto`, one JSON object per line in `seq` order;
/// returns how many it wrote.
pub async fn export_to(
    db: &Db,
    filter: &Filter,
    upto: u64,
    out: &mut dyn std::io::Write,
) -> Result<u64, ExportError> {
    let mut after = 0;
    let mut written = 0;
    loop {
        let batch = export_batch(db, filter, after, upto).await?;
        out.write_all(&batch.lines)?;
        written += batch.rows;
        match batch.next {
            Some(last) => after = last,
            None => return Ok(written),
        }
    }
}

#[derive(Debug, thiserror::Error)]
pub enum ExportError {
    #[error("{0}")]
    Read(#[from] ReadError),
    #[error("cannot write the export: {0}")]
    Write(#[from] std::io::Error),
}

/// The `seq` of the last row, `0` before the first.
pub async fn last_seq(db: &Db) -> Result<u64, sqlx::Error> {
    let last: Option<i64> = sqlx::query_scalar!(r#"SELECT max(seq) AS "last?: i64" FROM audit_events"#)
        .fetch_one(db.pool())
        .await?;
    Ok(last.and_then(|last| u64::try_from(last).ok()).unwrap_or(0))
}

/// What one retention run cleared.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Retained {
    pub payloads: u64,
    pub questions: u64,
    /// Held answers whose draft and note were emptied.
    pub approvals: u64,
}

/// Retention: the payloads of rows older than `days`, and the texts and answers of questions
/// older than that, their held drafts and the notes on them included, are deleted; the rows, their digests and their hashes stay, so the chain still
/// verifies. When it changed anything, the run writes its own row `audit_retained` in the same
/// transaction, so a run that can't be audited changes nothing.
pub async fn retain(db: &Db, days: u32, now: DateTime<Utc>) -> Result<Retained, AuditError> {
    let cutoff = db::time(now - chrono::TimeDelta::days(i64::from(days)));
    let mut tx = db.pool().begin_with("BEGIN IMMEDIATE").await?;
    let payloads = sqlx::query!(
        "UPDATE audit_events SET payload_json = NULL WHERE created_at < ? AND payload_json IS NOT NULL",
        cutoff
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let questions = sqlx::query!(
        "UPDATE questions SET text_redacted = '', answer_redacted = NULL \
         WHERE created_at < ? AND (text_redacted <> '' OR answer_redacted IS NOT NULL)",
        cutoff
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    // a held answer's draft is the answer it would have been, and goes with it: its text, the note
    // on it, and each flagged sentence (the check and its p stay); a reason that doesn't parse has
    // nothing left worth keeping past the window
    let approvals = sqlx::query!(
        "UPDATE approvals SET draft_answer = '', note = NULL, reason_json = CASE \
           WHEN json_valid(reason_json) THEN json_set(reason_json, '$.flags', json(( \
             SELECT json_group_array(json_set(value, '$.sentence', '')) \
             FROM json_each(reason_json, '$.flags')))) \
           ELSE '{}' END \
         WHERE question_id IN (SELECT id FROM questions WHERE created_at < ?) \
           AND (draft_answer <> '' OR note IS NOT NULL OR NOT json_valid(reason_json) \
                OR EXISTS (SELECT 1 FROM json_each( \
                             CASE WHEN json_valid(reason_json) THEN reason_json ELSE '{}' END, '$.flags') \
                           WHERE json_extract(value, '$.sentence') <> ''))",
        cutoff
    )
    .execute(&mut *tx)
    .await?
    .rows_affected();
    let retained = Retained {
        payloads,
        questions,
        approvals,
    };
    if payloads == 0 && questions == 0 && approvals == 0 {
        // nothing changed: no row, and the transaction ends without one
        tx.rollback().await?;
        return Ok(retained);
    }
    let payload = serde_json::json!({
        "before": cutoff,
        "days": days,
        "payloads": payloads,
        "questions": questions,
        "approvals": approvals,
    });
    action_in(&mut tx, "audit_retained", None, &payload, now).await?;
    tx.commit().await?;
    Ok(retained)
}

/// The first row that breaks the chain, and how.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Broken {
    pub seq: u64,
    pub what: Break,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Break {
    /// Rows before this one are missing: its `seq` doesn't follow the row before.
    Gap,
    /// Its `prev_hash` isn't the hash of the row before.
    PrevHash,
    /// Its payload, where retention left it, doesn't match its digest.
    Payload,
    /// Its hash doesn't recompute from its own columns.
    Hash,
    /// A column doesn't have the shape the writer gives it.
    Shape,
}

impl std::fmt::Display for Broken {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let what = match self.what {
            Break::Gap => "the rows before it are missing",
            Break::PrevHash => "its prev_hash is not the hash of the row before",
            Break::Payload => "its payload doesn't match its digest",
            Break::Hash => "its hash doesn't match its contents",
            Break::Shape => "a column doesn't have the shape the audit writes",
        };
        write!(f, "row {}: {what}", self.seq)
    }
}

/// What `audit verify` found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verified {
    pub rows: u64,
    pub broken: Option<Broken>,
}

/// Recomputes the whole chain in `seq` order and stops at the first row that breaks it.
pub async fn verify(db: &Db) -> Result<Verified, sqlx::Error> {
    let rows = sqlx::query!(
        "SELECT seq, type AS event_type, payload_json, payload_sha256, prev_hash, hash, created_at \
         FROM audit_events ORDER BY seq"
    )
    .fetch_all(db.pool())
    .await?;
    let mut expected_prev = GENESIS;
    let mut expected_seq = 1_u64;
    let mut count = 0_u64;
    for row in rows {
        count += 1;
        let seq = u64::try_from(row.seq).unwrap_or(0);
        let broken = |what| {
            Ok(Verified {
                rows: count,
                broken: Some(Broken { seq, what }),
            })
        };
        if seq != expected_seq {
            return broken(Break::Gap);
        }
        let (Some(prev), Some(stored), Some(payload_sha256)) = (
            hash_of(&row.prev_hash),
            hash_of(&row.hash),
            hash_of(&row.payload_sha256),
        ) else {
            return broken(Break::Shape);
        };
        if prev != expected_prev {
            return broken(Break::PrevHash);
        }
        if let Some(payload) = &row.payload_json
            && digest(payload.as_bytes()) != payload_sha256
        {
            return broken(Break::Payload);
        }
        if chain_hash(&prev, seq, &row.event_type, &payload_sha256, &row.created_at) != stored {
            return broken(Break::Hash);
        }
        expected_prev = stored;
        expected_seq = seq + 1;
    }
    Ok(Verified {
        rows: count,
        broken: None,
    })
}

#[cfg(test)]
mod tests {
    use mercury_schema::api::Accepted;
    use serde_json::json;
    use sqlx::sqlite::{SqliteConnectOptions, SqlitePoolOptions};

    use super::*;
    use crate::db::testing;

    fn now() -> DateTime<Utc> {
        db::parse_time("2026-10-02T10:00:00.000Z").unwrap()
    }

    fn question() -> QuestionId {
        QuestionId::parse("q_01JB2M3N4P5Q6R7S8T9V0W1X2Y").unwrap()
    }

    async fn three_rows(db: &Db) {
        for n in 1..=3 {
            action(
                db,
                "token_revoked",
                None,
                &json!({ "public_id": format!("publicid000{n}") }),
                now(),
            )
            .await
            .unwrap();
        }
    }

    async fn sql(db: &Db, statement: &'static str) {
        sqlx::query(statement).execute(db.pool()).await.unwrap();
    }

    #[tokio::test]
    async fn an_exported_row_re_digests_byte_for_byte() {
        let (_dir, db) = testing::db().await;
        // a `done` on a whole second, which a time written back out would lose the milliseconds of
        let done = json!({ "duration_ms": 8000, "cost_usd": 0.0001, "judge_mode": "chat",
                           "composer": "template", "audit_id": 1 });
        let event_type = EventType::parse("done").unwrap();
        let entry = Entry {
            question: Some(&question()),
            project: None,
            event_type: &event_type,
            payload: &done,
        };
        append(&db, entry, now()).await.unwrap();
        // a payload stored in an order of its own: the file must keep its bytes, not sort its keys
        let stored = r#"{"project":"delta","p":1.0,"source":"explicit"}"#;
        let payload_sha256 = digest(stored.as_bytes());
        let prev = hash_of(
            &sqlx::query_scalar::<_, Vec<u8>>("SELECT hash FROM audit_events WHERE seq = 1")
                .fetch_one(db.pool())
                .await
                .unwrap(),
        )
        .unwrap();
        let created_at = "2026-10-02T10:00:01.000Z";
        let hash = chain_hash(&prev, 2, "project", &payload_sha256, created_at);
        sqlx::query(
            "INSERT INTO audit_events (question_id, type, payload_json, payload_sha256, prev_hash, hash, created_at) \
             VALUES (?, 'project', ?, ?, ?, ?, ?)",
        )
        .bind(question().as_str())
        .bind(stored)
        .bind(payload_sha256.to_vec())
        .bind(prev.to_vec())
        .bind(hash.to_vec())
        .bind(created_at)
        .execute(db.pool())
        .await
        .unwrap();
        assert_eq!(verify(&db).await.unwrap().broken, None);

        let mut out = Vec::new();
        assert_eq!(export_to(&db, &Filter::default(), 2, &mut out).await.unwrap(), 2);
        let file = String::from_utf8(out).unwrap();
        let mut prev = "0".repeat(64);
        for line in file.lines() {
            let row: Value = serde_json::from_str(line).unwrap();
            // the file alone: the payload's bytes between its key and the next, and `at` as written
            let start = line.find(r#""payload":"#).unwrap() + r#""payload":"#.len();
            let end = line.find(r#","payload_sha256":"#).unwrap();
            let raw = &line[start..end];
            let field = |name: &str| -> [u8; 32] {
                let text = row[name].as_str().unwrap();
                let bytes: Vec<u8> = (0..64)
                    .step_by(2)
                    .map(|i| u8::from_str_radix(&text[i..i + 2], 16).unwrap())
                    .collect();
                bytes.try_into().unwrap()
            };
            assert_eq!(digest(raw.as_bytes()), field("payload_sha256"), "{line}");
            let recomputed = chain_hash(
                &field("prev_hash"),
                row["seq"].as_u64().unwrap(),
                row["type"].as_str().unwrap(),
                &field("payload_sha256"),
                row["at"].as_str().unwrap(),
            );
            assert_eq!(recomputed, field("hash"), "{line}");
            assert_eq!(row["prev_hash"], prev.as_str());
            prev = row["hash"].as_str().unwrap().to_owned();
        }
        assert!(file.contains(r#""at":"2026-10-02T10:00:00.000Z""#), "{file}");
        assert!(file.contains(stored), "{file}");
    }

    #[tokio::test]
    async fn editing_one_row_is_found_and_named() {
        for (tamper, row, what) in [
            (
                "UPDATE audit_events SET payload_json = '{\"public_id\":\"publicid0009\"}' WHERE seq = 2",
                2,
                Break::Payload,
            ),
            (
                "UPDATE audit_events SET type = 'token_created' WHERE seq = 2",
                2,
                Break::Hash,
            ),
            (
                "UPDATE audit_events SET created_at = '2026-10-01T10:00:00.000Z' WHERE seq = 2",
                2,
                Break::Hash,
            ),
            (
                "UPDATE audit_events SET hash = zeroblob(32) WHERE seq = 2",
                2,
                Break::Hash,
            ),
            ("DELETE FROM audit_events WHERE seq = 2", 3, Break::Gap),
            ("DELETE FROM audit_events WHERE seq = 1", 2, Break::Gap),
        ] {
            let (_dir, db) = testing::db().await;
            three_rows(&db).await;
            assert_eq!(verify(&db).await.unwrap().broken, None, "untouched");
            sql(&db, tamper).await;
            let found = verify(&db).await.unwrap().broken;
            assert_eq!(found, Some(Broken { seq: row, what }), "{tamper}");
        }
    }

    #[tokio::test]
    async fn retention_dropping_payloads_keeps_the_chain() {
        let (_dir, db) = testing::db().await;
        three_rows(&db).await;
        sql(&db, "UPDATE audit_events SET payload_json = NULL WHERE seq < 3").await;
        assert_eq!(
            verify(&db).await.unwrap(),
            Verified {
                rows: 3,
                broken: None
            }
        );
    }

    #[tokio::test]
    async fn a_stream_event_stores_its_data() {
        let (_dir, db) = testing::db().await;
        let q = question();
        let event = Event::Accepted(Accepted::new(q.clone()));
        let recorded = record(&db, &q, None, event.clone(), now()).await.unwrap();
        assert_eq!(recorded.seq(), 1);
        assert_eq!(recorded.item(), &event);
        let (event_type, payload): (String, String) =
            sqlx::query_as("SELECT type, payload_json FROM audit_events WHERE seq = 1")
                .fetch_one(db.pool())
                .await
                .unwrap();
        assert_eq!(event_type, "accepted");
        let rebuilt: Event = serde_json::from_value(
            json!({ "event": event_type, "data": serde_json::from_str::<Value>(&payload).unwrap() }),
        )
        .unwrap();
        assert_eq!(rebuilt, event, "a replay rebuilds the event from its row");
    }

    #[test]
    fn the_hash_is_the_documented_bytes() {
        let mut expected = Vec::new();
        expected.extend_from_slice(&GENESIS);
        expected.extend_from_slice(&7_u64.to_be_bytes());
        expected.push(8);
        expected.extend_from_slice(b"accepted");
        expected.extend_from_slice(&[0xab; 32]);
        expected.extend_from_slice(b"2026-10-02T10:00:00.000Z");
        let hash = chain_hash(&GENESIS, 7, "accepted", &[0xab; 32], "2026-10-02T10:00:00.000Z");
        assert_eq!(hash, digest(&expected));
    }

    /// A pool on the same file with one connection, read-only or with a setting applied to it.
    async fn limited(path: &std::path::Path, read_only: bool, pragma: Option<String>) -> Db {
        let options = SqliteConnectOptions::new().filename(path).read_only(read_only);
        let pool = SqlitePoolOptions::new()
            .max_connections(1)
            .after_connect(move |conn, _| {
                let pragma = pragma.clone();
                Box::pin(async move {
                    if let Some(pragma) = pragma {
                        sqlx::query(sqlx::AssertSqlSafe(pragma)).execute(conn).await?;
                    }
                    Ok(())
                })
            })
            .connect_with(options)
            .await
            .unwrap();
        testing::from_pool(pool)
    }

    /// What a stream would send: only what an append recorded.
    fn sent<T>(result: Result<Recorded<T>, AuditError>) -> Vec<u64> {
        result.map(|recorded| recorded.seq()).into_iter().collect()
    }

    #[tokio::test]
    async fn a_failed_append_sends_nothing() {
        let (dir, db) = testing::db().await;
        three_rows(&db).await;
        let path = dir.path().join("mercury.db");
        let q = question();
        let event = Event::Accepted(Accepted::new(q.clone()));
        let read_only = limited(&path, true, None).await;
        assert_eq!(
            sent(record(&read_only, &q, None, event, now()).await),
            Vec::<u64>::new()
        );
        // a full disk: no page may be added, and a 64 KB payload needs several
        let pages: i64 = sqlx::query_scalar("PRAGMA page_count")
            .fetch_one(db.pool())
            .await
            .unwrap();
        let full = limited(&path, false, Some(format!("PRAGMA max_page_count = {pages}"))).await;
        let big = json!({ "filler": "x".repeat(64 * 1024) });
        assert!(
            action(&full, "token_revoked", None, &big, now()).await.is_err(),
            "a full disk refuses the row"
        );
        assert_eq!(
            verify(&db).await.unwrap(),
            Verified {
                rows: 3,
                broken: None
            },
            "nothing half-written"
        );
    }

    #[tokio::test]
    async fn concurrent_appends_keep_one_chain() {
        let (dir, db) = testing::db().await;
        // a second pool on the same file stands for another process, an admin command
        let other = Db::open(&dir.path().join("mercury.db")).await.unwrap();
        let mut tasks = Vec::new();
        for n in 0..40 {
            let db = if n % 2 == 0 { db.clone() } else { other.clone() };
            tasks.push(tokio::spawn(async move {
                action(&db, "token_revoked", None, &json!({ "n": n }), now()).await
            }));
        }
        let mut seqs = Vec::new();
        for task in tasks {
            seqs.push(task.await.unwrap().unwrap());
        }
        seqs.sort_unstable();
        assert_eq!(seqs, (1..=40).collect::<Vec<u64>>());
        assert_eq!(
            verify(&db).await.unwrap(),
            Verified {
                rows: 40,
                broken: None
            }
        );
    }
}
