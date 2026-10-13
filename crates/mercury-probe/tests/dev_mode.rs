//! `dev` mode on the fixture repository: what it reads in each access mode.

#![allow(
    clippy::unwrap_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::fs;
use std::path::{Path, PathBuf};

use mercury_probe::probe::Probe;
use mercury_schema::{AccessLimit, AccessMode};

fn fixture() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/repo-delta")
}

fn load(repo: &Path, access: Option<AccessMode>) -> Result<Probe, String> {
    Probe::dev(repo, &repo.join("meta"), access)
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn code_mode_lists_the_files_but_not_the_excluded_ones_or_the_metadata() {
    let probe = load(&fixture(), None).unwrap();
    assert_eq!(probe.manifest.project.as_str(), "delta");
    assert_eq!((probe.access, probe.access_limited_by), (AccessMode::Code, None));
    let files = probe.files.expect("code mode lists the folder");
    // 27 files outside meta/, less the three the manifest's `exclude` globs match
    assert_eq!(files.len(), 24, "{files:#?}");
    assert!(files.iter().any(|f| f == ".mercury/probe.yaml"));
    assert!(files.iter().any(|f| f == "src/Invoices/InvoiceLedgerService.cs"));
    for gone in ["docs/customers/", "deploy/secrets/", ".pem", "meta/"] {
        assert!(!files.iter().any(|f| f.contains(gone)), "{gone} is listed");
    }
}

#[test]
fn metadata_mode_lists_nothing() {
    let probe = load(&fixture(), Some(AccessMode::Metadata)).unwrap();
    assert_eq!(probe.access, AccessMode::Metadata);
    assert_eq!(probe.access_limited_by, Some(AccessLimit::Token));
    assert!(probe.files.is_none());
    // the manifest is still read: its rules apply in every mode
    assert_eq!(probe.manifest.module_labels.len(), 3);
}

/// Every top-level entry but the manifest's folder and the metadata folder is made unreadable:
/// `metadata` mode still starts, so it touched none of them, not even their names.
#[cfg(unix)]
#[test]
fn metadata_mode_reads_none_of_the_folders_files() {
    use std::os::unix::fs::PermissionsExt;

    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo-delta");
    copy_tree(&fixture(), &repo);
    let locked: Vec<PathBuf> = fs::read_dir(&repo)
        .unwrap()
        .map(|e| e.unwrap().path())
        .filter(|p| !p.ends_with(".mercury") && !p.ends_with("meta"))
        .collect();
    for path in &locked {
        fs::set_permissions(path, fs::Permissions::from_mode(0o000)).unwrap();
    }
    // root can read anything; the code-mode half of the check only means something otherwise
    let enforced = fs::read_dir(repo.join("src")).is_err();

    let metadata = load(&repo, Some(AccessMode::Metadata));
    let code = load(&repo, Some(AccessMode::Code));

    for path in &locked {
        fs::set_permissions(path, fs::Permissions::from_mode(0o755)).unwrap();
    }
    let metadata = metadata.unwrap();
    assert!(metadata.files.is_none());
    if enforced {
        assert!(
            code.is_err(),
            "code mode should have tried to list the locked folders"
        );
    }
}

#[test]
fn the_manifest_decides_when_it_is_stricter_than_the_flag() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo-delta");
    copy_tree(&fixture(), &repo);
    let manifest = repo.join(".mercury/probe.yaml");
    let yaml = fs::read_to_string(&manifest).unwrap();
    fs::write(&manifest, yaml.replacen("access: code", "access: metadata", 1)).unwrap();
    let probe = load(&repo, Some(AccessMode::Code)).unwrap();
    assert_eq!(probe.access, AccessMode::Metadata);
    assert_eq!(probe.access_limited_by, Some(AccessLimit::Manifest));
    assert!(probe.files.is_none());
}

#[test]
fn an_invalid_or_missing_manifest_is_refused() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo-delta");
    copy_tree(&fixture(), &repo);
    let manifest = repo.join(".mercury/probe.yaml");
    let yaml = fs::read_to_string(&manifest).unwrap();

    let cases = [
        yaml.replace("blocked_labels", "blocked_label"),
        yaml.replace(r"'(?i)\bmegabank\b'", "'(unclosed'"),
        yaml.replace(r#""**/*.pem""#, r#""src/[unclosed""#),
    ];
    for broken in cases {
        fs::write(&manifest, &broken).unwrap();
        for access in [Some(AccessMode::Metadata), None] {
            let err = load(&repo, access).expect_err("an invalid manifest is refused");
            assert!(err.contains("manifest"), "{err}");
        }
    }
    fs::remove_file(&manifest).unwrap();
    assert!(load(&repo, None).unwrap_err().contains("manifest"));
}

