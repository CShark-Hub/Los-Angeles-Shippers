//! The code snapshot on the tmpfs `/repo`: a commit's tarball unpacked as it streams in, its
//! `exclude` paths deleted, then the link `/repo/current` swapped to it; two snapshots are kept.
//!
//! Only directories and regular files are written. Every link, every special file and every entry
//! whose path is absolute, holds `..` or isn't UTF-8 is skipped, so nothing in a snapshot points
//! out of it. The size cap counts the bytes written and stops the unpack at the first byte past
//! it. The errors and the summary carry counts and sizes, never a path or a file's contents.

use std::fmt;
use std::fs::{self, File};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use globset::GlobSet;
use tokio::sync::mpsc::{Receiver, Sender};

/// The folder of the snapshots under `/repo`, one per commit.
pub const SNAPSHOTS: &str = "snapshots";

/// The link to the snapshot in use, under `/repo` (what `serve` reads).
pub const CURRENT: &str = crate::serve::CURRENT;

/// `PROBE_SNAPSHOT_MAX_MB` when it isn't set.
pub const DEFAULT_MAX_MB: u64 = 512;

/// How many chunks of the reply wait for the unpacker at most.
pub const CHUNKS_IN_FLIGHT: usize = 8;

/// What one unpack wrote.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct Unpacked {
    pub files: usize,
    pub bytes: u64,
    /// Links, special files and paths that would leave the snapshot.
    pub skipped: usize,
    /// Files deleted afterwards because an `exclude` glob matches them.
    pub excluded: usize,
}

impl fmt::Display for Unpacked {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} files ({:.1} MB), {} excluded, {} entries skipped",
            self.files,
            megabytes(self.bytes),
            self.excluded,
            self.skipped
        )
    }
}

fn megabytes(bytes: u64) -> f64 {
    bytes as f64 / (1024.0 * 1024.0)
}

/// Why a snapshot wasn't made. Every message is fixed text and numbers.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SnapshotError {
    #[error("the snapshot is larger than PROBE_SNAPSHOT_MAX_MB ({0} MB)")]
    TooLarge(u64),
    #[error("the tarball broke off or isn't a gzipped tar archive")]
    Unreadable,
    #[error("cannot write the snapshot ({0})")]
    Write(&'static str),
    #[error("the commit id isn't one")]
    BadCommit,
}

/// Unpacks a gzipped tar from `reader` into `dir`, which is created, dropping the archive's top
/// folder (GitHub's `owner-repo-sha/`). Stops with [`SnapshotError::TooLarge`] at the first byte
/// past `cap` bytes; the caller deletes what was written.
pub fn unpack(reader: impl Read, dir: &Path, cap: u64) -> Result<Unpacked, SnapshotError> {
    fs::create_dir_all(dir).map_err(|_| SnapshotError::Write("the snapshot's folder"))?;
    let mut archive = tar::Archive::new(flate2::read::GzDecoder::new(reader));
    let mut done = Unpacked::default();
    let entries = archive.entries().map_err(|_| SnapshotError::Unreadable)?;
    for entry in entries {
        let mut entry = entry.map_err(|_| SnapshotError::Unreadable)?;
        let kind = entry.header().entry_type();
        if kind.is_pax_global_extensions() {
            continue;
        }
        let Some(relative) = inside(&entry.path_bytes()) else {
            done.skipped += 1;
            continue;
        };
        if relative.as_os_str().is_empty() {
            continue; // the top folder itself
        }
        let target = dir.join(&relative);
        if kind.is_dir() {
            fs::create_dir_all(&target).map_err(|_| SnapshotError::Write("a folder"))?;
        } else if kind.is_file() || kind.is_contiguous() {
            if let Some(parent) = target.parent() {
                fs::create_dir_all(parent).map_err(|_| SnapshotError::Write("a folder"))?;
            }
            let mut file = File::create(&target).map_err(|_| SnapshotError::Write("a file"))?;
            copy_capped(&mut entry, &mut file, &mut done.bytes, cap)?;
            done.files += 1;
        } else {
            done.skipped += 1;
        }
    }
    Ok(done)
}

