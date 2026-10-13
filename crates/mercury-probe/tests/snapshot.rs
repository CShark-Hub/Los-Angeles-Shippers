//! The snapshot unpacker on gzipped tarballs built here as GitHub builds them (one top folder,
//! a `pax` global header): what it writes, what it skips, the size cap, the `exclude` globs, and
//! the swap of `/repo/current`.

#![allow(
    clippy::unwrap_used,
    reason = "the archives are built here; a panic is the test failing"
)]

use std::fs;
use std::io::Write;
use std::path::Path;

use flate2::Compression;
use flate2::write::GzEncoder;
use globset::{Glob, GlobSet, GlobSetBuilder};
use mercury_probe::sync::snapshot::{self, SnapshotError};
use tar::{EntryType, Header};

const TOP: &str = "owner-repo-0a1b2c3";

/// One entry of a test archive.
enum Entry<'a> {
    File(&'a str, &'a [u8]),
    Dir(&'a str),
    Symlink(&'a str, &'a str),
    HardLink(&'a str, &'a str),
    Fifo(&'a str),
}

fn header(kind: EntryType, size: u64) -> Header {
    let mut header = Header::new_gnu();
    header.set_entry_type(kind);
    header.set_size(size);
    header.set_mode(0o644);
    header
}

/// A gzipped tar of the entries, GitHub's way: a `pax` global header first, then everything under
/// one top folder unless a name starts with `!` (written as it is, after the `!`).
fn tarball(entries: &[Entry<'_>]) -> Vec<u8> {
    let mut builder = tar::Builder::new(GzEncoder::new(Vec::new(), Compression::fast()));
    let comment = b"52 comment=0a1b2c3d4e5f60718293a4b5c6d7e8f901234567\n";
    let mut pax = header(EntryType::XGlobalHeader, comment.len() as u64);
    builder
        .append_data(&mut pax, "pax_global_header", &comment[..])
        .unwrap();
    let mut top = header(EntryType::Directory, 0);
    builder.append_data(&mut top, format!("{TOP}/"), &[][..]).unwrap();
    let name = |path: &str| match path.strip_prefix('!') {
        Some(raw) => raw.to_owned(),
        None => format!("{TOP}/{path}"),
    };
    for entry in entries {
        match entry {
            Entry::File(path, bytes) => {
                let mut h = header(EntryType::Regular, bytes.len() as u64);
                // `append_data` refuses `..`, so the raw name goes into the header itself
                raw_name(&mut h, &name(path));
                h.set_cksum();
                builder.append(&h, *bytes).unwrap();
            }
            Entry::Dir(path) => {
                let mut h = header(EntryType::Directory, 0);
                builder.append_data(&mut h, name(path), &[][..]).unwrap();
            }
            Entry::Symlink(path, target) | Entry::HardLink(path, target) => {
                let kind = if matches!(entry, Entry::Symlink(..)) {
                    EntryType::Symlink
                } else {
                    EntryType::Link
                };
                let mut h = header(kind, 0);
                builder.append_link(&mut h, name(path), target).unwrap();
            }
            Entry::Fifo(path) => {
                let mut h = header(EntryType::Fifo, 0);
                builder.append_data(&mut h, name(path), &[][..]).unwrap();
            }
        }
    }
    builder.into_inner().unwrap().finish().unwrap()
}

fn raw_name(header: &mut Header, name: &str) {
    let gnu = header.as_gnu_mut().unwrap();
    gnu.name = [0; 100];
    gnu.name[..name.len()].copy_from_slice(name.as_bytes());
}

fn globs(patterns: &[&str]) -> GlobSet {
    let mut set = GlobSetBuilder::new();
    for pattern in patterns {
        set.add(Glob::new(pattern).unwrap());
    }
    set.build().unwrap()
}

/// Every file under `root`, by its path with `/`, sorted.
fn files(root: &Path) -> Vec<String> {
    let mut found = Vec::new();
    let mut folders = vec![root.to_owned()];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let entry = entry.unwrap();
            let kind = entry.file_type().unwrap();
            if kind.is_dir() {
                folders.push(entry.path());
            } else {
                let relative = entry
                    .path()
                    .strip_prefix(root)
                    .unwrap()
                    .to_string_lossy()
                    .replace('\\', "/");
                found.push(relative);
            }
        }
    }
    found.sort();
    found
}

#[test]
fn only_folders_and_files_inside_the_snapshot_are_written() {
    let dir = tempfile::tempdir().unwrap();
    let outside = dir.path().join("outside");
    let archive = tarball(&[
        Entry::Dir("src"),
        Entry::File("src/main.rs", b"fn main() {}\n"),
        Entry::File("README.md", b"# Example project\n"),
        Entry::File("deep/a/b/c.txt", b"c"),
        Entry::File("!/tmp/absolute.txt", b"x"),
        Entry::File("../outside/escaped.txt", b"x"),
        Entry::File("src/../../outside/escaped2.txt", b"x"),
        Entry::Symlink("link-out", "../../outside"),
        Entry::Symlink("link-in", "src/main.rs"),
        Entry::HardLink("hard", "src/main.rs"),
        Entry::Fifo("pipe"),
    ]);
    let into = dir.path().join("repo/snapshots/0a1b2c3");
    let done = snapshot::unpack(&archive[..], &into, 1024 * 1024).unwrap();

    assert_eq!(files(&into), ["README.md", "deep/a/b/c.txt", "src/main.rs"]);
    assert_eq!(fs::read(into.join("src/main.rs")).unwrap(), b"fn main() {}\n");
    assert_eq!((done.files, done.skipped), (3, 7), "{done:?}");
    assert_eq!(done.bytes, 13 + 18 + 1);
    assert!(!outside.exists(), "nothing left the snapshot");
}

