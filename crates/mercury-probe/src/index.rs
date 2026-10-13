//! The index: SQLite, facts only, no code and no free text. `serve` keeps it at `/data/index.db`, `dev`
//! in memory.
//!
//! A rebuild replaces every table but `todo_counts` in one transaction, so a reader sees the old
//! index or the new one, never half of each. Reading turns the rows back into `mercury-schema`
//! types, which check every value again on the way out.

use std::path::Path;
use std::str::FromStr;

use chrono::{DateTime, Duration, NaiveDate, Utc};
use mercury_schema::facts::{
    BuildStatus, CiRun, Evidence, HealthReply, LastRelease, ModuleStats, OpenPrs, Ratio, SchemaId, Score,
    Snapshot, TodoFixme,
};
use mercury_schema::{AccessLimit, AccessMode, CommitShort, Fact, FactId, ModuleName, SafeText};
use serde::Serialize;
use serde::de::DeserializeOwned;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool, SqlitePoolOptions, SqliteRow};
use sqlx::{Row, Sqlite, Transaction};

use crate::facts::{Item, ModuleRow, Rows, RunRow};
use crate::meta::RunOutcome;

const SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS items (
    id TEXT PRIMARY KEY,
    position INTEGER NOT NULL,
    kind TEXT NOT NULL,
    title TEXT NOT NULL,
    title_withheld INTEGER NOT NULL,
    module TEXT NOT NULL,
    status TEXT NOT NULL,
    tested TEXT NOT NULL,
    confidence REAL NOT NULL,
    waiting_on TEXT,
    updated_at TEXT NOT NULL,
    open_pr_since TEXT
);
CREATE TABLE IF NOT EXISTS item_evidence (
    item_id TEXT NOT NULL,
    position INTEGER NOT NULL,
    type TEXT NOT NULL,
    ref INTEGER,
    state TEXT NOT NULL,
    at TEXT NOT NULL,
    PRIMARY KEY (item_id, position)
);
CREATE TABLE IF NOT EXISTS module_stats (
    module TEXT PRIMARY KEY,
    position INTEGER NOT NULL,
    tests_total INTEGER,
    tests_changed_14d INTEGER,
    todo_fixme INTEGER,
    commits_14d INTEGER NOT NULL,
    last_touched TEXT
);
CREATE TABLE IF NOT EXISTS ci_runs (
    id INTEGER PRIMARY KEY,
    head_sha TEXT NOT NULL,
    outcome TEXT NOT NULL,
    attempt INTEGER NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS changes (
    at TEXT NOT NULL,
    module TEXT NOT NULL,
    commits INTEGER NOT NULL,
    merged_pr INTEGER NOT NULL,
    item_id TEXT
);
CREATE INDEX IF NOT EXISTS changes_at ON changes (at);
CREATE TABLE IF NOT EXISTS todo_counts (
    meta_at TEXT PRIMARY KEY,
    count INTEGER NOT NULL
);
CREATE TABLE IF NOT EXISTS snapshot (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    commit_short TEXT,
    indexed_at TEXT NOT NULL,
    meta_at TEXT NOT NULL,
    access TEXT NOT NULL,
    access_limited_by TEXT,
    release_name TEXT,
    release_at TEXT
);
";

#[derive(Debug, thiserror::Error)]
pub enum IndexError {
    #[error("the index database failed: {0}")]
    Db(#[from] sqlx::Error),
    #[error("the index has not been built yet")]
    Empty,
    #[error("an index row doesn't fit the facts schema ({0})")]
    Row(&'static str),
}

#[derive(Debug, Clone)]
pub struct Index {
    pool: SqlitePool,
}

impl Index {
    /// An index in memory, for `dev` and the tests.
    pub async fn in_memory() -> Result<Self, IndexError> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?;
        // one connection that never closes: an in-memory database lives as long as it does
        let pool = SqlitePoolOptions::new()
            .min_connections(1)
            .max_connections(1)
            .idle_timeout(None)
            .max_lifetime(None)
            .connect_with(options)
            .await?;
        Self::init(pool).await
    }

    /// An index in a file, created when missing (`/data/index.db` in `serve`).
    pub async fn open(path: &Path) -> Result<Self, IndexError> {
        let options = SqliteConnectOptions::new().filename(path).create_if_missing(true);
        let pool = SqlitePoolOptions::new()
            .max_connections(4)
            .connect_with(options)
            .await?;
        Self::init(pool).await
    }

    async fn init(pool: SqlitePool) -> Result<Self, IndexError> {
        sqlx::raw_sql(SCHEMA_SQL).execute(&pool).await?;
        Ok(Self { pool })
    }

    /// Replaces the index with these rows, and records the TODO/FIXME count for the trend.
    pub async fn replace(&self, rows: &Rows) -> Result<(), IndexError> {
        let mut tx = self.pool.begin().await?;
        for clear in [
            "DELETE FROM items",
            "DELETE FROM item_evidence",
            "DELETE FROM module_stats",
            "DELETE FROM ci_runs",
            "DELETE FROM changes",
            "DELETE FROM snapshot",
        ] {
            sqlx::query(clear).execute(&mut *tx).await?;
        }
        for (position, item) in rows.items.iter().enumerate() {
            insert_item(&mut tx, position, item).await?;
        }
        for (position, module) in rows.modules.iter().enumerate() {
            insert_module(&mut tx, position, module).await?;
        }
        for run in &rows.runs {
            insert_run(&mut tx, run).await?;
        }
        for change in &rows.changes {
            sqlx::query(
                "INSERT INTO changes (at, module, commits, merged_pr, item_id) VALUES (?, ?, ?, ?, ?)",
            )
            .bind(time(change.at))
            .bind(change.module.as_str())
            .bind(i64::from(change.commits))
            .bind(change.merged_pr)
            .bind(change.item.as_ref().map(FactId::as_str))
            .execute(&mut *tx)
            .await?;
        }
        let s = &rows.snapshot;
        sqlx::query(
            "INSERT INTO snapshot (id, commit_short, indexed_at, meta_at, access, access_limited_by, release_name, release_at)
             VALUES (1, ?, ?, ?, ?, ?, ?, ?)",
        )
        .bind(&s.commit_short)
        .bind(time(s.indexed_at))
        .bind(time(s.meta_at))
        .bind(text(&s.access))
        .bind(s.access_limited_by.as_ref().map(text))
        .bind(s.release_name.as_ref().map(SafeText::as_str))
        .bind(s.release_at.map(time))
        .execute(&mut *tx)
        .await?;
        match rows.todo_total() {
            Some(count) => {
                sqlx::query("INSERT OR REPLACE INTO todo_counts (meta_at, count) VALUES (?, ?)")
                    .bind(time(s.meta_at))
                    .bind(i64::from(count))
                    .execute(&mut *tx)
                    .await?;
            }
            // no counts in metadata mode, so no trend either
            None => {
                sqlx::query("DELETE FROM todo_counts").execute(&mut *tx).await?;
            }
        }
        tx.commit().await?;
        Ok(())
    }

    /// Whether the index holds counts read from a snapshot's files: a code mode's index, built
    /// with the code and not from the metadata alone.
    pub async fn has_code_counts(&self) -> Result<bool, IndexError> {
        let found = sqlx::query("SELECT 1 FROM module_stats WHERE tests_total IS NOT NULL LIMIT 1")
            .fetch_optional(&self.pool)
            .await?;
        Ok(found.is_some())
    }

    /// The snapshot the index was built from, with its mode.
    pub async fn snapshot(&self) -> Result<IndexedSnapshot, IndexError> {
        let row = sqlx::query(
            "SELECT commit_short, indexed_at, meta_at, access, access_limited_by, release_name, release_at
             FROM snapshot WHERE id = 1",
        )
        .fetch_optional(&self.pool)
        .await?
        .ok_or(IndexError::Empty)?;
        let commit_short: Option<String> = row.try_get("commit_short")?;
        Ok(IndexedSnapshot {
            snapshot: Snapshot {
                commit_short: commit_short
                    .map(|c| CommitShort::parse(&c))
                    .transpose()
                    .map_err(|_| IndexError::Row("commit_short"))?,
                indexed_at: get_time(&row, "indexed_at")?,
                meta_at: get_time(&row, "meta_at")?,
            },
            access: get_enum(&row, "access")?,
            access_limited_by: get_opt_enum(&row, "access_limited_by")?,
            last_release: LastRelease {
                name: get_opt_json(&row, "release_name")?,
                at: get_opt_time(&row, "release_at")?,
            },
        })
    }

    /// Every fact, the most recently updated first.
    pub async fn facts(&self) -> Result<Vec<Fact>, IndexError> {
        let evidence_rows =
            sqlx::query("SELECT item_id, type, ref, state, at FROM item_evidence ORDER BY item_id, position")
                .fetch_all(&self.pool)
                .await?;
        let mut evidence: std::collections::BTreeMap<String, Vec<Evidence>> =
            std::collections::BTreeMap::new();
        for row in &evidence_rows {
            let reference: Option<i64> = row.try_get("ref")?;
            let value = serde_json::json!({
                "type": row.try_get::<String, _>("type")?,
                "ref": reference.map(|n| format!("#{n}")),
                "state": row.try_get::<String, _>("state")?,
                "at": row.try_get::<String, _>("at")?,
            });
            let one: Evidence = serde_json::from_value(value).map_err(|_| IndexError::Row("evidence"))?;
            evidence.entry(row.try_get("item_id")?).or_default().push(one);
        }
        let rows = sqlx::query(
            "SELECT id, kind, title, title_withheld, module, status, tested, confidence, waiting_on, updated_at
             FROM items ORDER BY position",
        )
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                let id: String = row.try_get("id")?;
                let value = serde_json::json!({
                    "id": id,
                    "kind": row.try_get::<String, _>("kind")?,
                    "title": row.try_get::<String, _>("title")?,
                    "module": row.try_get::<String, _>("module")?,
                    "status": row.try_get::<String, _>("status")?,
                    "tested": row.try_get::<String, _>("tested")?,
                    "confidence": row.try_get::<f64, _>("confidence")?,
                    "waiting_on": row.try_get::<Option<String>, _>("waiting_on")?,
                    "people": [],
                    "evidence": evidence.get(&id).cloned().unwrap_or_default(),
                    "title_withheld": row.try_get::<bool, _>("title_withheld")?,
                    "updated_at": row.try_get::<String, _>("updated_at")?,
                });
                serde_json::from_value(value).map_err(|_| IndexError::Row("item"))
            })
            .collect()
    }

    /// Every module's tests and CI. CI runs on the default branch, so every module shares it.
    pub async fn module_stats(&self) -> Result<Vec<ModuleStats>, IndexError> {
        let ci = self.ci_summary().await?;
        let rows =
            sqlx::query("SELECT module, tests_total, tests_changed_14d FROM module_stats ORDER BY position")
                .fetch_all(&self.pool)
                .await?;
        rows.iter()
            .map(|row| {
                Ok(ModuleStats {
                    module: ModuleName::parse(&row.try_get::<String, _>("module")?)
                        .map_err(|_| IndexError::Row("module"))?,
                    tests_total: get_opt_count(row, "tests_total")?,
                    tests_changed_14d: get_opt_count(row, "tests_changed_14d")?,
                    last_ci: ci.last.clone(),
                    pass_rate_7d: ci.pass_rate_7d,
                    flaky_7d: ci.flaky_7d,
                })
            })
            .collect()
    }

    /// Commits in the last 14 days and the last-touched time, per module.
    pub async fn change_heat(&self) -> Result<Vec<(ModuleName, u32, Option<DateTime<Utc>>)>, IndexError> {
        let rows =
            sqlx::query("SELECT module, commits_14d, last_touched FROM module_stats ORDER BY position")
                .fetch_all(&self.pool)
                .await?;
        rows.iter()
            .map(|row| {
                Ok((
                    ModuleName::parse(&row.try_get::<String, _>("module")?)
                        .map_err(|_| IndexError::Row("module"))?,
                    get_count(row, "commits_14d")?,
                    get_opt_time(row, "last_touched")?,
                ))
            })
            .collect()
    }

    /// The commits and merged pull requests of each module from `since` on, for the modules that
    /// have any.
    pub async fn changes_since(
        &self,
        since: DateTime<Utc>,
    ) -> Result<Vec<(ModuleName, u32, u32)>, IndexError> {
        let rows = sqlx::query(
            "SELECT module, SUM(commits) AS commits, SUM(merged_pr) AS merged_prs FROM changes
             WHERE at >= ? GROUP BY module",
        )
        .bind(time(since))
        .fetch_all(&self.pool)
        .await?;
        rows.iter()
            .map(|row| {
                Ok((
                    ModuleName::parse(&row.try_get::<String, _>("module")?)
                        .map_err(|_| IndexError::Row("module"))?,
                    get_count(row, "commits")?,
                    get_count(row, "merged_prs")?,
                ))
            })
            .collect()
    }

    /// The titles of a module's pull requests merged from `since` on, the most recent first, at
    /// most `limit`; a withheld title is left out.
    pub async fn notable_since(
        &self,
        since: DateTime<Utc>,
        module: &ModuleName,
        limit: u32,
    ) -> Result<Vec<SafeText>, IndexError> {
        let titles: Vec<String> = sqlx::query_scalar(
            "SELECT items.title FROM changes JOIN items ON items.id = changes.item_id
             WHERE changes.merged_pr = 1 AND changes.at >= ? AND changes.module = ?
               AND items.title_withheld = 0
             ORDER BY changes.at DESC, items.id LIMIT ?",
        )
        .bind(time(since))
        .bind(module.as_str())
        .bind(i64::from(limit))
        .fetch_all(&self.pool)
        .await?;
        // read back through SafeText, which checks each title again
        titles
            .into_iter()
            .map(|title| {
                serde_json::from_value(serde_json::Value::String(title)).map_err(|_| IndexError::Row("title"))
            })
            .collect()
    }

    /// The `health` reply, with the score v1.
    pub async fn health(&self) -> Result<HealthReply, IndexError> {
        let snapshot = self.snapshot().await?;
        let meta_at = snapshot.snapshot.meta_at;
        let ci = self.ci_summary().await?;

        let open: Vec<Option<String>> =
            sqlx::query_scalar("SELECT open_pr_since FROM items WHERE open_pr_since IS NOT NULL")
                .fetch_all(&self.pool)
                .await?;
        let mut oldest_days = 0u32;
        for since in open.iter().flatten() {
            let since = parse_time(since)?;
            let days = u32::try_from((meta_at - since).num_days().max(0)).unwrap_or(u32::MAX);
            oldest_days = oldest_days.max(days);
        }
        let open_prs = OpenPrs {
            count: u32::try_from(open.len()).unwrap_or(u32::MAX),
            oldest_days,
        };

        let todo_fixme = match snapshot.access {
            AccessMode::Metadata => None,
            _ => {
                let count: Option<i64> = sqlx::query_scalar("SELECT SUM(todo_fixme) FROM module_stats")
                    .fetch_one(&self.pool)
                    .await?;
                let count = u32::try_from(count.unwrap_or(0)).map_err(|_| IndexError::Row("todo_fixme"))?;
                let since = time(meta_at - Duration::days(14));
                let oldest: Option<i64> = sqlx::query_scalar(
                    "SELECT count FROM todo_counts WHERE meta_at >= ? ORDER BY meta_at LIMIT 1",
                )
                .bind(since)
                .fetch_optional(&self.pool)
                .await?;
                let then = oldest.unwrap_or(i64::from(count));
                let trend =
                    i32::try_from(i64::from(count) - then).map_err(|_| IndexError::Row("todo_counts"))?;
                Some(TodoFixme {
                    count,
                    trend_14d: trend,
                })
            }
        };

        let score = score(
            ci.pass_rate_7d.get(),
            ci.last.status == BuildStatus::Passed,
            oldest_days,
            ci.flaky_7d.get(),
            todo_fixme.as_ref().map(|t| t.trend_14d <= 0),
        )
        .ok_or(IndexError::Row("score"))?;
        Ok(HealthReply {
            schema: SchemaId,
            as_of: meta_at,
            build: ci.last,
            ci_pass_rate_7d: ci.pass_rate_7d,
            open_prs,
            todo_fixme,
            last_release: snapshot.last_release,
            score,
        })
    }

    async fn ci_summary(&self) -> Result<CiSummary, IndexError> {
        let meta_at: String = sqlx::query_scalar("SELECT meta_at FROM snapshot WHERE id = 1")
            .fetch_optional(&self.pool)
            .await?
            .ok_or(IndexError::Empty)?;
        let since = time(parse_time(&meta_at)? - Duration::days(7));
        let last = sqlx::query(
            "SELECT outcome, updated_at FROM ci_runs ORDER BY created_at DESC, attempt DESC LIMIT 1",
        )
        .fetch_optional(&self.pool)
        .await?;
        let last = match last {
            None => CiRun {
                status: BuildStatus::None,
                at: None,
            },
            Some(row) => CiRun {
                status: match row.try_get::<String, _>("outcome")?.as_str() {
                    "passed" => BuildStatus::Passed,
                    "failed" => BuildStatus::Failed,
                    _ => BuildStatus::Running,
                },
                at: Some(get_time(&row, "updated_at")?),
            },
        };
        let week = sqlx::query("SELECT outcome, attempt FROM ci_runs WHERE created_at >= ?")
            .bind(since)
            .fetch_all(&self.pool)
            .await?;
        let mut passed = 0u32;
        let mut flaky = 0u32;
        let mut total = 0u32;
        for row in &week {
            let outcome: String = row.try_get("outcome")?;
            let attempt: i64 = row.try_get("attempt")?;
            total += 1;
            if outcome == "passed" {
                passed += 1;
                if attempt > 1 {
                    flaky += 1;
                }
            }
        }
        let share = |n: u32| {
            if total == 0 {
                0.0
            } else {
                f64::from(n) / f64::from(total)
            }
        };
        Ok(CiSummary {
            last,
            pass_rate_7d: Ratio::new(share(passed)).ok_or(IndexError::Row("pass_rate_7d"))?,
            flaky_7d: Ratio::new(share(flaky)).ok_or(IndexError::Row("flaky_7d"))?,
        })
    }
}

