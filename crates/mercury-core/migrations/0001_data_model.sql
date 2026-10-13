-- The core's whole data model. Times are RFC 3339 UTC text with milliseconds, so text order is
-- time order; booleans are 0 or 1; every table is STRICT and foreign keys are enforced.

CREATE TABLE users (
    id           TEXT NOT NULL PRIMARY KEY,
    email        TEXT NOT NULL COLLATE NOCASE UNIQUE,
    display_name TEXT NOT NULL,
    role         TEXT NOT NULL CHECK (role IN ('pm', 'tech_lead', 'board', 'admin')),
    disabled_at  TEXT,
    created_at   TEXT NOT NULL
) STRICT;

-- `project` is a project id or `portfolio`.
CREATE TABLE user_projects (
    user_id  TEXT NOT NULL REFERENCES users (id),
    project  TEXT NOT NULL,
    approver INTEGER NOT NULL DEFAULT 0 CHECK (approver IN (0, 1)),
    PRIMARY KEY (user_id, project)
) STRICT;

-- A token is `mcy_<public_id>_<secret>`; only the SHA-256 of the secret is kept.
CREATE TABLE tokens (
    public_id     TEXT NOT NULL PRIMARY KEY,
    user_id       TEXT NOT NULL REFERENCES users (id),
    secret_sha256 BLOB NOT NULL CHECK (length(secret_sha256) = 32),
    label         TEXT NOT NULL,
    created_at    TEXT NOT NULL,
    expires_at    TEXT NOT NULL,
    revoked_at    TEXT,
    last_used_at  TEXT
) STRICT;
CREATE INDEX tokens_user_id ON tokens (user_id);

-- A browser session; only the SHA-256 of its id is kept.
CREATE TABLE sessions (
    id_sha256       BLOB NOT NULL PRIMARY KEY CHECK (length(id_sha256) = 32),
    token_public_id TEXT NOT NULL REFERENCES tokens (public_id),
    user_id         TEXT NOT NULL REFERENCES users (id),
    created_at      TEXT NOT NULL,
    expires_at      TEXT NOT NULL,
    last_seen_at    TEXT,
    revoked_at      TEXT
) STRICT;
CREATE INDEX sessions_token_public_id ON sessions (token_public_id);

CREATE TABLE probes (
    project           TEXT NOT NULL PRIMARY KEY,
    url               TEXT NOT NULL,
    access            TEXT NOT NULL,
    access_limited_by TEXT,
    status            TEXT NOT NULL,
    schema            TEXT NOT NULL,
    manifest_hash     TEXT NOT NULL,
    manifest_json     TEXT NOT NULL,
    tools_json        TEXT NOT NULL,
    quarantine_reason TEXT,
    registered_at     TEXT NOT NULL,
    last_seen_at      TEXT NOT NULL
) STRICT;

CREATE TABLE questions (
    id              TEXT NOT NULL PRIMARY KEY,
    user_id         TEXT NOT NULL REFERENCES users (id),
    token_public_id TEXT NOT NULL REFERENCES tokens (public_id),
    channel         TEXT NOT NULL,
    project         TEXT,
    class           TEXT,
    status          TEXT NOT NULL,
    clarify_of      TEXT REFERENCES questions (id),
    text_redacted   TEXT NOT NULL,
    answer_redacted TEXT,
    judge_mode      TEXT,
    prompt_version  TEXT,
    cost_usd        REAL NOT NULL DEFAULT 0.0,
    duration_ms     INTEGER,
    created_at      TEXT NOT NULL,
    finished_at     TEXT
) STRICT;
CREATE INDEX questions_project_created_at ON questions (project, created_at);
CREATE INDEX questions_user_id_created_at ON questions (user_id, created_at);

-- The hash chain covers the payload's digest, not the payload, so retention can drop payloads.
-- `question_id` is empty for probe and admin events.
CREATE TABLE audit_events (
    seq            INTEGER PRIMARY KEY AUTOINCREMENT,
    question_id    TEXT,
    project        TEXT,
    type           TEXT NOT NULL,
    payload_json   TEXT,
    payload_sha256 BLOB NOT NULL,
    prev_hash      BLOB NOT NULL,
    hash           BLOB NOT NULL,
    created_at     TEXT NOT NULL
) STRICT;
CREATE INDEX audit_events_question_id ON audit_events (question_id);
CREATE INDEX audit_events_project_created_at ON audit_events (project, created_at);

CREATE TABLE approvals (
    id           TEXT NOT NULL PRIMARY KEY,
    question_id  TEXT NOT NULL REFERENCES questions (id),
    reason_json  TEXT NOT NULL,
    draft_answer TEXT NOT NULL,
    status       TEXT NOT NULL,
    decided_by   TEXT REFERENCES users (id),
    note         TEXT,
    created_at   TEXT NOT NULL,
    decided_at   TEXT,
    expires_at   TEXT NOT NULL
) STRICT;
CREATE INDEX approvals_status_expires_at ON approvals (status, expires_at);

CREATE TABLE feedback (
    question_id TEXT NOT NULL PRIMARY KEY REFERENCES questions (id),
    helpful     INTEGER NOT NULL CHECK (helpful IN (0, 1)),
    needed_dev  INTEGER NOT NULL CHECK (needed_dev IN (0, 1)),
    created_at  TEXT NOT NULL
) STRICT;

-- Deleted after 24 hours.
CREATE TABLE idempotency_keys (
    key         TEXT NOT NULL,
    user_id     TEXT NOT NULL,
    question_id TEXT NOT NULL,
    created_at  TEXT NOT NULL,
    PRIMARY KEY (user_id, key)
) STRICT;

-- One row per user per day, plus the row `*` for everyone.
CREATE TABLE spend (
    day       TEXT NOT NULL,
    user_id   TEXT NOT NULL,
    spent_usd REAL NOT NULL DEFAULT 0.0,
    PRIMARY KEY (day, user_id)
) STRICT;