/// The entry's path below the archive's top folder, or `None` when it would leave the snapshot or
/// isn't UTF-8.
fn inside(raw: &[u8]) -> Option<PathBuf> {
    let text = std::str::from_utf8(raw).ok()?;
    // a backslash and a colon are a separator and a drive on Windows, where the tests run too
    if text.starts_with('/') || text.contains('\0') || (cfg!(windows) && text.contains('\\')) {
        return None;
    }
    let mut parts = text.split('/').filter(|part| !part.is_empty() && *part != ".");
    parts.next()?; // the top folder
    let mut path = PathBuf::new();
    for part in parts {
        if part == ".." || (cfg!(windows) && part.contains(':')) {
            return None;
        }
        path.push(part);
    }
    Some(path)
}

fn copy_capped(
    from: &mut impl Read,
    to: &mut File,
    written: &mut u64,
    cap: u64,
) -> Result<(), SnapshotError> {
    let mut buffer = [0u8; 64 * 1024];
    loop {
        let read = from.read(&mut buffer).map_err(|_| SnapshotError::Unreadable)?;
        if read == 0 {
            return Ok(());
        }
        *written += read as u64;
        if *written > cap {
            return Err(SnapshotError::TooLarge(cap / (1024 * 1024)));
        }
        to.write_all(&buffer[..read])
            .map_err(|_| SnapshotError::Write("a file"))?;
    }
}

/// Deletes every file under `root` that an `exclude` glob matches, by its path with `/`.
pub fn remove_excluded(root: &Path, exclude: &GlobSet) -> Result<usize, SnapshotError> {
    let mut removed = 0;
    let mut folders = vec![root.to_owned()];
    while let Some(folder) = folders.pop() {
        let entries = fs::read_dir(&folder).map_err(|_| SnapshotError::Write("a folder"))?;
        for entry in entries {
            let entry = entry.map_err(|_| SnapshotError::Write("a folder"))?;
            let path = entry.path();
            let kind = entry.file_type().map_err(|_| SnapshotError::Write("a file"))?;
            if kind.is_dir() {
                folders.push(path);
                continue;
            }
            let Ok(relative) = path.strip_prefix(root) else {
                continue;
            };
            let relative = relative.to_string_lossy().replace('\\', "/");
            if exclude.is_match(&relative) {
                fs::remove_file(&path).map_err(|_| SnapshotError::Write("an excluded file"))?;
                removed += 1;
            }
        }
    }
    Ok(removed)
}

/// A commit id as a folder name: 7 to 64 lowercase hexadecimal digits.
fn checked(commit: &str) -> Result<&str, SnapshotError> {
    let ok = (7..=64).contains(&commit.len())
        && commit
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    ok.then_some(commit).ok_or(SnapshotError::BadCommit)
}

/// `/repo/snapshots/<commit>`.
pub fn folder(repo: &Path, commit: &str) -> Result<PathBuf, SnapshotError> {
    Ok(repo.join(SNAPSHOTS).join(checked(commit)?))
}

/// The commit `/repo/current` points at, if any.
pub fn current(repo: &Path) -> Option<String> {
    let target = fs::read_link(repo.join(CURRENT)).ok()?;
    let name = target.file_name()?.to_str()?.to_owned();
    checked(&name).ok().map(str::to_owned)
}

/// Unpacks into `/repo/snapshots/<commit>`, deletes its `exclude` paths, and deletes the folder
/// again if anything fails.
pub fn make(
    repo: &Path,
    commit: &str,
    reader: impl Read,
    cap: u64,
    exclude: &GlobSet,
) -> Result<Unpacked, SnapshotError> {
    let dir = folder(repo, commit)?;
    if dir.exists() {
        fs::remove_dir_all(&dir).map_err(|_| SnapshotError::Write("an old copy of the snapshot"))?;
    }
    let made = unpack(reader, &dir, cap).and_then(|mut done| {
        done.excluded = remove_excluded(&dir, exclude)?;
        done.files -= done.excluded;
        Ok(done)
    });
    if made.is_err() {
        let _ = fs::remove_dir_all(&dir);
    }
    made
}

