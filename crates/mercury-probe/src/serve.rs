//! `serve` mode: the probe on what `sync` wrote. The mode and the manifest come from `/meta`
//! (`access.json`, `manifest.json`), the metadata beside them, and in the code modes the files of
//! the snapshot `/repo/current` points at, once there is one.

use std::fs;
use std::path::Path;

use mercury_schema::{AccessMode, ProjectId};

use crate::dev;
use crate::meta::{self, AccessFile, ManifestFile};
use crate::probe::Probe;

/// The snapshots' volume, read-only for `serve`.
pub const REPO_DIR: &str = "/repo";

/// The metadata volume `sync` writes, read-only for `serve`.
pub const META_DIR: &str = "/meta";

/// The index, the only file `serve` writes.
pub const INDEX_FILE: &str = "/data/index.db";

/// The link to the snapshot in use, under `/repo`.
pub const CURRENT: &str = "current";

impl Probe {
    /// `serve`: reads the mode and the manifest `sync` wrote, checks the manifest as `dev` does, and
    /// in the code modes lists the current snapshot when there is one. `project`, when set, is the
    /// one the manifest must name. Errors name the metadata file, never a file of the snapshot.
    pub fn serve(repo: &Path, meta: &Path, project: Option<&ProjectId>) -> Result<Self, String> {
        let AccessFile {
            access,
            access_limited_by,
        } = meta::read_json(meta, meta::ACCESS_FILE)?;
        let ManifestFile { sha256, manifest, .. } = meta::read_json(meta, meta::MANIFEST_FILE)?;
        if project.is_some_and(|project| project != &manifest.project) {
            return Err(format!(
                "the manifest in the metadata ({}) names another project than PROBE_PROJECT",
                meta::MANIFEST_FILE
            ));
        }
        if !meta.join(meta::GITHUB_FILE).is_file() {
            return Err(format!("the metadata ({}) isn't there yet", meta::GITHUB_FILE));
        }
        let (redactor, exclude) = dev::compile(&manifest)?;
        let current = repo.join(CURRENT);
        let (root, files) = if access >= AccessMode::Code && current.exists() {
            // the snapshot the link points at now; a swap during the listing is caught by the
            // next fingerprint
            let root = fs::canonicalize(&current)
                .map_err(|err| format!("cannot open the current snapshot: {err}"))?;
            let files = dev::list_files(&root, meta, &exclude)?;
            (root, Some(files))
        } else {
            (current, None)
        };
        Ok(Self {
            manifest,
            redactor,
            access,
            access_limited_by,
            manifest_hash: sha256,
            files,
            repo: root,
            meta: meta.to_owned(),
            exclude,
        })
    }
}
