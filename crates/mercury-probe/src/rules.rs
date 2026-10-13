//! From signals to facts, by rules alone: the module, the status, `tested`, `waiting_on` and the
//! title. Each rule is a pure function over signals, so the table tests can name every row.

use globset::{Glob, GlobSet, GlobSetBuilder};
use mercury_schema::facts::{Kind, Status, Tested, WaitingOn};
use mercury_schema::manifest::ProbeManifest;
use mercury_schema::redact::{safe_text, withheld_title};
use mercury_schema::{ModuleName, Redactor, SafeText};

/// How the manifest maps files and labels to modules.
#[derive(Debug)]
pub struct ModuleMap {
    /// In manifest order, each module's globs.
    globs: Vec<(ModuleName, GlobSet)>,
    /// In manifest order, each module's labels, lower case, its own name among them.
    labels: Vec<(ModuleName, Vec<String>)>,
}

impl ModuleMap {
    pub fn new(manifest: &ProbeManifest) -> Result<Self, String> {
        let mut globs = Vec::new();
        for (index, (module, patterns)) in manifest.modules.iter().enumerate() {
            let mut set = GlobSetBuilder::new();
            for pattern in patterns {
                let glob = Glob::new(pattern)
                    .map_err(|_| format!("the manifest's module {index} has an invalid glob"))?;
                set.add(glob);
            }
            let set = set
                .build()
                .map_err(|_| format!("the manifest's module {index} has invalid globs"))?;
            globs.push((module.clone(), set));
        }
        // a label equal to a module's name maps to it too, so every module with globs or labels
        // takes its own name as a label
        let mut labels: Vec<(ModuleName, Vec<String>)> = Vec::new();
        for module in manifest.modules.keys().chain(manifest.module_labels.keys()) {
            if !labels.iter().any(|(m, _)| m == module) {
                labels.push((module.clone(), vec![module.as_str().to_lowercase()]));
            }
        }
        for (module, names) in &manifest.module_labels {
            if let Some((_, known)) = labels.iter_mut().find(|(m, _)| m == module) {
                known.extend(names.iter().map(|n| n.to_lowercase()));
            }
        }
        Ok(Self { globs, labels })
    }

    /// The modules a reply may name, in manifest order, then `Other`.
    pub fn modules(&self) -> Vec<ModuleName> {
        let mut modules: Vec<ModuleName> = self.labels.iter().map(|(m, _)| m.clone()).collect();
        modules.push(ModuleName::other());
        modules
    }

    /// The first module whose globs match the file, or `None` for `Other`.
    pub fn module_of_file(&self, path: &str) -> Option<&ModuleName> {
        self.globs
            .iter()
            .find(|(_, set)| set.is_match(path))
            .map(|(module, _)| module)
    }

    /// The module with the most of these files; ties go to the first in manifest order, and
    /// `Other` only when no file matches a module.
    pub fn module_of_files<'a>(&self, paths: impl IntoIterator<Item = &'a str>) -> ModuleName {
        let mut counts = vec![0usize; self.globs.len()];
        for path in paths {
            if let Some(i) = self.globs.iter().position(|(_, set)| set.is_match(path)) {
                counts[i] += 1;
            }
        }
        let best = counts
            .iter()
            .enumerate()
            .filter(|&(_, &n)| n > 0)
            // max_by_key keeps the last of equals: reverse so the first in manifest order wins
            .rev()
            .max_by_key(|&(_, &n)| n)
            .map(|(i, _)| i);
        best.map_or_else(ModuleName::other, |i| self.globs[i].0.clone())
    }

    /// The first module in manifest order with one of these labels, case ignored, or `Other`.
    pub fn module_of_labels<'a>(&self, labels: impl IntoIterator<Item = &'a str> + Clone) -> ModuleName {
        self.labels
            .iter()
            .find(|(_, names)| {
                labels
                    .clone()
                    .into_iter()
                    .any(|label| names.contains(&label.to_lowercase()))
            })
            .map_or_else(ModuleName::other, |(module, _)| module.clone())
    }
}

/// A pull request's state, as the status rules see it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrState {
    Merged,
    ClosedUnmerged,
    Open {
        draft: bool,
        review_requested: bool,
        recent: bool,
    },
}

