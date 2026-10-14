//! The core's configuration in `MERCURY_CONFIG_DIR`: read at start, where a bad file stops the
//! core, and by `mercury-core admin config check`, which runs the same reading and says what is
//! wrong without starting anything.
//!
//! A problem names the file, the key and the position, never the value written there: the
//! parser's own messages quote values, and a slip could have put a token in a file.

use std::collections::HashMap;
use std::fmt;
use std::path::{Path, PathBuf};

use mercury_judge::Catalogue;
use mercury_schema::config::{ChecksConfig, ModelsConfig, Policies, ProjectsConfig};
use mercury_schema::manifest::ProbeManifest;
use mercury_schema::{CheckId, ProjectId, Redactor};
use serde::de::DeserializeOwned;

/// Where the configuration lives when `MERCURY_CONFIG_DIR` is not set: Compose mounts it there.
pub const DEFAULT_DIR: &str = "/config";

pub const PROJECTS: &str = "projects.yaml";
pub const POLICIES: &str = "policies.yaml";
pub const MODELS: &str = "models.yaml";
pub const CHECKS: &str = "checks.yaml";

/// The configuration directory: `value` (from `MERCURY_CONFIG_DIR`) or the default.
pub fn dir(value: Option<&str>) -> PathBuf {
    PathBuf::from(value.filter(|value| !value.is_empty()).unwrap_or(DEFAULT_DIR))
}

/// Every file of the configuration, read and checked.
#[derive(Debug, Clone)]
pub struct Config {
    pub projects: ProjectsConfig,
    pub policies: Policies,
    pub models: ModelsConfig,
    /// The judge's check catalogue. `read` always gives it, and reports a missing or refused
    /// `checks.yaml` as a problem; `None` is only a configuration built by hand. Nothing may read
    /// `None` as "no checks": a question can't be judged without the catalogue.
    pub checks: Option<ChecksConfig>,
}

/// Something wrong in one file, at one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Problem {
    pub file: &'static str,
    /// The key's path in the file, e.g. `roles.board.classes[1]`; empty for the file as a whole.
    pub key: String,
    /// Line and column, from 1, when the parser knows them.
    pub at: Option<(usize, usize)>,
    pub message: String,
}

impl fmt::Display for Problem {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.file)?;
        if !self.key.is_empty() {
            write!(f, ": {}", self.key)?;
        }
        if let Some((line, column)) = self.at {
            write!(f, " (line {line}, column {column})")?;
        }
        write!(f, ": {}", self.message)
    }
}

/// A secret file the deployment mounts that is missing or empty. It doesn't fail the check: the
/// file arrives separately, and the core fails closed without it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Note {
    pub file: &'static str,
    pub key: String,
    pub message: &'static str,
}

impl fmt::Display for Note {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}: {}", self.file, self.key, self.message)
    }
}

/// What reading the directory found.
#[derive(Debug)]
pub struct Report {
    pub config: Option<Config>,
    pub problems: Vec<Problem>,
    pub notes: Vec<Note>,
}

/// Reads and checks every file in `dir`.
pub fn read(dir: &Path) -> Report {
    let mut problems = Vec::new();
    let projects = parse::<ProjectsConfig>(dir, PROJECTS, &mut problems);
    let policies = parse::<Policies>(dir, POLICIES, &mut problems);
    let models = parse::<ModelsConfig>(dir, MODELS, &mut problems);
    let checks = parse::<ChecksConfig>(dir, CHECKS, &mut problems);
    if let Some(checks) = &checks {
        check_catalogue(checks, &mut problems);
    }
    if let Some(projects) = &projects {
        check_aliases(projects, &mut problems);
        check_manifest_copies(projects, &mut problems);
    }
    if let Some(policies) = &policies {
        check_patterns(POLICIES, "global.redact", &policies.global.redact, &mut problems);
    }
    let mut notes = Vec::new();
    if let Some(projects) = &projects {
        for (id, project) in &projects.projects {
            secret_file(
                PROJECTS,
                format!("projects.{id}.probe.token_file"),
                &project.probe.token_file,
                &mut problems,
                &mut notes,
            );
        }
    }
    if let Some(models) = &models {
        if let Some(openrouter) = &models.backends.openrouter {
            secret_file(
                MODELS,
                "backends.openrouter.key_file".to_owned(),
                &openrouter.key_file,
                &mut problems,
                &mut notes,
            );
        }
        if let Some(path) = models
            .backends
            .openai_compatible
            .as_ref()
            .and_then(|b| b.key_file.as_ref())
        {
            secret_file(
                MODELS,
                "backends.openai_compatible.key_file".to_owned(),
                path,
                &mut problems,
                &mut notes,
            );
        }
    }
    let config = match (projects, policies, models) {
        (Some(projects), Some(policies), Some(models)) if problems.is_empty() => Some(Config {
            projects,
            policies,
            models,
            checks,
        }),
        _ => None,
    };
    Report {
        config,
        problems,
        notes,
    }
}

