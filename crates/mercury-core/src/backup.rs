//! `admin backup --to <file>`: a copy of the whole database with SQLite's `VACUUM INTO`, taken
//! while the core keeps serving. The copy is one read transaction's snapshot, so it never holds
//! half of a write, and in WAL mode it blocks no writer. It is a full database (the people, the
//! tokens' and the sessions' digests, the questions, the audit chain), so the file is created for
//! its owner alone, and nothing is ever written over a file that is already there.
//!
//! The copy is written as `<file>.partial` and takes its name only once its `backup_written` row
//! has committed, so a file by the name given is always a complete, audited backup. A copy that
//! fails, can't be audited or can't take its name is removed, and the error names the file and
//! says whether the `.partial` went.

use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::json;

use crate::admin::AdminError;
use crate::audit;
use crate::db::Db;

/// A backup that was written and audited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Written {
    /// Where it is: the file's absolute path.
    pub path: PathBuf,
    /// Its size in bytes.
    pub bytes: u64,
    /// The `seq` of its `backup_written` row.
    pub seq: u64,
    /// The row's time: when the copy finished.
    pub at: DateTime<Utc>,
}

fn failed(path: &Path, source: io::Error) -> AdminError {
    AdminError::Backup {
        path: path.to_owned(),
        source,
    }
}

/// Whether `path` names a file, not a folder: not empty, and not ending in a separator, `.` or
/// `..`, which would put the `.partial` somewhere else than beside the file.
fn names_a_file(path: &Path) -> bool {
    let text = path.as_os_str().to_string_lossy();
    let last = text.rsplit(std::path::is_separator).next().unwrap_or_default();
    !matches!(last, "" | "." | "..")
}

/// `<file>.partial`, beside `<file>`.
fn partial_of(path: &Path) -> PathBuf {
    let mut name = OsString::from(path.as_os_str());
    name.push(".partial");
    PathBuf::from(name)
}

/// Refuses a name anything holds (a file, even an empty one, which `VACUUM INTO` alone would write
/// into; a folder; a link) before anything is written.
fn refuse_taken(path: &Path) -> Result<(), AdminError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Err(AdminError::BackupExists(path.to_owned())),
        Err(err) if err.kind() == ErrorKind::NotFound => Ok(()),
        Err(err) => Err(failed(path, err)),
    }
}

/// Takes `<file>.partial` for the copy, for its owner alone, and leaves it empty for SQLite to
/// fill. `create_new` fails when anything is there: the copy of a backup that was stopped half-way,
/// or of one to the same name that is still running, so two backups never write one file.
fn reserve(path: &Path, partial: &Path) -> Result<(), AdminError> {
    if let Some(folder) = path.parent().filter(|folder| !folder.as_os_str().is_empty()) {
        fs::create_dir_all(folder).map_err(|err| failed(path, err))?;
    }
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    match options.open(partial) {
        Ok(_) => Ok(()),
        Err(err) if err.kind() == ErrorKind::AlreadyExists => {
            let message = format!(
                "{} is there already: the copy of a backup that was stopped, or of one still running; \
                 nothing was written: delete it once no backup runs",
                partial.display()
            );
            Err(failed(path, io::Error::new(ErrorKind::AlreadyExists, message)))
        }
        Err(err) => Err(failed(path, err)),
    }
}

/// The copy, into the empty `partial` (`target` is its UTF-8 text), synced to the disk; its size.
async fn copy(db: &Db, partial: &Path, target: &str) -> io::Result<u64> {
    sqlx::query("VACUUM INTO ?")
        .bind(target)
        .execute(db.pool())
        .await
        .map_err(|err| match err {
            sqlx::Error::Io(err) => err,
            err => io::Error::other(err),
        })?;
    let file = OpenOptions::new().write(true).open(partial)?;
    file.sync_all()?;
    Ok(file.metadata()?.len())
}

/// Gives the finished copy its name. `rename` replaces whatever is at the target, so the name is
/// looked at again right before: a file that appeared there while the copy was written stays, and
/// the copy doesn't. Another backup can't be that file, since it would have needed the same
/// `.partial`.
fn publish(partial: &Path, path: &Path) -> io::Result<()> {
    match fs::symlink_metadata(path) {
        Ok(_) => {
            return Err(io::Error::new(
                ErrorKind::AlreadyExists,
                "a file by that name appeared while the copy was written, and a backup never \
                 overwrites a file",
            ));
        }
        Err(err) if err.kind() == ErrorKind::NotFound => {}
        Err(err) => return Err(err),
    }
    fs::rename(partial, path)
}

/// The error of a copy that isn't kept: what went wrong (`what`, then its `cause`), and what became
/// of its `.partial`, which the caller has just tried to remove (`removed`). It names the backup's
/// file, so a full disk reads as the backup's and not the live database's.
fn not_kept(
    path: &Path,
    partial: &Path,
    what: &str,
    cause: &io::Error,
    removed: io::Result<()>,
) -> AdminError {
    let fate = match removed {
        Ok(()) => format!("the partial copy {} was removed", partial.display()),
        Err(err) if err.kind() == ErrorKind::NotFound => {
            format!("no partial copy is left at {}", partial.display())
        }
        Err(err) => format!(
            "the partial copy {} could not be removed ({err}): delete it",
            partial.display()
        ),
    };
    failed(
        path,
        io::Error::new(cause.kind(), format!("{what}: {cause}; {fate}")),
    )
}