/// What the status rules read about one item.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Signals {
    Pull {
        state: PrState,
        /// Linked issues: any closed, any open with a blocking label.
        linked_closed: bool,
        linked_blocked: bool,
    },
    Issue {
        open: bool,
        blocked: bool,
    },
}

/// The first rule that matches. A conflict is `unclear` until the core's judge chooses
/// ([`crate::judging`]).
pub fn status(signals: Signals) -> (Status, f64) {
    match signals {
        Signals::Pull {
            state: PrState::Merged,
            linked_closed,
            ..
        } => (Status::Done, if linked_closed { 0.95 } else { 0.8 }),
        Signals::Pull {
            state: PrState::Open { .. },
            linked_blocked: true,
            ..
        } => CONFLICT,
        Signals::Pull {
            state:
                PrState::Open {
                    draft: false,
                    review_requested: true,
                    ..
                },
            ..
        } => (Status::InReview, 0.85),
        Signals::Pull {
            state: PrState::Open { draft: true, .. } | PrState::Open { recent: true, .. },
            ..
        } => (Status::InProgress, 0.7),
        Signals::Issue {
            open: true,
            blocked: true,
        } => (Status::Blocked, 0.9),
        Signals::Issue {
            open: true,
            blocked: false,
        } => (Status::NotStarted, 0.7),
        _ => UNCLEAR,
    }
}

/// Signals that point two ways, until the judge chooses.
pub const CONFLICT: (Status, f64) = (Status::Unclear, 0.5);
/// No rule matched.
pub const UNCLEAR: (Status, f64) = (Status::Unclear, 0.5);

/// What a merged PR's merge commit's latest run came to.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MergeRun {
    Passed,
    Failed,
    /// Running, or no run, or not merged.
    None,
}

/// `tested` for one fact. `module_tests` is `None` in `metadata` mode, where tests aren't counted.
pub fn tested(merge_run: MergeRun, module_tests: Option<u32>) -> Tested {
    match (module_tests, merge_run) {
        (Some(0), _) => Tested::Missing,
        (_, MergeRun::Failed) => Tested::Failing,
        (_, MergeRun::Passed) => Tested::Passing,
        _ => Tested::Unknown,
    }
}

/// What a blocked item waits on, from its first blocking label.
pub fn waiting_on(label: &str) -> WaitingOn {
    let label = label.to_lowercase();
    let has = |words: &[&str]| words.iter().any(|w| label.contains(w));
    if has(&["client", "customer", "vendor", "external"]) {
        WaitingOn::External
    } else if has(&["decision"]) {
        WaitingOn::Decision
    } else if has(&["environment", "infra", "staging"]) {
        WaitingOn::Environment
    } else if has(&["dependency", "upstream"]) {
        WaitingOn::Dependency
    } else if has(&["review"]) {
        WaitingOn::Review
    } else {
        WaitingOn::Unknown
    }
}

/// The label words that name a kind, case ignored.
const KIND_LABELS: [(Kind, &[&str]); 3] = [
    (
        Kind::Fix,
        &["bug", "fix", "bugfix", "hotfix", "defect", "regression"],
    ),
    (Kind::Feature, &["feature", "feat", "enhancement", "story"]),
    (
        Kind::Chore,
        &[
            "chore",
            "docs",
            "documentation",
            "ci",
            "build",
            "refactor",
            "refactoring",
            "dependencies",
            "deps",
            "maintenance",
            "tech-debt",
        ],
    ),
];

/// The kind the labels name: `Some` when they name exactly one, `None` when they name none or
/// two, which leaves the kind to the judge.
pub fn kind_of_labels<'a>(labels: impl IntoIterator<Item = &'a str>) -> Option<Kind> {
    let mut named = Vec::new();
    for label in labels {
        let label = label.trim().to_lowercase();
        for (kind, words) in KIND_LABELS {
            if words.contains(&label.as_str()) && !named.contains(&kind) {
                named.push(kind);
            }
        }
    }
    match named.as_slice() {
        [kind] => Some(*kind),
        _ => None,
    }
}

/// The first of these labels the manifest counts as blocking, case ignored.
pub fn blocking_label<'a>(
    manifest: &ProbeManifest,
    labels: impl IntoIterator<Item = &'a str>,
) -> Option<&'a str> {
    labels.into_iter().find(|label| {
        manifest
            .blocked_labels
            .iter()
            .any(|blocked| blocked.eq_ignore_ascii_case(label))
    })
}

