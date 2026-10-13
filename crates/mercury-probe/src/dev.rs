//! `dev` mode: the probe on a local folder and its metadata, with no network.
//!
//! The manifest is read in every mode. The folder's other files are listed only in `code` mode,
//! so in `metadata` mode nothing reads them, not even their names.

use std::fs;
use std::path::Path;

use globset::{GlobBuilder, GlobSet, GlobSetBuilder};
use mercury_schema::manifest::ProbeManifest;
use mercury_schema::{AccessMode, Redactor};

use crate::access::{self, Switches};
use crate::probe::{self, Probe};

/// The manifest's folder in the repository.
pub const MANIFEST_DIR: &str = ".mercury";

/// Where the manifest sits in the folder.
pub const MANIFEST_PATH: &str = ".mercury/probe.yaml";

impl Probe {
    /// `dev`: reads the folder's manifest, works out the mode, and in `code` mode lists the
    /// folder. An invalid manifest is refused in every mode. Errors never name a file of the
    /// folder.
    pub fn dev(repo: &Path, meta: &Path, token: Option<AccessMode>) -> Result<Self, String> {
        Self::dev_with(repo, meta, token, false)
    }

    /// The same, with whether a code reader is configured: a manifest that says `code_model` runs
    /// `code_model` only then, and `code` otherwise.
    pub fn dev_with(
        repo: &Path,
        meta: &Path,
        token: Option<AccessMode>,
        code_reader: bool,
    ) -> Result<Self, String> {
        if !meta.is_dir() {
            return Err("the metadata folder (--meta) is not a folder".to_owned());
        }
        let (manifest, bytes) = read_manifest(repo)?;
        let (redactor, exclude) = compile(&manifest)?;
        let (access, access_limited_by) = access::effective(Switches {
            token,
            manifest: manifest.access,
            access_max: None,
            code_reader,
        });
        let files = if access >= AccessMode::Code {
            Some(list_files(repo, meta, &exclude)?)
        } else {
            None
        };
        Ok(Self {
            manifest,
            redactor,
            access,
            access_limited_by,
            manifest_hash: probe::sha256(&bytes),
            files,
            repo: repo.to_owned(),
            meta: meta.to_owned(),
            exclude,
        })
    }
}

/// The manifest's redaction rules and `exclude` globs, compiled, after checking that no module
/// name is one its own redaction would change. The errors name a rule's position, never its text.
pub(crate) fn compile(manifest: &ProbeManifest) -> Result<(Redactor, GlobSet), String> {
    let redactor = Redactor::new(&manifest.redact).map_err(|err| format!("the manifest's {err}"))?;
    check_module_names(manifest, &redactor)?;
    let exclude = exclude_globs(&manifest.exclude)?;
    Ok((redactor, exclude))
}

/// The manifest and the bytes it was parsed from.
pub(crate) fn read_manifest(repo: &Path) -> Result<(ProbeManifest, Vec<u8>), String> {
    // The listing skips links, so the manifest can't be one either: it could point at any file,
    // a token say.
    for part in [MANIFEST_DIR, MANIFEST_PATH] {
        let kind = fs::symlink_metadata(repo.join(part))
            .map_err(|err| format!("cannot read the manifest ({MANIFEST_PATH}): {err}"))?
            .file_type();
        if kind.is_symlink() {
            return Err(format!(
                "the manifest ({MANIFEST_PATH}) must be a regular file in the folder, not a link"
            ));
        }
    }
    let bytes = fs::read(repo.join(MANIFEST_PATH))
        .map_err(|err| format!("cannot read the manifest ({MANIFEST_PATH}): {err}"))?;
    let manifest = parse_manifest(&bytes)?;
    Ok((manifest, bytes))
}

/// A manifest from its file's bytes; the error gives the position only.
pub(crate) fn parse_manifest(bytes: &[u8]) -> Result<ProbeManifest, String> {
    let yaml =
        std::str::from_utf8(bytes).map_err(|_| format!("the manifest ({MANIFEST_PATH}) is not UTF-8"))?;
    serde_norway::from_str(yaml).map_err(|err| invalid_manifest(&err))
}

/// Where the manifest failed to parse, never the text there: the parser's own message quotes
/// values and keys, and a slip could have put a token in the file.
fn invalid_manifest(err: &serde_norway::Error) -> String {
    match err.location() {
        Some(at) => format!(
            "the manifest ({MANIFEST_PATH}) is invalid at line {}, column {}",
            at.line(),
            at.column()
        ),
        None => format!("the manifest ({MANIFEST_PATH}) is invalid"),
    }
}

/// A module's name reaches every reply as it is, so a name the manifest's own redaction would
/// change is refused. The error names the module's position, not its name.
fn check_module_names(manifest: &ProbeManifest, redactor: &Redactor) -> Result<(), String> {
    for (key, names) in [
        ("modules", manifest.modules.keys()),
        ("module_labels", manifest.module_labels.keys()),
    ] {
        for (index, name) in names.enumerate() {
            if redactor.apply(name.as_str()) != name.as_str() {
                return Err(format!(
                    "the manifest's module {index} in `{key}` has a name its redaction rules would change"
                ));
            }
        }
    }
    Ok(())
}

fn exclude_globs(patterns: &[String]) -> Result<GlobSet, String> {
    let mut set = GlobSetBuilder::new();
    for (index, pattern) in patterns.iter().enumerate() {
        // on Windows and macOS `Signing.PEM` and `signing.pem` name one file
        let glob = GlobBuilder::new(pattern)
            .case_insensitive(true)
            .build()
            .map_err(|_| format!("the manifest's exclude glob {index} is invalid"))?;
        set.add(glob);
    }
    set.build()
        .map_err(|_| "the manifest's exclude globs are invalid".to_owned())
}

/// Every file under `repo`, leaving out `.git`, the metadata folder when it sits inside, links, and
/// whatever an `exclude` glob matches (a matching folder with everything in it).
pub(crate) fn list_files(repo: &Path, meta: &Path, exclude: &GlobSet) -> Result<Vec<String>, String> {
    let root = fs::canonicalize(repo).map_err(|err| format!("cannot open the repository folder: {err}"))?;
    let meta = fs::canonicalize(meta).map_err(|err| format!("cannot open the metadata folder: {err}"))?;
    let mut files = Vec::new();
    let mut folders = vec![(root.clone(), String::new())];
    while let Some((folder, prefix)) = folders.pop() {
        let entries =
            fs::read_dir(&folder).map_err(|err| format!("cannot list the repository folder: {err}"))?;
        for entry in entries {
            let entry = entry.map_err(|err| format!("cannot list the repository folder: {err}"))?;
            let kind = entry
                .file_type()
                .map_err(|err| format!("cannot list the repository folder: {err}"))?;
            let Some(name) = entry.file_name().to_str().map(str::to_owned) else {
                continue; // a name that isn't UTF-8 matches no glob and no module
            };
            let relative = format!("{prefix}{name}");
            if name == ".git" || kind.is_symlink() || exclude.is_match(&relative) {
                continue;
            }
            if kind.is_dir() {
                if entry.path() != meta {
                    folders.push((entry.path(), format!("{relative}/")));
                }
            } else if kind.is_file() {
                files.push(relative);
            }
        }
    }
    files.sort();
    Ok(files)
}
