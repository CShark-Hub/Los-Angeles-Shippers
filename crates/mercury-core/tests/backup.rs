//! `admin backup --to <file>`: a copy of the whole database taken while the core keeps writing,
//! which opens as a database of its own with the people, their tokens, their sessions, the
//! questions and an audit chain that verifies; never written over an existing file, and audited
//! in the database it was taken from, or not kept at all.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::path::{Path, PathBuf};
use std::time::Duration;

use chrono::{DateTime, TimeDelta, Utc};
use clap::Parser as _;
use mercury_core::admin::{self, AdminError};
use mercury_core::audit;
use mercury_core::backup;
use mercury_core::cli::{Cli, Command};
use mercury_core::db::{self, Db};
use mercury_core::identity;
use mercury_schema::api::{Accepted, ErrorCode, Usd};
use mercury_schema::events::{Event, EventError};
use mercury_schema::{ProjectId, QuestionId, Role};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

// ------------------------------------------------------------------------------------ the setup

const Q_ANNA: &str = "q_01JB2M3N4P5Q6R7S8T9V0W1X2A";
const Q_BEN: &str = "q_01JB2M3N4P5Q6R7S8T9V0W1X2B";

/// A core's database with something in every table a restore must bring back.
struct Core {
    dir: tempfile::TempDir,
    db: Db,
    /// Anna's token, as `token create` printed it.
    anna_token: String,
}

impl Core {
    fn file(&self) -> PathBuf {
        self.dir.path().join("mercury.db")
    }
}

async fn person(
    db: &Db,
    email: &str,
    role: Role,
    projects: &[&str],
    now: DateTime<Utc>,
) -> (String, String, String) {
    let id = admin::add_user(db, email, "Pat Example", role, now)
        .await
        .unwrap();
    if !projects.is_empty() {
        let projects: Vec<String> = projects.iter().map(|p| (*p).to_owned()).collect();
        admin::grant(
            db,
            email,
            &projects,
            false,
            &admin::KnownProjects::new(["delta", "gamma"]),
            now,
        )
        .await
        .unwrap();
    }
    let created = admin::create_token(db, email, "laptop", TimeDelta::days(30), now)
        .await
        .unwrap();
    (id, created.token.reveal(), created.token.public_id().to_owned())
}

/// A question of `user` with its token on `project`, asked at `at`, and two of its events.
async fn question(db: &Db, id: &str, user: &(String, String, String), project: &str, at: DateTime<Utc>) {
    sqlx::query(
        "INSERT INTO questions (id, user_id, token_public_id, channel, project, class, status, text_redacted, \
         answer_redacted, created_at) VALUES (?, ?, ?, 'cli', ?, 'progress', 'delivered', 'what was merged?', \
         'Two items.', ?)",
    )
    .bind(id)
    .bind(&user.0)
    .bind(&user.2)
    .bind(project)
    .bind(db::time(at))
    .execute(db.pool())
    .await
    .unwrap();
    let qid = QuestionId::parse(id).unwrap();
    let project = ProjectId::parse(project).unwrap();
    let accepted = Event::Accepted(Accepted::new(qid.clone()));
    let _ = audit::record(db, &qid, None, accepted, at).await.unwrap();
    let error = Event::Error(EventError {
        code: ErrorCode::Internal,
        message: "Something failed.".to_owned(),
        cost_usd: Usd::default(),
        judge_mode: None,
    });
    let _ = audit::record(db, &qid, Some(&project), error, at).await.unwrap();
}