/// The title as it may leave, or `Work item in <module>` and `true` when it fails a rule.
pub fn title(raw: &str, module: &ModuleName, redactor: &Redactor) -> (SafeText, bool) {
    match safe_text(raw, redactor) {
        Ok(text) => (text, false),
        Err(_) => (withheld_title(module), true),
    }
}

#[cfg(test)]
mod tests {
    use mercury_schema::redact::RedactRule;

    use super::*;

    fn manifest() -> ProbeManifest {
        serde_norway::from_str(
            r#"
version: 0.1
project: delta
display_name: Delta
modules:
  Invoicing: ["src/Invoices/**", "src/Billing/**"]
  Export: ["src/Export/**"]
  Auth: ["src/Auth/**"]
module_labels:
  Invoicing: [invoicing, billing]
  Export: [export]
blocked_labels: [blocked, "Waiting: client"]
allow: [progress]
judge_context: [titles]
"#,
        )
        .unwrap()
    }

    #[test]
    fn status_rules_one_row_each() {
        use PrState::*;
        use Status::*;
        let open = |draft, review_requested, recent| Open {
            draft,
            review_requested,
            recent,
        };
        let pull = |state, linked_closed, linked_blocked| Signals::Pull {
            state,
            linked_closed,
            linked_blocked,
        };
        let cases = [
            // the table in order: signals → status, confidence
            (pull(Merged, true, false), (Done, 0.95)),
            (pull(Merged, false, false), (Done, 0.8)),
            (pull(Merged, false, true), (Done, 0.8)),
            (pull(open(false, true, true), false, true), CONFLICT),
            (pull(open(true, false, true), false, true), CONFLICT),
            (pull(open(false, true, false), false, false), (InReview, 0.85)),
            (pull(open(true, true, false), false, false), (InProgress, 0.7)),
            (pull(open(true, false, false), false, false), (InProgress, 0.7)),
            (pull(open(false, false, true), false, false), (InProgress, 0.7)),
            (pull(open(false, false, false), false, false), UNCLEAR),
            (pull(ClosedUnmerged, true, false), UNCLEAR),
            (
                Signals::Issue {
                    open: true,
                    blocked: true,
                },
                (Blocked, 0.9),
            ),
            (
                Signals::Issue {
                    open: true,
                    blocked: false,
                },
                (NotStarted, 0.7),
            ),
            (
                Signals::Issue {
                    open: false,
                    blocked: true,
                },
                UNCLEAR,
            ),
            (
                Signals::Issue {
                    open: false,
                    blocked: false,
                },
                UNCLEAR,
            ),
        ];
        for (signals, expected) in cases {
            assert_eq!(status(signals), expected, "{signals:?}");
        }
    }

    #[test]
    fn tested_rules_in_order_in_both_modes() {
        use MergeRun as R;
        use Tested::*;
        let cases = [
            // code mode: counted tests
            (R::Passed, Some(0), Missing),
            (R::Failed, Some(0), Missing),
            (R::Failed, Some(3), Failing),
            (R::Passed, Some(3), Passing),
            (R::None, Some(3), Unknown),
            (R::None, Some(0), Missing),
            // metadata mode: CI alone, never missing
            (R::Passed, None, Passing),
            (R::Failed, None, Failing),
            (R::None, None, Unknown),
        ];
        for (run, tests, expected) in cases {
            assert_eq!(tested(run, tests), expected, "{run:?} with {tests:?}");
        }
    }

    #[test]
    fn waiting_on_from_the_label_words() {
        use WaitingOn::*;
        for (label, expected) in [
            ("waiting: client", External),
            ("Waiting on customer", External),
            ("waiting: decision", Decision),
            ("waiting: environment", Environment),
            ("needs-staging", Environment),
            ("waiting: dependency", Dependency),
            ("upstream", Dependency),
            ("needs review", Review),
            ("blocked", Unknown),
            ("waiting", Unknown),
        ] {
            assert_eq!(waiting_on(label), expected, "{label}");
        }
    }