/// A copy of the fixture with `yaml` as its manifest.
fn repo_with_manifest(dir: &Path, yaml: &str) -> PathBuf {
    let repo = dir.join("repo-delta");
    copy_tree(&fixture(), &repo);
    fs::write(repo.join(".mercury/probe.yaml"), yaml).unwrap();
    repo
}

fn fixture_manifest() -> String {
    fs::read_to_string(fixture().join(".mercury/probe.yaml")).unwrap()
}

#[test]
fn exclude_globs_match_regardless_of_case() {
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo-delta");
    copy_tree(&fixture(), &repo);
    // `**/*.pem` and `docs/customers/**` in other cases
    fs::write(repo.join("deploy/Signing.PEM"), "placeholder").unwrap();
    fs::create_dir_all(repo.join("Docs/Customers")).unwrap();
    fs::write(repo.join("Docs/Customers/Notes.md"), "placeholder").unwrap();

    let files = load(&repo, None).unwrap().files.unwrap();
    for gone in ["Signing.PEM", "Notes.md"] {
        assert!(!files.iter().any(|f| f.ends_with(gone)), "{gone} is listed");
    }
    assert_eq!(files.len(), 24, "{files:#?}");
}

#[test]
fn an_invalid_manifest_is_refused_without_quoting_it() {
    let yaml = fixture_manifest();
    // what a slip could put there: a token as the whole file, as a key, or as a value
    let planted = "mcy_plantedtoken_0123456789abcdef";
    let cases = [
        planted.to_owned(),
        format!("{yaml}{planted}: 1\n"),
        yaml.replacen("tracker: github_issues", &format!("tracker: {planted}"), 1),
    ];
    for broken in cases {
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_manifest(dir.path(), &broken);
        for access in [Some(AccessMode::Metadata), None] {
            let err = load(&repo, access).expect_err("an invalid manifest is refused");
            assert!(err.contains("manifest"), "{err}");
            assert!(
                !err.contains("plantedtoken"),
                "the error quotes the manifest: {err}"
            );
        }
    }
}

#[test]
fn a_module_name_the_redaction_would_change_is_refused() {
    let yaml = fixture_manifest();
    // the fixture's redaction turns the client's name into "the client"
    let cases = [
        yaml.replacen(
            r#"  Auth:      ["src/Auth/**"]"#,
            r#"  Megabank Auth: ["src/Auth/**"]"#,
            1,
        ),
        yaml.replacen("  Auth:      [auth]", "  MegaBank: [auth]", 1),
    ];
    for broken in cases {
        assert_ne!(broken, yaml, "the case didn't change the manifest");
        let dir = tempfile::tempdir().unwrap();
        let repo = repo_with_manifest(dir.path(), &broken);
        for access in [Some(AccessMode::Metadata), None] {
            let err = load(&repo, access).expect_err("the module name is refused");
            assert!(err.contains("module"), "{err}");
            assert!(!err.to_lowercase().contains("megabank"), "{err}");
        }
    }
}

/// The listing skips links, so the manifest can't be one: it could point at a token.
#[cfg(unix)]
#[test]
fn a_linked_manifest_is_refused() {
    use std::os::unix::fs::symlink;

    // the manifest a link to a valid manifest outside the folder
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo-delta");
    copy_tree(&fixture(), &repo);
    let outside = dir.path().join("outside.yaml");
    fs::rename(repo.join(".mercury/probe.yaml"), &outside).unwrap();
    symlink(&outside, repo.join(".mercury/probe.yaml")).unwrap();
    for access in [Some(AccessMode::Metadata), None] {
        let err = load(&repo, access).expect_err("a linked manifest is refused");
        assert!(err.contains("manifest"), "{err}");
    }

    // its folder a link
    let dir = tempfile::tempdir().unwrap();
    let repo = dir.path().join("repo-delta");
    copy_tree(&fixture(), &repo);
    let outside = dir.path().join("outside");
    fs::rename(repo.join(".mercury"), &outside).unwrap();
    symlink(&outside, repo.join(".mercury")).unwrap();
    for access in [Some(AccessMode::Metadata), None] {
        let err = load(&repo, access).expect_err("a linked manifest folder is refused");
        assert!(err.contains("manifest"), "{err}");
    }
}

#[test]
fn the_metadata_folder_must_exist() {
    let err = Probe::dev(&fixture(), &fixture().join("no-such-folder"), None).unwrap_err();
    assert!(err.contains("metadata folder"), "{err}");
}