/// What `config check` prints: one line per problem, then the notes, then a line saying the
/// check found nothing, when it didn't.
pub fn render(report: &Report, dir: &Path, out: &mut dyn std::io::Write) -> std::io::Result<()> {
    for problem in &report.problems {
        writeln!(out, "{problem}")?;
    }
    for note in &report.notes {
        writeln!(out, "note: {note}")?;
    }
    if report.config.is_some() {
        writeln!(out, "ok: 4 files in {}, no problem", dir.display())?;
    }
    Ok(())
}

/// Reads `dir` for the core's start: the configuration, or every problem, one per line.
pub fn load(dir: &Path) -> Result<(Config, Vec<Note>), String> {
    let report = read(dir);
    match report.config {
        Some(config) => Ok((config, report.notes)),
        None => {
            let lines: Vec<String> = report.problems.iter().map(ToString::to_string).collect();
            Err(format!(
                "the configuration in {} has {} problem(s):\n{}",
                dir.display(),
                lines.len(),
                lines.join("\n")
            ))
        }
    }
}

/// The projects `projects.yaml` lists, read on their own: a command that names a project needs
/// these and nothing else, and a slip in another file shouldn't stop it. The file is parsed as the
/// core parses it, so what it lists is what the core starts with; a file that can't be read gives
/// its problems.
pub fn project_ids(dir: &Path) -> Result<Vec<ProjectId>, Vec<Problem>> {
    let mut problems = Vec::new();
    match parse::<ProjectsConfig>(dir, PROJECTS, &mut problems) {
        Some(projects) => Ok(projects.projects.keys().cloned().collect()),
        None => Err(problems),
    }
}

/// `models.yaml` read on its own, as the core parses it: `admin status` shows the backends and the
/// caps from it, and a slip in another file shouldn't hide them. A file that can't be read gives
/// its problems.
pub fn models(dir: &Path) -> Result<ModelsConfig, Vec<Problem>> {
    let mut problems = Vec::new();
    parse::<ModelsConfig>(dir, MODELS, &mut problems).ok_or(problems)
}

/// `projects.yaml` and `policies.yaml` read on their own: `access list` combines them with the
/// manifests to say what each grant lets a person ask, and a slip in the models or the checks
/// shouldn't hide that. Each is parsed as the core parses it; the problems of either, if it can't be
/// read.
pub fn access_inputs(dir: &Path) -> Result<(ProjectsConfig, Policies), Vec<Problem>> {
    let mut problems = Vec::new();
    let projects = parse::<ProjectsConfig>(dir, PROJECTS, &mut problems);
    let policies = parse::<Policies>(dir, POLICIES, &mut problems);
    match (projects, policies) {
        (Some(projects), Some(policies)) => Ok((projects, policies)),
        _ => Err(problems),
    }
}

