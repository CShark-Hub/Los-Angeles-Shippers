//! The effective access mode: the strictest of what may be read, and what lowered it.

use mercury_schema::{AccessLimit, AccessMode};

/// The switches that cap the mode. `None` is a switch that doesn't apply: `dev` has no git host
/// token (the `--access` flag stands in for one) and no `access_max`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Switches {
    /// What the git host token can read.
    pub token: Option<AccessMode>,
    /// The manifest's `access`; `metadata` when there is no readable manifest.
    pub manifest: AccessMode,
    /// The admin's cap from `projects.yaml`.
    pub access_max: Option<AccessMode>,
    /// Whether a code reader is configured (`PROBE_CODE_READER_URL`): without one, `code_model` runs
    /// as `code`.
    pub code_reader: bool,
}

/// The strictest of the switches and the missing code reader (`code_model` runs as `code`). Below
/// `code`, or below `code_model` when the manifest asks for it, `access_limited_by` names the first
/// switch in the spec's order (token, manifest, `access_max`, then the code reader) that set it.
pub fn effective(switches: Switches) -> (AccessMode, Option<AccessLimit>) {
    let Switches {
        token,
        manifest,
        access_max,
        code_reader,
    } = switches;
    let caps = [
        (AccessLimit::Token, token),
        (AccessLimit::Manifest, Some(manifest)),
        (AccessLimit::AccessMax, access_max),
        (
            AccessLimit::NoCodeReader,
            (!code_reader).then_some(AccessMode::Code),
        ),
    ];
    let mode = caps.iter().filter_map(|&(_, cap)| cap).min().unwrap_or(manifest);
    let limited_by = if mode < manifest.max(AccessMode::Code) {
        caps.iter()
            .find(|&&(_, cap)| cap == Some(mode))
            .map(|&(limit, _)| limit)
    } else {
        None
    };
    (mode, limited_by)
}

/// The mode's name as `describe` and the manifest write it.
pub fn mode_name(mode: AccessMode) -> &'static str {
    match mode {
        AccessMode::Metadata => "metadata",
        AccessMode::Code => "code",
        AccessMode::CodeModel => "code_model",
    }
}

/// What lowered the mode, in words for a log line. `token` is how the caller calls the git host
/// token: `dev` has none, and its `--access` flag stands in for one.
pub fn limit_name(limit: AccessLimit, token: &'static str) -> &'static str {
    match limit {
        AccessLimit::Token => token,
        AccessLimit::Manifest => "the manifest",
        AccessLimit::AccessMax => "access_max",
        AccessLimit::SizeCap => "the size cap",
        AccessLimit::NoCodeReader => "no code reader (PROBE_CODE_READER_URL unset)",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use AccessLimit::{AccessMax, Manifest, NoCodeReader, Token};
    use AccessMode::{Code, CodeModel, Metadata};

    fn mode(
        manifest: AccessMode,
        token: Option<AccessMode>,
        access_max: Option<AccessMode>,
        code_reader: bool,
    ) -> (AccessMode, Option<AccessLimit>) {
        effective(Switches {
            token,
            manifest,
            access_max,
            code_reader,
        })
    }

    #[test]
    fn the_strictest_switch_sets_the_mode_and_is_named() {
        let cases = [
            // manifest, token (or --access), reader, mode, limited by
            (Code, None, false, Code, None),
            (Code, Some(Code), false, Code, None),
            (Code, Some(Metadata), false, Metadata, Some(Token)),
            (Metadata, None, false, Metadata, Some(Manifest)),
            (Metadata, Some(Code), false, Metadata, Some(Manifest)),
            (Metadata, Some(Metadata), false, Metadata, Some(Token)),
            (CodeModel, None, false, Code, Some(NoCodeReader)),
            (CodeModel, Some(Code), false, Code, Some(Token)),
            (CodeModel, Some(Metadata), false, Metadata, Some(Token)),
            // with a reader: only `code_model` changes, and only when nothing else caps it
            (Code, None, true, Code, None),
            (Metadata, None, true, Metadata, Some(Manifest)),
            (CodeModel, None, true, CodeModel, None),
            (CodeModel, Some(CodeModel), true, CodeModel, None),
            (CodeModel, Some(Code), true, Code, Some(Token)),
            (CodeModel, Some(Metadata), true, Metadata, Some(Token)),
        ];
        for (manifest, token, code_reader, expected, limited_by) in cases {
            assert_eq!(
                mode(manifest, token, None, code_reader),
                (expected, limited_by),
                "{manifest:?} with {token:?}, reader {code_reader}"
            );
        }
    }

    #[test]
    fn access_max_comes_after_the_token_and_the_manifest() {
        let cases = [
            // manifest, token, access_max, reader, mode, limited by
            (Code, Some(Code), Some(Code), false, Code, None),
            (Code, Some(Code), Some(CodeModel), false, Code, None),
            (Code, Some(Code), Some(Metadata), false, Metadata, Some(AccessMax)),
            (Code, Some(Metadata), Some(Metadata), false, Metadata, Some(Token)),
            (
                Metadata,
                Some(Code),
                Some(Metadata),
                false,
                Metadata,
                Some(Manifest),
            ),
            (CodeModel, Some(Code), Some(Code), false, Code, Some(Token)),
            (
                CodeModel,
                Some(CodeModel),
                Some(Code),
                false,
                Code,
                Some(AccessMax),
            ),
            (
                CodeModel,
                Some(CodeModel),
                Some(CodeModel),
                false,
                Code,
                Some(NoCodeReader),
            ),
            // with a reader
            (CodeModel, Some(CodeModel), Some(CodeModel), true, CodeModel, None),
            (
                CodeModel,
                Some(CodeModel),
                Some(Code),
                true,
                Code,
                Some(AccessMax),
            ),
            (CodeModel, Some(Code), Some(CodeModel), true, Code, Some(Token)),
            (
                CodeModel,
                Some(CodeModel),
                Some(Metadata),
                true,
                Metadata,
                Some(AccessMax),
            ),
            (Code, Some(CodeModel), Some(CodeModel), true, Code, None),
        ];
        for (manifest, token, access_max, code_reader, expected, limited_by) in cases {
            assert_eq!(
                mode(manifest, token, access_max, code_reader),
                (expected, limited_by),
                "{manifest:?} with {token:?} under {access_max:?}, reader {code_reader}"
            );
        }
    }

    #[test]
    fn the_names_are_the_manifests_and_the_logs() {
        assert_eq!(mode_name(CodeModel), "code_model");
        assert_eq!(mode_name(Code), "code");
        assert_eq!(mode_name(Metadata), "metadata");
        assert_eq!(limit_name(Token, "the token"), "the token");
        assert_eq!(limit_name(Token, "the token (--access)"), "the token (--access)");
        assert_eq!(
            limit_name(NoCodeReader, "the token"),
            "no code reader (PROBE_CODE_READER_URL unset)"
        );
        assert_eq!(limit_name(AccessMax, "the token"), "access_max");
    }
}