/// Two people who ask and an admin, each with a token; a browser session of Anna's; a question
/// each; a probe's row in the audit.
async fn core() -> Core {
    let dir = tempfile::tempdir().unwrap();
    let db = Db::open(&dir.path().join("mercury.db")).await.unwrap();
    let now = db::now();
    let anna = person(&db, "anna@example.com", Role::Pm, &["delta"], now).await;
    let ben = person(&db, "ben@example.com", Role::Pm, &["delta", "gamma"], now).await;
    let _admin = person(&db, "admin@example.com", Role::Admin, &[], now).await;
    sqlx::query(
        "INSERT INTO sessions (id_sha256, token_public_id, user_id, created_at, expires_at) VALUES (?, ?, ?, ?, ?)",
    )
    .bind(Sha256::digest(b"a session id").to_vec())
    .bind(&anna.2)
    .bind(&anna.0)
    .bind(db::time(now))
    .bind(db::time(now + TimeDelta::hours(12)))
    .execute(db.pool())
    .await
    .unwrap();
    question(&db, Q_ANNA, &anna, "delta", now).await;
    question(&db, Q_BEN, &ben, "gamma", now).await;
    let delta = ProjectId::parse("delta").unwrap();
    audit::action(
        &db,
        "probe_registered",
        Some(&delta),
        &json!({ "access": "code" }),
        now,
    )
    .await
    .unwrap();
    Core {
        dir,
        db,
        anna_token: anna.1,
    }
}

/// `mercury-core admin <args>` the way `main` runs it: what it printed, or its error.
async fn cli(db: &Db, args: &[&str]) -> Result<String, AdminError> {
    let command = Cli::try_parse_from(["mercury-core", "admin"].iter().chain(args)).unwrap();
    let Some(Command::Admin(command)) = command.command else {
        panic!("not an admin command");
    };
    let examples = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples");
    let (mut out, mut note) = (Vec::new(), Vec::new());
    admin::run(command, db, &examples, &mut out, &mut note).await?;
    Ok(String::from_utf8(out).unwrap())
}

async fn strings(db: &Db, query: &'static str) -> Vec<String> {
    sqlx::query_scalar(query).fetch_all(db.pool()).await.unwrap()
}

/// The `backup_written` rows: question, project and payload.
async fn backup_rows(db: &Db) -> Vec<(Option<String>, Option<String>, Value)> {
    let rows: Vec<(Option<String>, Option<String>, String)> = sqlx::query_as(
        "SELECT question_id, project, payload_json FROM audit_events WHERE type = 'backup_written' ORDER BY seq",
    )
    .fetch_all(db.pool())
    .await
    .unwrap();
    rows.into_iter()
        .map(|(question, project, payload)| (question, project, serde_json::from_str(&payload).unwrap()))
        .collect()
}

/// Each row's `seq` and `hash`, in order.
async fn chain(db: &Db) -> Vec<(i64, Vec<u8>)> {
    sqlx::query_as("SELECT seq, hash FROM audit_events ORDER BY seq")
        .fetch_all(db.pool())
        .await
        .unwrap()
}

/// Where the copy is written before it takes its name: `<file>.partial`.
fn partial(to: &Path) -> PathBuf {
    let mut name = to.as_os_str().to_owned();
    name.push(".partial");
    PathBuf::from(name)
}

/// What is on the disk at a moment of the backup: whether the file by its name is there, and the
/// size of its `.partial` (0 when there is none).
fn on_disk(to: &Path) -> (bool, u64) {
    let partial = std::fs::metadata(partial(to)).map_or(0, |meta| meta.len());
    (to.exists(), partial)
}

/// Every `backup_written` row fails to be written, as on a full disk.
async fn refuse_backup_rows(db: &Db) {
    sqlx::query(
        "CREATE TRIGGER no_backup BEFORE INSERT ON audit_events WHEN NEW.type = 'backup_written' \
         BEGIN SELECT RAISE(ABORT, 'database or disk is full'); END",
    )
    .execute(db.pool())
    .await
    .unwrap();
}