/// Removes the `.partial` of a copy that isn't kept; the error that says so.
fn discard(path: &Path, partial: &Path, what: &str, cause: &io::Error) -> AdminError {
    let removed = fs::remove_file(partial);
    not_kept(path, partial, what, cause, removed)
}

/// Writes a backup of `db` to `path` and audits it; `clock` gives the row's time, and is read once
/// the copy has finished.
///
/// The copy goes to `<file>.partial` and takes its name only after its `backup_written` row has
/// committed: only then is its size known, and so no file by the name given exists without its
/// row. The row comes after the snapshot, so the copy's chain ends at the row before its own,
/// which is a chain that verifies. A copy that fails, can't be audited or can't take its name is
/// removed, and the error says whether the removal worked.
pub async fn write(
    db: &Db,
    path: &Path,
    clock: impl FnOnce() -> DateTime<Utc>,
) -> Result<Written, AdminError> {
    if !names_a_file(path) {
        return Err(AdminError::Invalid(
            "name the file to write the backup to".to_owned(),
        ));
    }
    // an absolute path: the line printed at the end says exactly where the file is, and SQLite
    // never reads the name as a `file:` URI and resolves it where the checks here did
    let path = std::path::absolute(path).map_err(|err| failed(path, err))?;
    let partial = partial_of(&path);
    let target = partial
        .to_str()
        .ok_or_else(|| AdminError::Invalid("the backup's path must be UTF-8".to_owned()))?;
    refuse_taken(&path)?;
    reserve(&path, &partial)?;
    let bytes = match copy(db, &partial, target).await {
        Ok(bytes) => bytes,
        Err(err) => return Err(discard(&path, &partial, "the copy failed", &err)),
    };
    let at = clock();
    // the name only: a path says where the operator keeps their backups, which the audit's readers
    // don't need
    let file = path
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .unwrap_or_default();
    let payload = json!({ "file": file, "bytes": bytes });
    let seq = match audit::action(db, "backup_written", None, &payload, at).await {
        Ok(seq) => seq,
        Err(err) => {
            let what = "the copy can't be audited, so it isn't kept";
            return Err(discard(&path, &partial, what, &io::Error::other(err)));
        }
    };
    if let Err(err) = publish(&partial, &path) {
        let what = format!("row {seq} records the copy, but it can't take its name");
        return Err(discard(&path, &partial, &what, &err));
    }
    Ok(Written { path, bytes, seq, at })
}

/// `admin backup --to <file>`: the backup, its row stamped by `clock` once the copy has finished,
/// then one line saying where it is.
pub async fn run(
    db: &Db,
    path: &Path,
    out: &mut dyn Write,
    clock: impl FnOnce() -> DateTime<Utc>,
) -> Result<(), AdminError> {
    let written = write(db, path, clock).await?;
    writeln!(
        out,
        "wrote {} ({} bytes): the whole database, keep it like a secret",
        written.path.display(),
        written.bytes
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_partial_is_beside_the_file_and_a_folder_is_no_file() {
        assert_eq!(
            partial_of(Path::new("backups/mercury-2026-10-07.db")),
            Path::new("backups/mercury-2026-10-07.db.partial")
        );
        assert!(names_a_file(Path::new("mercury.db")));
        assert!(names_a_file(Path::new("backups/mercury.db")));
        for folder in ["", "backups/", ".", "..", "backups/.", "backups/.."] {
            assert!(!names_a_file(Path::new(folder)), "{folder:?}");
        }
    }

    #[test]
    fn a_full_disk_names_the_backup_and_says_what_became_of_its_partial() {
        let path = Path::new("/data/backups/mercury-2026-10-07.db");
        let partial = partial_of(path);
        let full = io::Error::new(ErrorKind::StorageFull, "database or disk is full");

        let removed = not_kept(path, &partial, "the copy failed", &full, Ok(()));
        let AdminError::Backup { path: named, source } = &removed else {
            panic!("{removed:?}");
        };
        assert_eq!(named, path);
        assert_eq!(source.kind(), ErrorKind::StorageFull, "the cause's kind is kept");
        assert_eq!(
            removed.to_string(),
            format!(
                "cannot write the backup {}: the copy failed: database or disk is full; the partial copy {} \
                 was removed",
                path.display(),
                partial.display()
            )
        );

        // a removal that fails is said, with its reason, and what to do
        let denied = io::Error::new(ErrorKind::PermissionDenied, "access denied");
        let stuck = not_kept(path, &partial, "the copy failed", &full, Err(denied)).to_string();
        assert!(stuck.contains("database or disk is full"), "{stuck}");
        assert!(
            stuck.ends_with(&format!(
                "the partial copy {} could not be removed (access denied): delete it",
                partial.display()
            )),
            "{stuck}"
        );

        // and one already gone is not one to delete
        let gone = not_kept(
            path,
            &partial,
            "the copy failed",
            &full,
            Err(ErrorKind::NotFound.into()),
        );
        assert!(
            gone.to_string()
                .ends_with(&format!("no partial copy is left at {}", partial.display())),
            "{gone}"
        );
    }
}