    #[test]
    fn labels_that_name_one_kind_settle_it() {
        use Kind::*;
        let cases: [(&[&str], Option<Kind>); 9] = [
            (&["bug", "auth"], Some(Fix)),
            (&["Hotfix"], Some(Fix)),
            (&["feature", "export"], Some(Feature)),
            (&["ENHANCEMENT"], Some(Feature)),
            (&["chore"], Some(Chore)),
            (&["docs", "ci"], Some(Chore)),
            // no kind named: the judge's
            (&["export"], None),
            (&[], None),
            // two kinds named: the judge's too
            (&["bug", "feature"], None),
        ];
        for (labels, expected) in cases {
            assert_eq!(kind_of_labels(labels.iter().copied()), expected, "{labels:?}");
        }
    }

    #[test]
    fn blocking_labels_ignore_case() {
        let m = manifest();
        assert_eq!(blocking_label(&m, ["bug", "BLOCKED"]), Some("BLOCKED"));
        assert_eq!(blocking_label(&m, ["waiting: Client"]), Some("waiting: Client"));
        assert_eq!(blocking_label(&m, ["waiting"]), None);
    }

    #[test]
    fn files_map_through_the_globs() {
        let map = ModuleMap::new(&manifest()).unwrap();
        assert_eq!(
            map.module_of_file("src/Billing/X.cs").unwrap().as_str(),
            "Invoicing"
        );
        assert!(map.module_of_file("config/app.json").is_none());
        let pick = |files: &[&str]| map.module_of_files(files.iter().copied()).as_str().to_owned();
        assert_eq!(
            pick(&["src/Export/A.cs", "src/Export/B.cs", "src/Auth/C.cs"]),
            "Export"
        );
        // a tie goes to the first module in the manifest
        assert_eq!(pick(&["src/Auth/C.cs", "src/Invoices/D.cs"]), "Invoicing");
        // Other only when no file matches a module
        assert_eq!(pick(&["config/app.json", "src/Auth/C.cs", "README.md"]), "Auth");
        assert_eq!(pick(&["config/app.json"]), "Other");
        assert_eq!(pick(&[]), "Other");
    }

    #[test]
    fn labels_map_in_manifest_order_and_a_module_name_in_any_case() {
        let map = ModuleMap::new(&manifest()).unwrap();
        let pick = |labels: &[&str]| map.module_of_labels(labels.iter().copied()).as_str().to_owned();
        assert_eq!(pick(&["bug", "Billing"]), "Invoicing");
        assert_eq!(pick(&["export", "invoicing"]), "Invoicing");
        assert_eq!(pick(&["EXPORT"]), "Export");
        // Auth has no module_labels entry; its own name still maps to it
        assert_eq!(pick(&["auth"]), "Auth");
        assert_eq!(pick(&["AUTH"]), "Auth");
        assert_eq!(pick(&["chore"]), "Other");
        assert_eq!(pick(&[]), "Other");
        let names: Vec<String> = map.modules().iter().map(|m| m.as_str().to_owned()).collect();
        assert_eq!(names, ["Invoicing", "Export", "Auth", "Other"]);
    }

    #[test]
    fn unsafe_titles_are_withheld() {
        let redactor = Redactor::new(&[RedactRule {
            pattern: r"(?i)\bmegabank\b".into(),
            replace: "the client".into(),
        }])
        .unwrap();
        let module = ModuleName::parse("Export").unwrap();
        let (text, withheld) = title("Faster statement export", &module, &redactor);
        assert_eq!((text.as_str(), withheld), ("Faster statement export", false));
        let (text, withheld) = title("Monthly digest for Megabank", &module, &redactor);
        assert_eq!(
            (text.as_str(), withheld),
            ("Monthly digest for the client", false)
        );
        // an e-mail address is redacted, never shown
        let (text, withheld) = title(
            "Reminder emails bounce for billing@example.com",
            &module,
            &redactor,
        );
        assert_eq!(
            (text.as_str(), withheld),
            ("Reminder emails bounce for [email]", false)
        );
        for raw in [
            "Fix gap in InvoiceNumberSequencer after restarts",
            "Rename `ledgerEntryCache` setting",
            "Crash when src/Export/SepaExportWriter.cs gets an empty batch",
            "Mirror statements to https://files.example.com",
        ] {
            let (text, withheld) = title(raw, &module, &redactor);
            assert_eq!((text.as_str(), withheld), ("Work item in Export", true), "{raw}");
        }
    }
}
