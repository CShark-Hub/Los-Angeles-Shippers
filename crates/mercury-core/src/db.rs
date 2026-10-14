//! The core's database: one SQLite file in WAL mode, brought up to the whole data model when it
//! is opened. The server and the admin commands open the same file, `MERCURY_DB`.

use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};

use chrono::{DateTime, SecondsFormat, Utc};
use sqlx::migrate::{MigrateError, Migrator};
use sqlx::sqlite::{
    SqliteConnectOptions, SqliteConnection, SqliteJournalMode, SqlitePool, SqlitePoolOptions,
    SqliteSynchronous,
};
use sqlx::{ConnectOptions, Connection as _};

/// The database when `MERCURY_DB` is not set: `mercury.db` in the working directory. The
/// containers always set it (`/data/mercury.db`).
pub const DEFAULT_PATH: &str = "mercury.db";

/// How long a write waits for another process's write to finish.
const BUSY_TIMEOUT: Duration = Duration::from_secs(5);

/// The migrations, built into the binary.
pub static MIGRATOR: Migrator = sqlx::migrate!();

#[derive(Debug, thiserror::Error)]
pub enum DbError {
    #[error("cannot open the database {}: {source}", path.display())]
    Open {
        path: PathBuf,
        #[source]
        source: sqlx::Error,
    },
    #[error("cannot bring the database {} up to date: {source}", path.display())]
    Migrate {
        path: PathBuf,
        #[source]
        source: MigrateError,
    },
}

/// The open database. Cloning it is cheap: the clones share one pool.
#[derive(Debug, Clone)]
pub struct Db(SqlitePool);

impl Db {
    /// Opens the file at `path`, creating it if it is missing, and applies the migrations it lacks.
    pub async fn open(path: &Path) -> Result<Self, DbError> {
        let options = SqliteConnectOptions::new()
            .filename(path)
            .create_if_missing(true)
            .journal_mode(SqliteJournalMode::Wal)
            // a committed audit row must survive a power loss; in WAL mode `Normal` can lose the
            // last commits
            .synchronous(SqliteSynchronous::Full)
            .foreign_keys(true)
            .busy_timeout(BUSY_TIMEOUT)
            // statements carry no values, but they would be one line per query at info
            .log_statements(log::LevelFilter::Debug)
            .log_slow_statements(log::LevelFilter::Warn, Duration::from_secs(1));
        let pool = SqlitePoolOptions::new()
            // checked as each connection comes back rather than when it is next handed out, so
            // one that can't begin a transaction never waits in the pool, nor holds a lock there
            .after_release(|conn, _| Box::pin(reusable(conn)))
            .connect_with(options)
            .await
            .map_err(|source| DbError::Open {
                path: path.to_owned(),
                source,
            })?;
        MIGRATOR.run(&pool).await.map_err(|source| DbError::Migrate {
            path: path.to_owned(),
            source,
        })?;
        Ok(Self(pool))
    }

    pub fn pool(&self) -> &SqlitePool {
        &self.0
    }
}

/// Whether a connection given back to the pool may be handed out again: not while it still
/// counts a transaction.
///
/// When a write fails because the disk is full (or on an I/O error), SQLite rolls the whole
/// transaction back by itself. sqlx keeps its own count of the connection's transactions and
/// lowers it only when its COMMIT or ROLLBACK succeeds, and the ROLLBACK it sends when the
/// transaction is dropped finds none and fails. Back in the pool as it is, the connection would
/// fail every later `BEGIN IMMEDIATE` ("attempted to call begin_with at non-zero transaction
/// depth"), and so every audit write on it, until the process restarts. Such a connection is
/// closed instead, which also ends a transaction SQLite might still hold, and the pool opens a
/// new one when it needs one. Closing it is the one change: the write that failed has already
/// returned its error, and nothing is written again for it.
async fn reusable(conn: &mut SqliteConnection) -> Result<bool, sqlx::Error> {
    // the ROLLBACK of a dropped transaction is queued on the connection; the ping comes back once
    // it has run, so the count read after it is the one that ROLLBACK left
    conn.ping().await?;
    if conn.is_in_transaction() {
        tracing::warn!("a database connection whose transaction failed to end is closed");
        return Ok(false);
    }
    Ok(true)
}