// ------------------------------------------------------------------------------------ the tests

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_backup_taken_while_the_core_writes_opens_with_everything_and_a_chain_that_verifies() {
    let core = core().await;
    let before = audit::last_seq(&core.db).await.unwrap();
    // the core keeps serving: another process appends to the audit all through the backup
    let serving = Db::open(&core.file()).await.unwrap();
    let writer = tokio::spawn(async move {
        for n in 0..200 {
            audit::action(&serving, "token_revoked", None, &json!({ "n": n }), db::now())
                .await
                .unwrap();
        }
    });
    // the backup starts once the writer is well under way, and the writer goes on through it
    while audit::last_seq(&core.db).await.unwrap() < before + 50 {
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    // a folder that isn't there yet is made
    let to = core.dir.path().join("backups").join("mercury-2026-10-06.db");
    let printed = cli(&core.db, &["backup", "--to", to.to_str().unwrap()])
        .await
        .unwrap();
    writer.await.unwrap();

    let bytes = std::fs::metadata(&to).unwrap().len();
    assert!(printed.contains(&to.display().to_string()), "{printed}");
    assert!(printed.contains(&format!("{bytes} bytes")), "{printed}");

    // the copy is a database of its own, as a restored core opens it
    let copy = Db::open(&to).await.unwrap();
    assert_eq!(
        strings(&copy, "SELECT email FROM users ORDER BY email").await,
        ["admin@example.com", "anna@example.com", "ben@example.com"]
    );
    assert_eq!(
        strings(&copy, "SELECT project FROM user_projects ORDER BY project").await,
        ["delta", "delta", "gamma"]
    );
    assert_eq!(
        strings(&copy, "SELECT public_id FROM tokens ORDER BY public_id").await,
        strings(&core.db, "SELECT public_id FROM tokens ORDER BY public_id").await
    );
    assert_eq!(
        strings(&copy, "SELECT token_public_id FROM sessions").await.len(),
        1,
        "the browser session comes back"
    );
    assert_eq!(
        strings(&copy, "SELECT id FROM questions ORDER BY id").await,
        [Q_ANNA, Q_BEN]
    );
    // a token works against the copy as it did against the source
    let caller = identity::authenticate_token(&copy, &core.anna_token, db::now())
        .await
        .unwrap();
    assert_eq!(caller.email, "anna@example.com");

    // its chain verifies, and is the source's up to the moment of the copy: one snapshot, whatever
    // was written while it was taken
    let verified = audit::verify(&copy).await.unwrap();
    assert_eq!(verified.broken, None);
    assert!(
        verified.rows >= before + 50 && verified.rows <= before + 200,
        "{} rows in the copy, {before} before the backup",
        verified.rows
    );
    let (copied, source) = (chain(&copy).await, chain(&core.db).await);
    assert_eq!(
        copied[..],
        source[..copied.len()],
        "the copy is a prefix of the source's chain"
    );
    assert!(
        backup_rows(&copy).await.is_empty(),
        "the copy doesn't hold its own row"
    );

    // the source has the one row, without a question or a project, naming the file and its size
    assert_eq!(audit::verify(&core.db).await.unwrap().broken, None);
    assert_eq!(audit::last_seq(&core.db).await.unwrap(), before + 200 + 1);
    let rows = backup_rows(&core.db).await;
    assert_eq!(
        rows,
        [(
            None,
            None,
            json!({ "file": "mercury-2026-10-06.db", "bytes": bytes })
        )]
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(&to).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "the copy is the owner's alone");
    }
}

#[tokio::test]
async fn a_backup_never_overwrites_a_file() {
    let core = core().await;
    let to = core.dir.path().join("mercury-2026-10-06.db");
    let first = backup::write(&core.db, &to, db::now).await.unwrap();
    let written = std::fs::read(&to).unwrap();
    assert_eq!(first.bytes, u64::try_from(written.len()).unwrap());

    // the same name again, through the command line: refused, the file as it was, no second row
    let err = cli(&core.db, &["backup", "--to", to.to_str().unwrap()])
        .await
        .unwrap_err();
    assert!(
        matches!(err, AdminError::BackupExists(ref path) if path == &to),
        "{err:?}"
    );
    let message = err.to_string();
    assert!(
        message.contains("exists") && message.contains("never overwrites"),
        "{message}"
    );
    assert_eq!(std::fs::read(&to).unwrap(), written);
    assert_eq!(backup_rows(&core.db).await.len(), 1);

    // an empty file, which SQLite alone would write into, and the database itself, are refused too
    let empty = core.dir.path().join("empty.db");
    std::fs::write(&empty, b"").unwrap();
    for target in [empty.clone(), core.file()] {
        let err = backup::write(&core.db, &target, db::now).await.unwrap_err();
        assert!(matches!(err, AdminError::BackupExists(_)), "{target:?}: {err:?}");
    }
    assert_eq!(std::fs::metadata(&empty).unwrap().len(), 0);
    assert_eq!(audit::verify(&core.db).await.unwrap().broken, None);
    assert_eq!(backup_rows(&core.db).await.len(), 1);
}