#[test]
fn above_the_cap_the_unpack_stops_and_make_deletes_what_it_wrote() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let big = vec![b'a'; 700 * 1024];
    let archive = tarball(&[Entry::File("one.bin", &big), Entry::File("two.bin", &big)]);
    let cap = 1024 * 1024;
    assert_eq!(
        snapshot::make(repo, "0a1b2c3", &archive[..], cap, &globs(&[])),
        Err(SnapshotError::TooLarge(1))
    );
    assert!(
        !repo.join("snapshots/0a1b2c3").exists(),
        "the partial snapshot is deleted"
    );
    // the same archive fits a larger cap
    let done = snapshot::make(repo, "0a1b2c3", &archive[..], 2 * cap, &globs(&[])).unwrap();
    assert_eq!((done.files, done.bytes), (2, 2 * 700 * 1024));
}

#[test]
fn a_broken_archive_leaves_nothing_behind() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let archive = tarball(&[Entry::File("a.txt", &[b'x'; 4096])]);
    let cut = &archive[..archive.len() / 2];
    assert_eq!(
        snapshot::make(repo, "0a1b2c3", cut, 1 << 20, &globs(&[])),
        Err(SnapshotError::Unreadable)
    );
    assert!(!repo.join("snapshots/0a1b2c3").exists());
    assert_eq!(
        snapshot::make(repo, "0a1b2c3", &b"not a tarball"[..], 1 << 20, &globs(&[])),
        Err(SnapshotError::Unreadable)
    );
}

#[test]
fn the_exclude_globs_are_deleted_right_after_the_unpack() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    let archive = tarball(&[
        Entry::File("src/app.ts", b"export {}\n"),
        Entry::File("secrets/prod.env", b"KEY=1\n"),
        Entry::File("config/local.settings.json", b"{}\n"),
        Entry::File("config/app.json", b"{}\n"),
    ]);
    let exclude = globs(&["secrets/**", "**/*.settings.json"]);
    let done = snapshot::make(repo, "0a1b2c3", &archive[..], 1 << 20, &exclude).unwrap();
    assert_eq!(
        files(&repo.join("snapshots/0a1b2c3")),
        ["config/app.json", "src/app.ts"]
    );
    assert_eq!((done.files, done.excluded), (2, 2));
}

#[test]
fn a_bad_commit_id_is_no_folder_name() {
    let dir = tempfile::tempdir().unwrap();
    let archive = tarball(&[Entry::File("a.txt", b"a")]);
    assert_eq!(
        snapshot::make(dir.path(), "../x", &archive[..], 1 << 20, &globs(&[])),
        Err(SnapshotError::BadCommit)
    );
}

/// The link swap needs symbolic links, which Windows gives only with a developer setting.
#[cfg(unix)]
#[test]
fn the_switch_is_one_rename_and_two_snapshots_stay() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path();
    for (commit, text) in [("aaaaaaa", "1"), ("bbbbbbb", "2"), ("ccccccc", "3")] {
        let archive = tarball(&[Entry::File("v.txt", text.as_bytes())]);
        snapshot::make(repo, commit, &archive[..], 1 << 20, &globs(&[])).unwrap();
        snapshot::switch(repo, commit).unwrap();
        assert_eq!(snapshot::current(repo).as_deref(), Some(commit));
        assert_eq!(fs::read_to_string(repo.join("current/v.txt")).unwrap(), text);
        assert!(
            fs::read_link(repo.join("current")).unwrap().is_relative(),
            "relative, so serve's read-only mount of the same volume resolves it"
        );
    }
    let mut kept: Vec<String> = fs::read_dir(repo.join("snapshots"))
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    kept.sort();
    assert_eq!(
        kept,
        ["bbbbbbb", "ccccccc"],
        "the new one and the one it replaced"
    );
    assert!(!repo.join(".current.tmp").exists());

    snapshot::clear(repo).unwrap();
    assert!(!repo.join("current").exists() && !repo.join("snapshots").exists());
    assert_eq!(snapshot::current(repo), None);
    snapshot::clear(repo).unwrap();
}

#[test]
fn a_reply_arrives_in_chunks_and_unpacks_as_it_streams() {
    let dir = tempfile::tempdir().unwrap();
    let archive = tarball(&[Entry::File("a.txt", &[b'z'; 100_000])]);
    let (sender, reader) = snapshot::chunks();
    let into = dir.path().join("s");
    let unpacker = std::thread::spawn(move || snapshot::unpack(reader, &into, 1 << 20));
    for chunk in archive.chunks(1000) {
        sender.blocking_send(Ok(chunk.to_vec())).unwrap();
    }
    drop(sender);
    let done = unpacker.join().unwrap().unwrap();
    assert_eq!((done.files, done.bytes), (1, 100_000));
    let mut sink = Vec::new();
    write!(sink, "{done}").unwrap();
    assert_eq!(
        String::from_utf8(sink).unwrap(),
        "1 files (0.1 MB), 0 excluded, 0 entries skipped"
    );
}