/// The `snapshot` row as the replies use it.
#[derive(Debug, Clone, PartialEq)]
pub struct IndexedSnapshot {
    pub snapshot: Snapshot,
    pub access: AccessMode,
    pub access_limited_by: Option<AccessLimit>,
    pub last_release: LastRelease,
}

struct CiSummary {
    last: CiRun,
    pass_rate_7d: Ratio,
    flaky_7d: Ratio,
}

/// The health score v1. `todo_not_rising` is `None` in `metadata` mode, which leaves the term out
/// and scales the others up to 100.
pub fn score(
    pass_rate_7d: f64,
    build_green: bool,
    oldest_pr_days: u32,
    flaky_7d: f64,
    todo_not_rising: Option<bool>,
) -> Option<Score> {
    let age = f64::from(oldest_pr_days.min(30)) / 30.0;
    let mut points = 40.0 * pass_rate_7d + 15.0 * (1.0 - age) + 15.0 * (1.0 - flaky_7d);
    if build_green {
        points += 20.0;
    }
    let points = match todo_not_rising {
        Some(true) => points + 10.0,
        Some(false) => points,
        None => points * 100.0 / 90.0,
    };
    // clamped to 0..=100 first, so the cast can't truncate
    #[allow(
        clippy::cast_possible_truncation,
        clippy::cast_sign_loss,
        reason = "clamped to 0..=100"
    )]
    let rounded = points.round().clamp(0.0, 100.0) as u8;
    Score::new(rounded)
}