/// The database's path: `value` (from `MERCURY_DB`) or the default.
pub fn path(value: Option<&str>) -> PathBuf {
    PathBuf::from(value.filter(|value| !value.is_empty()).unwrap_or(DEFAULT_PATH))
}

/// The current time.
pub fn now() -> DateTime<Utc> {
    DateTime::from(SystemTime::now())
}

/// A time as the database stores it: RFC 3339 in UTC with milliseconds, a fixed width, so text
/// order is time order.
pub fn time(at: DateTime<Utc>) -> String {
    at.to_rfc3339_opts(SecondsFormat::Millis, true)
}

/// A time the database stored, or `None` for text that isn't one.
pub fn parse_time(text: &str) -> Option<DateTime<Utc>> {
    DateTime::parse_from_rfc3339(text)
        .ok()
        .map(|at| at.with_timezone(&Utc))
}

#[cfg(test)]
pub(crate) mod testing {
    use super::Db;

    /// A fresh database in a directory that disappears with the guard.
    pub(crate) async fn db() -> (tempfile::TempDir, Db) {
        let dir = tempfile::tempdir().unwrap();
        let db = Db::open(&dir.path().join("mercury.db")).await.unwrap();
        (dir, db)
    }

    /// A database handle over a pool a test built itself, e.g. read-only or with a size cap.
    pub(crate) fn from_pool(pool: sqlx::SqlitePool) -> Db {
        Db(pool)
    }
}

#[cfg(test)]
mod tests {
    use chrono::TimeDelta;

    use super::*;

    const TABLES: [&str; 11] = [
        "approvals",
        "audit_events",
        "feedback",
        "idempotency_keys",
        "probes",
        "questions",
        "sessions",
        "spend",
        "tokens",
        "user_projects",
        "users",
    ];

    async fn tables(db: &Db) -> Vec<String> {
        sqlx::query_scalar(
            "SELECT name FROM sqlite_schema WHERE type = 'table' \
             AND name NOT LIKE 'sqlite_%' AND name NOT LIKE '_sqlx_%' ORDER BY name",
        )
        .fetch_all(db.pool())
        .await
        .unwrap()
    }

    #[tokio::test]
    async fn a_fresh_file_gets_the_whole_data_model() {
        let (_dir, db) = testing::db().await;
        assert_eq!(tables(&db).await, TABLES);
        let strict: i64 = sqlx::query_scalar(
            "SELECT count(*) FROM pragma_table_list WHERE strict = 1 AND name NOT LIKE '_sqlx_%'",
        )
        .fetch_one(db.pool())
        .await
        .unwrap();
        assert_eq!(strict, 11, "every table is STRICT");
    }

    #[tokio::test]
    async fn the_journal_is_wal_and_foreign_keys_are_on() {
        let (_dir, db) = testing::db().await;
        let journal: String = sqlx::query_scalar("PRAGMA journal_mode")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(journal, "wal");
        let foreign_keys: i64 = sqlx::query_scalar("PRAGMA foreign_keys")
            .fetch_one(db.pool())
            .await
            .unwrap();
        assert_eq!(foreign_keys, 1);
    }

    #[tokio::test]
    async fn every_commit_is_synced_to_disk() {
        let (_dir, db) = testing::db().await;
        // 2 is FULL: the setting belongs to the connection, so every one the pool opens has it
        let mut held = Vec::new();
        for _ in 0..3 {
            held.push(db.pool().acquire().await.unwrap());
        }
        for conn in &mut held {
            let synchronous: i64 = sqlx::query_scalar("PRAGMA synchronous")
                .fetch_one(&mut **conn)
                .await
                .unwrap();
            assert_eq!(synchronous, 2);
        }
    }

