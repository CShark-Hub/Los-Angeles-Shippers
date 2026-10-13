//! `docs/security.md` says which test shows each answer to a threat and each limit. Every test name it
//! cites must be a test that exists, and every file it cites must be there, so the page can't go on
//! naming a test that was renamed or deleted.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

/// The sections of the page that cite tests: from their headings to the next heading of the page's
/// next topic.
fn cited_text() -> String {
    let page = fs::read_to_string(root().join("docs/security.md")).unwrap();
    let mut cited = String::new();
    let mut inside = false;
    for line in page.lines() {
        if let Some(heading) = line.strip_prefix("## ") {
            inside = heading.starts_with("Where each answer is shown")
                || heading.starts_with("The limits, and where");
        }
        if inside {
            cited.push_str(line);
            cited.push('\n');
        }
    }
    cited
}

/// What stands between backticks, in the order the page has it.
fn code_spans(text: &str) -> Vec<String> {
    text.split('`').skip(1).step_by(2).map(str::to_owned).collect()
}

/// Every `fn` name in the Rust sources and tests of the workspace.
fn function_names() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let mut folders = vec![root().join("crates")];
    while let Some(folder) = folders.pop() {
        for entry in fs::read_dir(&folder).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                if path.file_name().is_some_and(|n| n != "target") {
                    folders.push(path);
                }
            } else if path.extension().is_some_and(|e| e == "rs") {
                let text = fs::read_to_string(&path).unwrap();
                for line in text.lines() {
                    let line = line.trim_start();
                    let line = line.strip_prefix("pub ").unwrap_or(line);
                    let line = line.strip_prefix("async ").unwrap_or(line);
                    if let Some(rest) = line.strip_prefix("fn ")
                        && let Some((name, _)) = rest.split_once('(')
                    {
                        names.insert(name.trim().to_owned());
                    }
                }
            }
        }
    }
    names
}

#[test]
fn the_page_cites_tests_and_files_that_exist() {
    let cited = cited_text();
    assert!(!cited.is_empty(), "the page has no section that cites tests");
    let functions = function_names();
    let (mut tests, mut files) = (0, 0);
    for span in code_spans(&cited) {
        if span.contains('/') && Path::new(&span).extension().is_some() {
            assert!(
                root().join(&span).is_file(),
                "the page cites {span}, which is not a file"
            );
            files += 1;
        } else if span.matches('_').count() >= 2
            && span
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        {
            assert!(
                functions.contains(&span),
                "the page cites the test {span}, which no source has"
            );
            tests += 1;
        }
    }
    // the page is not an empty shell: it names a test for each of twenty threats and the limits
    assert!(tests >= 80, "{tests} tests cited");
    assert!(files >= 8, "{files} files cited");
}

#[test]
fn every_threat_of_the_table_has_a_row_that_shows_it() {
    let page = fs::read_to_string(root().join("docs/security.md")).unwrap();
    let rows = |heading: &str| -> usize {
        let mut inside = false;
        let mut count: usize = 0;
        for line in page.lines() {
            if let Some(h) = line.strip_prefix("## ") {
                inside = h.starts_with(heading);
                continue;
            }
            if inside && line.starts_with('|') && !line.starts_with("| ---") && !line.starts_with("|---") {
                count += 1;
            }
        }
        count.saturating_sub(1) // the header
    };
    let threats = rows("Threats and answers");
    assert_eq!(threats, 20, "the page lists twenty threats");
    assert_eq!(
        rows("Where each answer is shown"),
        threats,
        "one row of proof for each threat"
    );
}