#[tokio::test]
async fn a_backup_that_cant_be_audited_is_not_kept() {
    let core = core().await;
    sqlx::query(
        "CREATE TRIGGER no_backup BEFORE INSERT ON audit_events WHEN NEW.type = 'backup_written' \
         BEGIN SELECT RAISE(ABORT, 'the disk is full'); END",
    )
    .execute(core.db.pool())
    .await
    .unwrap();
    let to = core.dir.path().join("mercury-2026-10-06.db");
    let err = backup::write(&core.db, &to, db::now).await.unwrap_err();
    assert!(
        matches!(err, AdminError::Backup { ref path, .. } if path == &to),
        "{err:?}"
    );
    assert!(err.to_string().contains("can't be audited"), "{err}");
    assert!(!to.exists(), "no audit, no backup");

    // the name isn't taken by the failed attempt
    sqlx::query("DROP TRIGGER no_backup")
        .execute(core.db.pool())
        .await
        .unwrap();
    backup::write(&core.db, &to, db::now).await.unwrap();
    assert!(to.exists());
    assert_eq!(backup_rows(&core.db).await.len(), 1);
}

#[tokio::test]
async fn a_backup_taken_after_retention_still_verifies() {
    let core = core().await;
    let old = db::now() - TimeDelta::days(100);
    let ben = sqlx::query_as::<_, (String, String)>(
        "SELECT u.id, t.public_id FROM users u JOIN tokens t ON t.user_id = u.id WHERE u.email = 'ben@example.com'",
    )
    .fetch_one(core.db.pool())
    .await
    .unwrap();
    let ben = (ben.0, String::new(), ben.1);
    question(&core.db, "q_01JB2M3N4P5Q6R7S8T9V0W1X2C", &ben, "delta", old).await;
    let retained = audit::retain(&core.db, 90, db::now()).await.unwrap();
    assert_eq!(retained.payloads, 2);

    let to = core.dir.path().join("after-retention.db");
    backup::write(&core.db, &to, db::now).await.unwrap();
    let copy = Db::open(&to).await.unwrap();
    assert_eq!(
        audit::verify(&copy).await.unwrap().broken,
        None,
        "digests, not payloads"
    );
    let cleared: i64 = sqlx::query_scalar("SELECT count(*) FROM audit_events WHERE payload_json IS NULL")
        .fetch_one(copy.pool())
        .await
        .unwrap();
    assert_eq!(cleared, 2);
    assert_eq!(
        strings(
            &copy,
            "SELECT type FROM audit_events WHERE type = 'audit_retained'"
        )
        .await,
        ["audit_retained"]
    );
}

#[tokio::test]
async fn the_copy_takes_its_name_only_once_its_row_has_committed() {
    let core = core().await;
    let to = core.dir.path().join("mercury-2026-10-07.db");

    // the row can't be written: the whole copy was beside the name, never at it, and is gone
    refuse_backup_rows(&core.db).await;
    let mut seen = None;
    let err = backup::write(&core.db, &to, || {
        seen = Some(on_disk(&to));
        db::now()
    })
    .await
    .unwrap_err();
    let (named, written) = seen.unwrap();
    assert!(!named && written > 0, "the copy is written as its .partial");
    assert!(!to.exists(), "no row, no file by the name");
    assert!(!partial(&to).exists(), "the partial copy is removed");
    assert!(backup_rows(&core.db).await.is_empty());
    let message = err.to_string();
    assert!(
        message.starts_with(&format!("cannot write the backup {}:", to.display())),
        "{message}"
    );
    assert!(message.contains("database or disk is full"), "{message}");
    assert!(
        message.ends_with(&format!(
            "the partial copy {} was removed",
            partial(&to).display()
        )),
        "{message}"
    );

    // the row can be written: the copy takes its name after it, and the .partial is gone
    sqlx::query("DROP TRIGGER no_backup")
        .execute(core.db.pool())
        .await
        .unwrap();
    let mut seen = None;
    let backup = backup::write(&core.db, &to, || {
        seen = Some(on_disk(&to));
        db::now()
    })
    .await
    .unwrap();
    assert_eq!(seen, Some((false, backup.bytes)));
    assert_eq!(std::fs::metadata(&to).unwrap().len(), backup.bytes);
    assert!(!partial(&to).exists());
    assert_eq!(backup_rows(&core.db).await.len(), 1);
    assert_eq!(audit::last_seq(&core.db).await.unwrap(), backup.seq);
}