fn parse<T: DeserializeOwned>(dir: &Path, file: &'static str, problems: &mut Vec<Problem>) -> Option<T> {
    let whole = |message: &str| Problem {
        file,
        key: String::new(),
        at: None,
        message: message.to_owned(),
    };
    let text = match std::fs::read(dir.join(file)) {
        Ok(bytes) => match String::from_utf8(bytes) {
            Ok(text) => text,
            Err(_) => {
                problems.push(whole("the file isn't UTF-8 text"));
                return None;
            }
        },
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {
            problems.push(whole("the file is missing"));
            return None;
        }
        Err(_) => {
            problems.push(whole("the file can't be read"));
            return None;
        }
    };
    match serde_path_to_error::deserialize(serde_norway::Deserializer::from_str(&text)) {
        Ok(value) => Some(value),
        Err(err) => {
            let full = err.path().to_string();
            let at_root = full == ".";
            let inner = err.into_inner();
            let at = inner.location().map(|l| (l.line(), l.column()));
            let (parent, message) = describe(&inner, at_root);
            // The full path names the key itself, but writes a key that is an enum as `?`; the
            // parser's own path names every map key as written, but stops at the map.
            let key = if at_root || full.contains('?') {
                parent
            } else {
                full
            };
            let message = match check_named_by(&text, &key) {
                Some(id) if file == CHECKS => format!("the check `{id}`: {message}"),
                _ => message,
            };
            problems.push(Problem {
                file,
                key,
                at,
                message,
            });
            None
        }
    }
}

/// The id of the check a `checks.yaml` key like `[3].thresholds.block_at` is inside, when it
/// has one of the right shape: the catalogue is a list, and an index alone is hard to find.
fn check_named_by(text: &str, key: &str) -> Option<String> {
    let index: usize = key.strip_prefix('[')?.split(']').next()?.parse().ok()?;
    let catalogue: serde_norway::Value = serde_norway::from_str(text).ok()?;
    let id = catalogue.get(index)?.get("id")?.as_str()?;
    CheckId::parse(id).ok().map(|id| id.to_string())
}

/// The key and the parser's message without the value it quotes. The message comes as `<path>:
/// <text> at line L column C`, the path left out at the top of the file; the path and the position
/// are reported apart, and of the text only what names keys and expected kinds is kept. The
/// parser's own path is used because it names map keys as written (`roles.board`).
fn describe(err: &serde_norway::Error, at_root: bool) -> (String, String) {
    let mut text = err.to_string();
    if let Some(at) = err.location() {
        let suffix = format!(" at line {} column {}", at.line(), at.column());
        if let Some(stripped) = text.strip_suffix(&suffix) {
            text = stripped.to_owned();
        }
    }
    let (key, text) = match text.split_once(": ") {
        Some((path, rest)) if !at_root && is_path(path) => {
            (path.trim_start_matches('.').to_owned(), rest.to_owned())
        }
        _ => (String::new(), text),
    };
    (key, sanitize(&text))
}

/// The parser's path: keys and indexes, never one of its own message openings.
fn is_path(text: &str) -> bool {
    let opening = ["invalid ", "unknown ", "missing ", "duplicate "]
        .iter()
        .any(|o| text.starts_with(o));
    !opening && !text.contains(['`', '"', '\''])
}

fn sanitize(text: &str) -> String {
    // The value comes before `expected`; keep only what was expected.
    for prefix in [
        "invalid type: ",
        "invalid value: ",
        "invalid length ",
        "unknown variant ",
    ] {
        if text.starts_with(prefix) {
            return match text.split_once(", expected ") {
                Some((_, expected)) => format!("expected {expected}"),
                None => "a value of the wrong kind".to_owned(),
            };
        }
    }
    // Messages that quote only key names.
    for (from, to) in [
        ("unknown field ", "unknown key "),
        ("missing field ", "missing key "),
        ("duplicate field ", "duplicate key "),
    ] {
        if let Some(rest) = text.strip_prefix(from) {
            return format!("{to}{rest}");
        }
    }
    // Every other message is the schema's own or the YAML parser's, and quotes no value; a
    // backtick or a quote that still slips through is cut off with what follows.
    match text.find(['"', '\'']) {
        Some(at) => format!("{}…", text[..at].trim_end()),
        None => text.to_owned(),
    }
}

/// The catalogue as the judge loads it at the core's start: no check it doesn't know, each with the
/// stage, type and polarity it has, its thresholds in order, and none of the table missing. The
/// file's own parse misses these, and the core refuses to start on them, so `config check` must too.
/// The message names the check, never its instructions.
fn check_catalogue(checks: &ChecksConfig, problems: &mut Vec<Problem>) {
    if let Err(err) = Catalogue::new(checks.clone()) {
        problems.push(Problem {
            file: CHECKS,
            key: String::new(),
            at: None,
            message: err.to_string(),
        });
    }
}

