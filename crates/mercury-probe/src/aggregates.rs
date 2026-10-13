//! Counts derived from the snapshot's files, in the code modes only: tests and TODO/FIXME markers
//! per module, and change heat from the history. Nothing here keeps a file's name or a line.

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;
use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use mercury_schema::ModuleName;
use mercury_schema::manifest::TestFramework;
use regex::Regex;

use crate::meta::History;
use crate::rules::ModuleMap;

/// The test patterns of one framework: which files hold tests, and what one test looks like.
struct Pattern {
    framework: TestFramework,
    file: fn(&str) -> bool,
    test: &'static LazyLock<Regex>,
}

macro_rules! pattern {
    ($name:ident, $re:literal) => {
        static $name: LazyLock<Regex> = LazyLock::new(|| {
            #[allow(
                clippy::expect_used,
                reason = "a literal pattern, covered by the tests below"
            )]
            Regex::new($re).expect("literal pattern")
        });
    };
}

pattern!(RUST_TEST, r"(?m)^\s*#\[(?:tokio::)?test\b");
pattern!(JS_TEST, r"(?m)(?:^|[^\w.$])(?:it|test)\s*\(");
pattern!(XUNIT_TEST, r"(?m)^\s*\[\s*(?:Fact|Theory)\b");
pattern!(NUNIT_TEST, r"(?m)^\s*\[\s*(?:Test|TestCase)\b");
pattern!(MSTEST_TEST, r"(?m)^\s*\[\s*TestMethod\b");
pattern!(JUNIT_TEST, r"(?m)^\s*@Test\b");
pattern!(PYTEST_TEST, r"(?m)^\s*(?:async\s+)?def\s+test_");
pattern!(MARKER, r"\b(?:TODO|FIXME)\b");

fn extension(path: &str) -> &str {
    path.rsplit_once('.').map_or("", |(_, ext)| ext)
}

fn file_name(path: &str) -> &str {
    path.rsplit('/').next().unwrap_or(path)
}

fn js_test_file(path: &str) -> bool {
    let name = file_name(path);
    name.contains(".test.") || name.contains(".spec.")
}

fn python_test_file(path: &str) -> bool {
    let name = file_name(path);
    extension(name) == "py" && (name.starts_with("test_") || name.ends_with("_test.py"))
}

static PATTERNS: [Pattern; 8] = [
    Pattern {
        framework: TestFramework::Rust,
        file: |p| extension(p) == "rs",
        test: &RUST_TEST,
    },
    Pattern {
        framework: TestFramework::Jest,
        file: js_test_file,
        test: &JS_TEST,
    },
    Pattern {
        framework: TestFramework::Vitest,
        file: js_test_file,
        test: &JS_TEST,
    },
    Pattern {
        framework: TestFramework::Xunit,
        file: |p| extension(p) == "cs",
        test: &XUNIT_TEST,
    },
    Pattern {
        framework: TestFramework::Nunit,
        file: |p| extension(p) == "cs",
        test: &NUNIT_TEST,
    },
    Pattern {
        framework: TestFramework::Mstest,
        file: |p| extension(p) == "cs",
        test: &MSTEST_TEST,
    },
    Pattern {
        framework: TestFramework::Junit,
        file: |p| matches!(extension(p), "java" | "kt"),
        test: &JUNIT_TEST,
    },
    Pattern {
        framework: TestFramework::Pytest,
        file: python_test_file,
        test: &PYTEST_TEST,
    },
];

/// The tests in one file's text, by the frameworks the manifest lists (all of them when it lists
/// none). Jest and Vitest share a pattern, so it counts once.
pub fn count_tests(path: &str, text: &str, frameworks: &[TestFramework]) -> u32 {
    let mut seen: Vec<*const LazyLock<Regex>> = Vec::new();
    let mut total = 0;
    for pattern in &PATTERNS {
        let listed = frameworks.is_empty() || frameworks.contains(&pattern.framework);
        let key: *const LazyLock<Regex> = pattern.test;
        if !listed || !(pattern.file)(path) || seen.contains(&key) {
            continue;
        }
        seen.push(key);
        total += u32::try_from(pattern.test.find_iter(text).count()).unwrap_or(u32::MAX);
    }
    total
}

/// `TODO` and `FIXME` as words, upper case.
pub fn count_markers(text: &str) -> u32 {
    u32::try_from(MARKER.find_iter(text).count()).unwrap_or(u32::MAX)
}

/// One module's counts from the files.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FileCounts {
    pub tests_total: u32,
    pub tests_changed_14d: u32,
    pub todo_fixme: u32,
}

/// Change heat of one module.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Heat {
    pub commits_14d: u32,
    pub last_touched: Option<DateTime<Utc>>,
}