/// Points `/repo/current` at the commit's snapshot in one rename, then deletes every snapshot but
/// that one and the one it replaced.
pub fn switch(repo: &Path, commit: &str) -> Result<(), SnapshotError> {
    let commit = checked(commit)?;
    let previous = current(repo);
    let temporary = repo.join(".current.tmp");
    let _ = fs::remove_file(&temporary);
    link(&Path::new(SNAPSHOTS).join(commit), &temporary)
        .map_err(|_| SnapshotError::Write("the link to the snapshot"))?;
    fs::rename(&temporary, repo.join(CURRENT)).map_err(|_| {
        let _ = fs::remove_file(&temporary);
        SnapshotError::Write("the link to the snapshot")
    })?;
    let keep = [Some(commit.to_owned()), previous];
    let entries = fs::read_dir(repo.join(SNAPSHOTS)).map_err(|_| SnapshotError::Write("the snapshots"))?;
    for entry in entries.flatten() {
        let name = entry.file_name().to_string_lossy().into_owned();
        if !keep.iter().flatten().any(|kept| *kept == name) {
            fs::remove_dir_all(entry.path()).map_err(|_| SnapshotError::Write("an old snapshot"))?;
        }
    }
    Ok(())
}

/// Deletes the link and every snapshot: the mode dropped to `metadata`.
pub fn clear(repo: &Path) -> Result<(), SnapshotError> {
    match fs::remove_file(repo.join(CURRENT)) {
        Ok(()) => {}
        Err(err) if err.kind() == io::ErrorKind::NotFound => {}
        Err(_) => return Err(SnapshotError::Write("the link to the snapshot")),
    }
    match fs::remove_dir_all(repo.join(SNAPSHOTS)) {
        Ok(()) => Ok(()),
        Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_) => Err(SnapshotError::Write("the snapshots")),
    }
}

#[cfg(unix)]
fn link(target: &Path, at: &Path) -> io::Result<()> {
    std::os::unix::fs::symlink(target, at)
}

#[cfg(windows)]
fn link(target: &Path, at: &Path) -> io::Result<()> {
    std::os::windows::fs::symlink_dir(target, at)
}

/// The reply's chunks as a [`Read`] for the unpacker, which runs on a blocking thread while the
/// reply is read on the runtime: a chunk at a time, `Err` once the stream broke off, the end once
/// the sender is dropped.
pub struct Chunks {
    receiver: Receiver<Result<Vec<u8>, ()>>,
    chunk: Vec<u8>,
    at: usize,
}

/// A channel whose sending half takes the reply's chunks and whose reading half is [`Chunks`].
pub fn chunks() -> (Sender<Result<Vec<u8>, ()>>, Chunks) {
    let (sender, receiver) = tokio::sync::mpsc::channel(CHUNKS_IN_FLIGHT);
    (
        sender,
        Chunks {
            receiver,
            chunk: Vec::new(),
            at: 0,
        },
    )
}

impl Read for Chunks {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        while self.at == self.chunk.len() {
            match self.receiver.blocking_recv() {
                Some(Ok(chunk)) => {
                    self.chunk = chunk;
                    self.at = 0;
                }
                Some(Err(())) => return Err(io::Error::other("the tarball broke off")),
                None => return Ok(0),
            }
        }
        let n = buffer.len().min(self.chunk.len() - self.at);
        buffer[..n].copy_from_slice(&self.chunk[self.at..self.at + n]);
        self.at += n;
        Ok(n)
    }
}

/// Whether `path` is itself a tmpfs mount, by the last line of `/proc/self/mountinfo` that mounts
/// there (a later mount hides an earlier one).
pub fn is_tmpfs(mountinfo: &str, path: &str) -> bool {
    mountinfo.lines().rev().find_map(|line| {
        let (before, after) = line.split_once(" - ")?;
        let point = before.split(' ').nth(4)?;
        let fstype = after.split(' ').next()?;
        (unescape(point) == path).then_some(fstype)
    }) == Some("tmpfs")
}