/// An alias is how people name a project; one that is another project's id or alias would make
/// the judge's project choice ambiguous.
fn check_aliases(projects: &ProjectsConfig, problems: &mut Vec<Problem>) {
    let mut owners: HashMap<String, &ProjectId> = projects
        .projects
        .keys()
        .map(|id| (id.as_str().to_lowercase(), id))
        .collect();
    for (id, project) in &projects.projects {
        for (index, alias) in project.aliases.iter().enumerate() {
            let alias = alias.trim().to_lowercase();
            let empty = alias.is_empty();
            let clash = match owners.get(&alias) {
                Some(owner) => *owner != id,
                None => {
                    owners.insert(alias, id);
                    false
                }
            };
            if clash || empty {
                problems.push(Problem {
                    file: PROJECTS,
                    key: format!("projects.{id}.aliases[{index}]"),
                    at: None,
                    message: if clash {
                        "the alias is another project's id or alias".to_owned()
                    } else {
                        "an alias can't be empty".to_owned()
                    },
                });
            }
        }
    }
}

/// A copy follows the manifest's own load rules: its patterns compile, and no module name is one
/// its redaction would change.
fn check_manifest_copies(projects: &ProjectsConfig, problems: &mut Vec<Problem>) {
    for (id, project) in &projects.projects {
        let Some(copy) = &project.manifest else {
            continue;
        };
        let base = format!("projects.{id}.manifest");
        check_patterns(PROJECTS, &format!("{base}.redact"), &copy.redact, problems);
        let Ok(redactor) = Redactor::new(&copy.redact) else {
            continue;
        };
        for (key, names) in [
            ("modules", copy.modules.keys()),
            ("module_labels", copy.module_labels.keys()),
        ] {
            for (index, name) in names.enumerate() {
                if redactor.apply(name.as_str()) != name.as_str() {
                    problems.push(Problem {
                        file: PROJECTS,
                        key: format!("{base}.{key}"),
                        at: None,
                        message: format!("module {index} has a name the copy's redaction would change"),
                    });
                }
            }
        }
    }
}

fn check_patterns(
    file: &'static str,
    key: &str,
    rules: &[mercury_schema::redact::RedactRule],
    problems: &mut Vec<Problem>,
) {
    for (index, rule) in rules.iter().enumerate() {
        if Redactor::new(std::slice::from_ref(rule)).is_err() {
            problems.push(Problem {
                file,
                key: format!("{key}[{index}].pattern"),
                at: None,
                message: "the pattern is not a valid regular expression".to_owned(),
            });
        }
    }
}

/// A secret file's path must be absolute, so it doesn't depend on where the core started; a
/// missing or empty file is a note.
fn secret_file(
    file: &'static str,
    key: String,
    path: &Path,
    problems: &mut Vec<Problem>,
    notes: &mut Vec<Note>,
) {
    if !path.has_root() {
        problems.push(Problem {
            file,
            key,
            at: None,
            message: "the path must be absolute".to_owned(),
        });
        return;
    }
    let message = match std::fs::metadata(path) {
        Ok(meta) if meta.is_file() && meta.len() > 0 => return,
        Ok(meta) if meta.is_file() => "the file is empty",
        Ok(_) => "the path is not a file",
        Err(_) => "the file isn't there",
    };
    notes.push(Note { file, key, message });
}