async fn insert_item(
    tx: &mut Transaction<'_, Sqlite>,
    position: usize,
    item: &Item,
) -> Result<(), IndexError> {
    sqlx::query(
        "INSERT INTO items (id, position, kind, title, title_withheld, module, status, tested, confidence, waiting_on, updated_at, open_pr_since)
         VALUES (?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(item.id.as_str())
    .bind(i64::try_from(position).unwrap_or(i64::MAX))
    .bind(text(&item.kind))
    .bind(item.title.as_str())
    .bind(item.title_withheld)
    .bind(item.module.as_str())
    .bind(text(&item.status))
    .bind(text(&item.tested))
    .bind(item.confidence)
    .bind(item.waiting_on.as_ref().map(text))
    .bind(time(item.updated_at))
    .bind(item.open_pr_since.map(time))
    .execute(&mut **tx)
    .await?;
    for (position, e) in item.evidence.iter().enumerate() {
        sqlx::query(
            "INSERT INTO item_evidence (item_id, position, type, ref, state, at) VALUES (?, ?, ?, ?, ?, ?)",
        )
        .bind(item.id.as_str())
        .bind(i64::try_from(position).unwrap_or(i64::MAX))
        .bind(text(&e.kind))
        .bind(e.reference.map(|n| i64::from(n.get())))
        .bind(text(&e.state))
        .bind(date(e.at))
        .execute(&mut **tx)
        .await?;
    }
    Ok(())
}

async fn insert_module(
    tx: &mut Transaction<'_, Sqlite>,
    position: usize,
    m: &ModuleRow,
) -> Result<(), IndexError> {
    sqlx::query(
        "INSERT INTO module_stats (module, position, tests_total, tests_changed_14d, todo_fixme, commits_14d, last_touched)
         VALUES (?, ?, ?, ?, ?, ?, ?)",
    )
    .bind(m.module.as_str())
    .bind(i64::try_from(position).unwrap_or(i64::MAX))
    .bind(m.tests_total.map(i64::from))
    .bind(m.tests_changed_14d.map(i64::from))
    .bind(m.todo_fixme.map(i64::from))
    .bind(i64::from(m.commits_14d))
    .bind(m.last_touched.map(time))
    .execute(&mut **tx)
    .await?;
    Ok(())
}

async fn insert_run(tx: &mut Transaction<'_, Sqlite>, run: &RunRow) -> Result<(), IndexError> {
    sqlx::query("INSERT OR REPLACE INTO ci_runs (id, head_sha, outcome, attempt, created_at, updated_at) VALUES (?, ?, ?, ?, ?, ?)")
        .bind(i64::try_from(run.id).unwrap_or(i64::MAX))
        .bind(&run.head_sha)
        .bind(match run.outcome {
            RunOutcome::Passed => "passed",
            RunOutcome::Failed => "failed",
            RunOutcome::Running => "running",
        })
        .bind(i64::from(run.attempt))
        .bind(time(run.created_at))
        .bind(time(run.updated_at))
        .execute(&mut **tx)
        .await?;
    Ok(())
}

/// An enum as its wire name.
fn text<T: Serialize>(value: &T) -> String {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(s)) => s,
        _ => String::new(),
    }
}