/// A mount point as `mountinfo` writes it: a space, a tab, a newline and `\` as octal escapes.
fn unescape(field: &str) -> String {
    field
        .replace("\\040", " ")
        .replace("\\011", "\t")
        .replace("\\012", "\n")
        .replace("\\134", "\\")
}

/// `sync` refuses to start unless `/repo` is a tmpfs mount, so no code reaches a disk.
pub fn check_tmpfs(path: &str) -> Result<(), String> {
    let mountinfo = fs::read_to_string("/proc/self/mountinfo").map_err(|_| {
        format!("{path} must be a tmpfs mount, and /proc/self/mountinfo can't be read to check it")
    })?;
    if is_tmpfs(&mountinfo, path) {
        Ok(())
    } else {
        Err(format!(
            "{path} is not a tmpfs mount: the snapshot would reach a disk, so sync doesn't start"
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_path_stays_inside_the_snapshot_or_is_skipped() {
        let cases: [(&[u8], Option<&str>); 8] = [
            (b"owner-repo-abc/src/main.rs", Some("src/main.rs")),
            (b"owner-repo-abc/", Some("")),
            (b"owner-repo-abc/./a//b", Some("a/b")),
            (b"/etc/passwd", None),
            (b"owner-repo-abc/../../etc/passwd", None),
            (b"owner-repo-abc/a/../b", None),
            (b"owner-repo-abc/\xff", None),
            (b"", None),
        ];
        for (raw, expected) in cases {
            assert_eq!(
                inside(raw),
                expected.map(PathBuf::from),
                "{:?}",
                String::from_utf8_lossy(raw)
            );
        }
        // a drive or a backslash would leave the folder on Windows; on Linux they are names
        assert_eq!(inside(b"owner-repo-abc/C:/x").is_none(), cfg!(windows));
        assert_eq!(inside(b"owner-repo-abc/a\\b").is_none(), cfg!(windows));
    }

    #[test]
    fn a_tmpfs_mount_is_told_from_a_disk() {
        let mountinfo = "\
22 1 8:1 / / rw,relatime - ext4 /dev/sda1 rw
36 22 0:41 / /repo rw,nosuid - tmpfs tmpfs rw,size=1048576k,uid=10001
37 22 8:1 /data /data rw - ext4 /dev/sda1 rw
38 22 8:1 /x /with\\040space rw - tmpfs tmpfs rw";
        assert!(is_tmpfs(mountinfo, "/repo"));
        assert!(!is_tmpfs(mountinfo, "/data"));
        assert!(
            !is_tmpfs(mountinfo, "/meta"),
            "not a mount of its own: the disk below it"
        );
        assert!(is_tmpfs(mountinfo, "/with space"));
        let hidden = format!("{mountinfo}\n39 36 8:1 /y /repo rw - ext4 /dev/sda1 rw");
        assert!(
            !is_tmpfs(&hidden, "/repo"),
            "a later mount over it is what is read"
        );
    }

    #[test]
    fn a_commit_id_is_hexadecimal() {
        assert!(checked("0a1b2c3").is_ok());
        assert!(checked(&"a".repeat(40)).is_ok());
        for bad in ["0a1b2c", "../etc", "0A1B2C3", "0a1b2c3/x", ""] {
            assert_eq!(checked(bad), Err(SnapshotError::BadCommit), "{bad}");
        }
    }

    #[test]
    fn chunks_read_in_order_and_a_break_is_an_error() {
        let (sender, mut reader) = chunks();
        sender.blocking_send(Ok(b"ab".to_vec())).unwrap();
        sender.blocking_send(Ok(b"cde".to_vec())).unwrap();
        drop(sender);
        let mut all = Vec::new();
        reader.read_to_end(&mut all).unwrap();
        assert_eq!(all, b"abcde");

        let (sender, mut reader) = chunks();
        sender.blocking_send(Ok(b"ab".to_vec())).unwrap();
        sender.blocking_send(Err(())).unwrap();
        let mut all = Vec::new();
        assert!(reader.read_to_end(&mut all).is_err());
    }
}