/// Reads every listed file once and counts per module. `changed` holds the paths a commit in the
/// 14-day window touched. A file that can't be read as text counts nothing.
pub fn file_counts(
    repo: &Path,
    files: &[String],
    map: &ModuleMap,
    frameworks: &[TestFramework],
    changed: &BTreeSet<&str>,
) -> BTreeMap<ModuleName, FileCounts> {
    let mut counts: BTreeMap<ModuleName, FileCounts> = map
        .modules()
        .into_iter()
        .map(|module| (module, FileCounts::default()))
        .collect();
    for path in files {
        let Ok(text) = fs::read_to_string(repo.join(path)) else {
            continue;
        };
        let module = map
            .module_of_file(path)
            .cloned()
            .unwrap_or_else(ModuleName::other);
        let entry = counts.entry(module).or_default();
        let tests = count_tests(path, &text, frameworks);
        entry.tests_total += tests;
        if changed.contains(path.as_str()) {
            entry.tests_changed_14d += tests;
        }
        entry.todo_fixme += count_markers(&text);
    }
    counts
}

/// The paths commits in `[since, until]` touched.
pub fn changed_since(history: &History, since: DateTime<Utc>, until: DateTime<Utc>) -> BTreeSet<&str> {
    history
        .commits
        .iter()
        .filter(|c| c.committed_at >= since && c.committed_at <= until)
        .flat_map(|c| c.files.iter().map(String::as_str))
        .collect()
}

/// Commits per module in `[since, until]` and each module's last-touched date, from the history.
/// A commit counts once per module it touches; a path `excluded` matches counts for none.
pub fn heat_from_history(
    history: &History,
    map: &ModuleMap,
    excluded: impl Fn(&str) -> bool,
    since: DateTime<Utc>,
    until: DateTime<Utc>,
) -> BTreeMap<ModuleName, Heat> {
    let mut heat: BTreeMap<ModuleName, Heat> = map
        .modules()
        .into_iter()
        .map(|module| (module, Heat::default()))
        .collect();
    for commit in history.commits.iter().filter(|c| c.committed_at <= until) {
        let modules: BTreeSet<ModuleName> = commit
            .files
            .iter()
            .filter(|path| !excluded(path))
            .map(|path| {
                map.module_of_file(path)
                    .cloned()
                    .unwrap_or_else(ModuleName::other)
            })
            .collect();
        for module in modules {
            let entry = heat.entry(module).or_default();
            if commit.committed_at >= since {
                entry.commits_14d += 1;
            }
            if entry.last_touched.is_none_or(|at| at < commit.committed_at) {
                entry.last_touched = Some(commit.committed_at);
            }
        }
    }
    heat
}

#[cfg(test)]
mod tests {
    use super::*;
    use TestFramework::*;

    #[test]
    fn each_framework_counts_its_own_tests() {
        let cases: [(&str, &str, &[TestFramework], u32); 12] = [
            (
                "a.rs",
                "#[test]\nfn a() {}\n  #[tokio::test]\nasync fn b() {}\n// #[test] in a comment\n",
                &[Rust],
                2,
            ),
            (
                "a.test.ts",
                "test('a', () => {});\nit(\"b\", () => {});\nobj.test(1);\n",
                &[Jest],
                2,
            ),
            (
                "a.spec.js",
                "describe('x', () => { it('a', () => {}); });",
                &[Vitest],
                1,
            ),
            ("a.ts", "test('a', () => {});", &[Jest], 0),
            (
                "A.cs",
                "[Fact]\nvoid A(){}\n[Theory]\n[InlineData(1)]\nvoid B(int x){}\n",
                &[Xunit],
                2,
            ),
            ("A.cs", "[Fact(Skip = \"x\")]\nvoid A(){}\n", &[Xunit], 1),
            (
                "A.cs",
                "[Test]\nvoid A(){}\n[TestCase(1)]\nvoid B(){}\n",
                &[Nunit],
                2,
            ),
            ("A.cs", "[TestMethod]\nvoid A(){}\n", &[Mstest], 1),
            ("A.java", "@Test\nvoid a() {}\n@Test\nvoid b() {}\n", &[Junit], 2),
            (
                "test_a.py",
                "def test_a():\n    pass\nasync def test_b():\n    pass\ndef helper():\n",
                &[Pytest],
                2,
            ),
            ("a.py", "def test_a():\n    pass\n", &[Pytest], 0),
            // the manifest's frameworks pick the patterns
            ("A.cs", "[Fact]\nvoid A(){}\n", &[Junit], 0),
        ];
        for (path, text, frameworks, expected) in cases {
            assert_eq!(
                count_tests(path, text, frameworks),
                expected,
                "{path} {frameworks:?}"
            );
        }
    }

    #[test]
    fn with_no_framework_listed_every_pattern_applies_once() {
        assert_eq!(count_tests("a.test.ts", "test('a', () => {});", &[]), 1);
        assert_eq!(count_tests("A.cs", "[Fact]\n[Test]\n[TestMethod]\n", &[]), 3);
    }

    #[test]
    fn markers_are_upper_case_words() {
        assert_eq!(
            count_markers("// TODO: a\n// FIXME b\n// todo c\n// TODOS\nNOTODO\n"),
            2
        );
    }
}