/// The manifest a project's policy reads: the one its probe registered combined with the admin's
/// copy in `projects.yaml`, the stricter winning ([`crate::policy::combine`]), or whichever of
/// the two exists. `None`: the project can't be asked.
pub fn manifest_of(
    projects: &ProjectsConfig,
    project: &ProjectId,
    registered: Option<&ProbeManifest>,
) -> Option<ProbeManifest> {
    let copy = projects.projects.get(project).and_then(|config| {
        config
            .manifest
            .clone()
            .map(|copy| copy.into_manifest(project.clone(), config.display_name.clone()))
    });
    match (registered, copy) {
        (Some(registered), Some(copy)) => Some(crate::policy::combine(registered, &copy)),
        (Some(registered), None) => Some(registered.clone()),
        (None, copy) => copy,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn examples() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../config-examples")
    }

    /// The examples copied into a fresh directory, with `edit` applied to one file.
    fn planted(file: &str, edit: impl FnOnce(String) -> String) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut edit = Some(edit);
        for name in [PROJECTS, POLICIES, MODELS, CHECKS] {
            // the checkout may have CRLF line endings; the edits below are written with LF
            let text = std::fs::read_to_string(examples().join(name))
                .unwrap()
                .replace("\r\n", "\n");
            let text = match edit.take_if(|_| name == file) {
                Some(edit) => edit(text),
                None => text,
            };
            std::fs::write(dir.path().join(name), text).unwrap();
        }
        dir
    }

    fn problems(file: &str, edit: impl FnOnce(String) -> String) -> Vec<String> {
        let dir = planted(file, edit);
        read(dir.path())
            .problems
            .iter()
            .map(ToString::to_string)
            .collect()
    }

    #[test]
    fn the_examples_have_no_problem() {
        let report = read(&examples());
        assert_eq!(report.problems, []);
        let config = report.config.unwrap();
        assert_eq!(config.projects.projects.len(), 1);
        // the judge's catalogue, every check of it
        assert_eq!(config.checks.map(|c| c.0.len()), Some(18));
        // The token and key files are mounted by a deployment, not kept with the examples.
        let keys: Vec<_> = report.notes.iter().map(|n| n.key.as_str()).collect();
        assert_eq!(
            keys,
            ["projects.delta.probe.token_file", "backends.openrouter.key_file"]
        );
    }

    #[test]
    fn an_unknown_key_names_the_file_and_the_key() {
        let found = problems(POLICIES, |t| {
            t.replace("aggregates_only: true", "aggregate_only: true")
        });
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].starts_with("policies.yaml: roles.board"), "{found:?}");
        assert!(found[0].contains("unknown key `aggregate_only`"), "{found:?}");
        assert!(found[0].contains("(line "), "{found:?}");
    }

    #[test]
    fn a_wrong_value_is_never_quoted() {
        let secret = "mcy_PLANTEDsecret_value";
        let found = problems(PROJECTS, |t| {
            t.replace("access_max: code ", &format!("access_max: {secret} "))
        });
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(found[0].contains("projects.delta.access_max"), "{found:?}");
        assert!(found[0].contains("expected one of"), "{found:?}");
        assert!(!found[0].contains("PLANTED"), "{found:?}");
        let found = problems(POLICIES, |t| t.replace("burst: 5", &format!("burst: {secret}")));
        assert!(
            found[0].contains("limits.burst") && !found[0].contains("PLANTED"),
            "{found:?}"
        );
    }

    #[test]
    fn a_missing_file_is_a_problem() {
        let dir = planted(MODELS, |t| t);
        std::fs::remove_file(dir.path().join(MODELS)).unwrap();
        let report = read(dir.path());
        assert_eq!(report.problems[0].to_string(), "models.yaml: the file is missing");
        assert!(report.config.is_none());
    }

    /// The core can't judge a question without the catalogue, and it refuses to start without it:
    /// a configuration `config check` passes is one the core starts with.
    #[test]
    fn a_missing_catalogue_is_a_problem() {
        let dir = planted(MODELS, |t| t);
        std::fs::remove_file(dir.path().join(CHECKS)).unwrap();
        let report = read(dir.path());
        assert_eq!(report.problems.len(), 1, "{:?}", report.problems);
        assert_eq!(report.problems[0].to_string(), "checks.yaml: the file is missing");
        assert!(report.config.is_none());
        assert!(load(dir.path()).unwrap_err().contains("checks.yaml"));
    }

    /// What the check catalogue's loader refuses at the core's start, `config check` refuses too:
    /// the file parses, and the catalogue is still not one the core would run on.
    #[test]
    fn a_catalogue_the_loader_refuses_is_a_problem() {
        let found = problems(CHECKS, |t| {
            t.replace(
                "{ block_at: 0.8, review_at: 0.7 }",
                "{ block_at: 0.2, review_at: 0.8 }",
            )
        });
        assert_eq!(
            found,
            ["checks.yaml: `sentence_contains_code_or_identifier` has thresholds in the wrong order"]
        );

        let found = problems(CHECKS, |t| {
            t.replace("- id: grounded\n", "- id: grounded_enough\n")
        });
        assert_eq!(
            found,
            ["checks.yaml: `grounded_enough` isn't a check Mercury knows"]
        );

        let found = problems(CHECKS, |t| {
            t.replace(
                "  stage: output\n  type: noul\n  polarity: legit",
                "  stage: input\n  type: noul\n  polarity: legit",
            )
        });
        assert_eq!(found, ["checks.yaml: `grounded` has the wrong stage"]);

        // a check missing from the table, whole
        let found = problems(CHECKS, |t| {
            let at = t.find("- id: grounded\n").unwrap();
            t[..at].to_owned()
        });
        assert_eq!(found, ["checks.yaml: the catalogue doesn't list `grounded`"]);

        // the problem says which check, never what its instructions say
        let found = problems(CHECKS, |t| {
            t.replace(
                "{ block_at: 0.8, review_at: 0.7 }",
                "{ block_at: 0.2, review_at: 0.8 }",
            )
        });
        assert!(!found[0].contains("source code"), "{found:?}");
    }

    #[test]
    fn a_bad_catalogue_is_a_problem() {
        let dir = planted(MODELS, |t| t);
        std::fs::write(
            dir.path().join(CHECKS),
            "- id: scope\n  stage: planner\n  type: rule\n",
        )
        .unwrap();
        let found: Vec<String> = read(dir.path())
            .problems
            .iter()
            .map(ToString::to_string)
            .collect();
        assert!(found[0].starts_with("checks.yaml: [0].stage"), "{found:?}");
    }

    #[test]
    fn a_threshold_out_of_range_names_the_check_and_the_key() {
        let dir = planted(MODELS, |t| t);
        let catalogue = "- id: class
  stage: input
  type: choice
- id: asks_for_code
  stage: input
  type: noul
  polarity: risk
  thresholds: { block_at: 1.4 }
";
        std::fs::write(dir.path().join(CHECKS), catalogue).unwrap();
        let found: Vec<String> = read(dir.path())
            .problems
            .iter()
            .map(ToString::to_string)
            .collect();
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].starts_with("checks.yaml: [1].thresholds.block_at"),
            "{found:?}"
        );
        assert!(found[0].contains("the check `asks_for_code`"), "{found:?}");
        assert!(!found[0].contains("1.4"), "{found:?}");
    }

    #[test]
    fn the_links_between_keys_are_checked() {
        let found = problems(MODELS, |t| {
            t.replace(
                "backend: openrouter, kind: jev",
                "backend: openai_compatible, kind: jev",
            )
        });
        assert!(
            found[0].starts_with("models.yaml: ") && found[0].contains("openrouter backend"),
            "{found:?}"
        );
        let omega = "  omega:\n    display_name: \"Omega\"\n    aliases: [invoicing]\n    access_max: metadata\n    \
                     probe:\n      url: http://probe-omega:7000/mcp\n      token_file: /run/mercury/probe-tokens/omega.token\n";
        let found = problems(PROJECTS, |t| {
            t.replace(
                "  # A project whose probe",
                &format!("{omega}  # A project whose probe"),
            )
        });
        assert_eq!(
            found,
            ["projects.yaml: projects.omega.aliases[0]: the alias is another project's id or alias"]
        );
        let found = problems(POLICIES, |t| {
            t.replace(r"'(?i)\bacme(\s+corp)?\b'", "'(unclosed'")
        });
        assert_eq!(
            found,
            ["policies.yaml: global.redact[0].pattern: the pattern is not a valid regular expression"]
        );
    }

    #[test]
    fn a_copy_follows_the_manifest_rules() {
        let copy = "    manifest:\n      module_labels: { Acme Portal: [portal] }\n      allow: [progress]\n      redact:\n        - { pattern: '(?i)acme', replace: 'the client' }\n      judge_context: []\n";
        let found = problems(PROJECTS, |t| {
            t.replace(
                "    probe:\n      url: http://probe-delta",
                &format!("{copy}    probe:\n      url: http://probe-delta"),
            )
        });
        assert_eq!(
            found,
            [
                "projects.yaml: projects.delta.manifest.module_labels: module 0 has a name the copy's redaction would change"
            ]
        );
    }

    #[test]
    fn a_relative_secret_path_is_a_problem() {
        let found = problems(PROJECTS, |t| {
            t.replace(
                "token_file: /run/mercury/probe-tokens/delta.token",
                "token_file: delta.token",
            )
        });
        assert_eq!(
            found,
            ["projects.yaml: projects.delta.probe.token_file: the path must be absolute"]
        );
    }

    #[test]
    fn a_secret_file_that_is_there_needs_no_note() {
        let tokens = tempfile::tempdir().unwrap();
        let token = tokens.path().join("delta.token");
        std::fs::write(&token, "x").unwrap();
        let dir = planted(PROJECTS, |t| {
            t.replace(
                "/run/mercury/probe-tokens/delta.token",
                &token.display().to_string().replace('\\', "/"),
            )
        });
        let report = read(dir.path());
        assert_eq!(report.problems, []);
        assert_eq!(report.notes.len(), 1, "only the model key: {:?}", report.notes);
    }

    #[test]
    fn the_message_never_keeps_a_quoted_value() {
        assert_eq!(
            sanitize("invalid type: string \"hunter2\", expected u32"),
            "expected u32"
        );
        assert_eq!(
            sanitize("unknown variant `hunter2`, expected one of `metadata`, `code`"),
            "expected one of `metadata`, `code`"
        );
        assert_eq!(sanitize("missing field `limits`"), "missing key `limits`");
        assert_eq!(sanitize("something \"hunter2\" happened"), "something…");
    }

    #[test]
    fn the_registered_manifest_comes_before_the_copy() {
        let config = read(&examples()).config.unwrap();
        let delta = ProjectId::parse("delta").unwrap();
        assert!(
            manifest_of(&config.projects, &delta, None).is_none(),
            "no copy for delta"
        );
        let other = ProjectId::parse("omega").unwrap();
        assert!(manifest_of(&config.projects, &other, None).is_none());
    }

    #[test]
    fn a_registered_manifest_and_the_copy_combine_and_either_alone_stands() {
        let copy = "    manifest:\n      allow: [progress, tested]\n      deny: [tested]\n      redact:\n        - { pattern: '(?i)omega bank', replace: 'the client' }\n      signoff: [changes]\n      judge_context: [labels]\n";
        let dir = planted(PROJECTS, |t| {
            t.replace(
                "    probe:\n      url: http://probe-delta",
                &format!("{copy}    probe:\n      url: http://probe-delta"),
            )
        });
        let config = read(dir.path()).config.unwrap();
        let delta = ProjectId::parse("delta").unwrap();
        let fixture =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("../../tests/fixtures/repo-delta/.mercury/probe.yaml");
        let registered: ProbeManifest =
            serde_norway::from_str(&std::fs::read_to_string(fixture).unwrap().replace("\r\n", "\n")).unwrap();

        let alone = manifest_of(&config.projects, &delta, None).unwrap();
        assert_eq!(alone.allow.len(), 2, "the copy alone, before the probe registers");
        let both = manifest_of(&config.projects, &delta, Some(&registered)).unwrap();
        assert_eq!(both, crate::policy::combine(&registered, &alone));
        assert_eq!(both.allow, registered.allow, "the registered manifest's classes");
        assert_eq!(both.redact.len(), 2, "both lists' rules");
        assert_eq!(both.signoff.len(), 1);
        assert_eq!(both.judge_context.len(), 1, "labels only");

        let examples = read(&examples()).config.unwrap();
        assert_eq!(
            manifest_of(&examples.projects, &delta, Some(&registered)),
            Some(registered),
            "no copy: the registered manifest as it is"
        );
    }

    #[test]
    fn the_core_stops_with_one_line_per_problem() {
        let dir = planted(POLICIES, |t| {
            t.replace("version: 1", "version: 2")
                .replace("burst: 5", "burst: 0")
        });
        let err = load(dir.path()).unwrap_err();
        assert!(err.contains("1 problem(s)"), "{err}");
        assert!(err.contains("policies.yaml"), "{err}");
    }

    #[test]
    fn a_retention_of_zero_days_is_a_problem() {
        let found = problems(POLICIES, |t| {
            t.replace("audit_payload_days: 90", "audit_payload_days: 0")
        });
        assert_eq!(found.len(), 1, "{found:?}");
        assert!(
            found[0].contains("policies.yaml") && found[0].contains("0 days"),
            "{found:?}"
        );
        let report = read(&examples());
        assert_eq!(report.config.unwrap().policies.retention.audit_payload_days, 90);
    }
}