#[tokio::test]
async fn a_partial_copy_that_cant_be_removed_is_reported() {
    let core = core().await;
    let to = core.dir.path().join("mercury-2026-10-07.db");
    refuse_backup_rows(&core.db).await;
    // once the copy is written, its .partial turns into a folder with a file in it, which no
    // `remove_file` removes on any system
    let err = backup::write(&core.db, &to, || {
        let partial = partial(&to);
        std::fs::remove_file(&partial).unwrap();
        std::fs::create_dir(&partial).unwrap();
        std::fs::write(partial.join("kept"), b"").unwrap();
        db::now()
    })
    .await
    .unwrap_err();
    let message = err.to_string();
    assert!(message.contains("can't be audited"), "{message}");
    assert!(
        message.contains(&format!(
            "the partial copy {} could not be removed (",
            partial(&to).display()
        )),
        "{message}"
    );
    assert!(message.ends_with("delete it"), "{message}");
    assert!(!to.exists());
    assert!(backup_rows(&core.db).await.is_empty());
}

#[tokio::test]
async fn a_partial_copy_left_behind_is_refused() {
    let core = core().await;
    let to = core.dir.path().join("mercury-2026-10-07.db");
    std::fs::write(partial(&to), b"half a copy").unwrap();
    let err = backup::write(&core.db, &to, db::now).await.unwrap_err();
    assert!(
        matches!(err, AdminError::Backup { ref path, .. } if path == &to),
        "{err:?}"
    );
    let message = err.to_string();
    assert!(
        message.contains(&format!("{} is there already", partial(&to).display()))
            && message.contains("nothing was written"),
        "{message}"
    );
    assert_eq!(std::fs::read(partial(&to)).unwrap(), b"half a copy");
    assert!(!to.exists());
    assert!(backup_rows(&core.db).await.is_empty());
}

#[tokio::test]
async fn the_row_is_stamped_when_the_copy_has_finished() {
    let core = core().await;
    let to = core.dir.path().join("mercury-2026-10-07.db");
    // a time of the test's own, after everything the core has written
    let finished = db::now() + TimeDelta::minutes(5);
    let mut seen = None;
    let backup = backup::write(&core.db, &to, || {
        seen = Some(on_disk(&to));
        finished
    })
    .await
    .unwrap();
    assert_eq!(
        seen,
        Some((false, backup.bytes)),
        "the clock is read once the whole copy is on the disk"
    );
    assert_eq!(backup.at, finished);
    assert_eq!(
        strings(
            &core.db,
            "SELECT created_at FROM audit_events WHERE type = 'backup_written'"
        )
        .await,
        [db::time(finished)]
    );
    assert_eq!(audit::verify(&core.db).await.unwrap().broken, None);
}

#[tokio::test]
async fn the_success_line_names_the_file_by_its_absolute_path() {
    let core = core().await;
    // a relative name, resolved where the command runs; nextest runs each test in a process of its
    // own, and no other test here reads the working folder
    let back = std::env::current_dir().unwrap();
    std::env::set_current_dir(core.dir.path()).unwrap();
    let printed = cli(&core.db, &["backup", "--to", "backups/mercury-2026-10-07.db"]).await;
    std::env::set_current_dir(back).unwrap();
    let printed = printed.unwrap();

    let named = printed
        .strip_prefix("wrote ")
        .and_then(|rest| rest.split(" (").next())
        .unwrap();
    assert!(Path::new(named).is_absolute(), "{printed}");
    let file = core.dir.path().join("backups").join("mercury-2026-10-07.db");
    assert_eq!(
        std::fs::canonicalize(named).unwrap(),
        std::fs::canonicalize(&file).unwrap(),
        "{printed}"
    );
}