/// RFC 3339 in UTC with a `Z`, which sorts as text in time order.
fn time(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

fn date(at: NaiveDate) -> String {
    at.format("%Y-%m-%d").to_string()
}

fn parse_time(raw: &str) -> Result<DateTime<Utc>, IndexError> {
    DateTime::parse_from_rfc3339(raw)
        .map(|t| t.with_timezone(&Utc))
        .map_err(|_| IndexError::Row("time"))
}

fn get_time(row: &SqliteRow, column: &'static str) -> Result<DateTime<Utc>, IndexError> {
    parse_time(&row.try_get::<String, _>(column)?)
}

fn get_opt_time(row: &SqliteRow, column: &'static str) -> Result<Option<DateTime<Utc>>, IndexError> {
    row.try_get::<Option<String>, _>(column)?
        .map(|raw| parse_time(&raw))
        .transpose()
}

fn get_count(row: &SqliteRow, column: &'static str) -> Result<u32, IndexError> {
    u32::try_from(row.try_get::<i64, _>(column)?).map_err(|_| IndexError::Row(column))
}

fn get_opt_count(row: &SqliteRow, column: &'static str) -> Result<Option<u32>, IndexError> {
    row.try_get::<Option<i64>, _>(column)?
        .map(|n| u32::try_from(n).map_err(|_| IndexError::Row(column)))
        .transpose()
}

fn get_enum<T: DeserializeOwned>(row: &SqliteRow, column: &'static str) -> Result<T, IndexError> {
    serde_json::from_value(serde_json::Value::String(row.try_get(column)?))
        .map_err(|_| IndexError::Row(column))
}

fn get_opt_enum<T: DeserializeOwned>(row: &SqliteRow, column: &'static str) -> Result<Option<T>, IndexError> {
    row.try_get::<Option<String>, _>(column)?
        .map(|raw| {
            serde_json::from_value(serde_json::Value::String(raw)).map_err(|_| IndexError::Row(column))
        })
        .transpose()
}

/// A text column read back through its schema type, which checks it again.
fn get_opt_json<T: DeserializeOwned>(row: &SqliteRow, column: &'static str) -> Result<Option<T>, IndexError> {
    get_opt_enum(row, column)
}