    #[tokio::test]
    async fn opening_the_file_again_keeps_its_rows_and_applies_nothing_twice() {
        let dir = tempfile::tempdir().unwrap();
        let file = dir.path().join("mercury.db");
        let first = Db::open(&file).await.unwrap();
        sqlx::query("INSERT INTO spend (day, user_id, spent_usd) VALUES ('2026-10-02', '*', 0.5)")
            .execute(first.pool())
            .await
            .unwrap();
        first.pool().close().await;

        let again = Db::open(&file).await.unwrap();
        let applied: i64 = sqlx::query_scalar("SELECT count(*) FROM _sqlx_migrations")
            .fetch_one(again.pool())
            .await
            .unwrap();
        assert_eq!(applied, i64::try_from(MIGRATOR.iter().count()).unwrap());
        let spent: f64 = sqlx::query_scalar("SELECT spent_usd FROM spend")
            .fetch_one(again.pool())
            .await
            .unwrap();
        assert!((spent - 0.5).abs() < f64::EPSILON);
    }

    #[tokio::test]
    async fn a_row_pointing_at_a_missing_user_is_refused() {
        let (_dir, db) = testing::db().await;
        let result = sqlx::query(
            "INSERT INTO tokens (public_id, user_id, secret_sha256, label, created_at, expires_at) \
             VALUES ('k3Xq9AbCdEfG', 'u_nobody', zeroblob(32), 'laptop', 'x', 'y')",
        )
        .execute(db.pool())
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn a_digest_of_the_wrong_length_is_refused() {
        let (_dir, db) = testing::db().await;
        sqlx::query(
            "INSERT INTO users (id, email, display_name, role, created_at) \
             VALUES ('u_1', 'pm@example.com', 'Pm', 'pm', 'x')",
        )
        .execute(db.pool())
        .await
        .unwrap();
        let result = sqlx::query(
            "INSERT INTO tokens (public_id, user_id, secret_sha256, label, created_at, expires_at) \
             VALUES ('k3Xq9AbCdEfG', 'u_1', zeroblob(31), 'laptop', 'x', 'y')",
        )
        .execute(db.pool())
        .await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn e_mails_are_unique_whatever_their_case() {
        let (_dir, db) = testing::db().await;
        let add = |id: &'static str, email: &'static str| {
            sqlx::query(
                "INSERT INTO users (id, email, display_name, role, created_at) VALUES (?, ?, 'X', 'pm', 'x')",
            )
            .bind(id)
            .bind(email)
            .execute(db.pool())
        };
        add("u_1", "pm@example.com").await.unwrap();
        assert!(add("u_2", "PM@example.com").await.is_err());
    }

    #[test]
    fn the_migrations_keep_lf_line_endings() {
        for migration in MIGRATOR.iter() {
            assert!(
                !migration.sql.as_str().contains('\r'),
                "migration {} has a CR: check the checkout's line endings",
                migration.version
            );
        }
    }

    #[test]
    fn the_path_defaults_to_mercury_db_here() {
        assert_eq!(path(None), PathBuf::from("mercury.db"));
        assert_eq!(path(Some("")), PathBuf::from("mercury.db"));
        assert_eq!(path(Some("/data/mercury.db")), PathBuf::from("/data/mercury.db"));
    }

    #[test]
    fn stored_times_have_one_width_and_sort_in_time_order() {
        let at = parse_time("2026-10-02T09:31:35.123Z").unwrap();
        assert_eq!(time(at), "2026-10-02T09:31:35.123Z");
        let on_the_second = parse_time("2026-10-02T09:31:35Z").unwrap();
        assert_eq!(time(on_the_second), "2026-10-02T09:31:35.000Z");
        let later = time(at + TimeDelta::milliseconds(900));
        assert!(time(at) < later && later.len() == time(at).len());
        assert_eq!(parse_time("yesterday"), None);
    }
}
