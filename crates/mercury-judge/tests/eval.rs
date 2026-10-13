//! The labelled sets and the harness: the sets' size and shape, and the rules judge refusing every
//! question a check must refuse without refusing an allowed one.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use std::path::{Path, PathBuf};

use mercury_judge::Catalogue;
use mercury_judge::eval::{self, EvalContext, Sets};
use regex::Regex;

fn root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../..")
}

fn sets() -> Sets {
    Sets::load(&root().join("tests/golden")).unwrap()
}

/// The questions of one part: how many, all of them in English, refused by a check and refused by
/// the policy, and that every input check is the reason for a refusal.
fn assert_part(part: &Sets, total: usize, by_check: usize, by_policy: usize) {
    let q = &part.questions;
    assert_eq!(q.len(), total);
    for question in q {
        assert_eq!(question.lang, "en", "{}", question.id);
    }
    let checks = q.iter().filter(|q| q.expect.refused_by_a_check()).count();
    let policy = q
        .iter()
        .filter(|q| q.expect.refuse && !q.expect.refused_by_a_check())
        .count();
    assert_eq!((checks, policy), (by_check, by_policy));
    for check in eval::INPUT_RISKS {
        assert!(
            q.iter().any(|q| q.expect.reason.as_deref() == Some(check)),
            "{check}"
        );
    }
}

/// The full set: 123 questions in English, the first 75 the ones the thresholds were tuned on and
/// the last 48 held out. Each part refuses for every input check, the confidential check among them.
#[test]
fn the_full_set_has_123_questions_in_two_parts() {
    let sets = sets();
    assert_eq!(sets.questions.len(), 123);
    assert_part(&sets.part(false), 75, 19, 5);
    assert_part(&sets.part(true), 48, 12, 3);
    let mut ids: Vec<&str> = sets.questions.iter().map(|q| q.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), 123, "ids are unique");
}

/// The held-out part is kept apart: it comes after the part the thresholds were tuned on, in every
/// set, and repeats none of its texts, so nothing tuned on can have seen it.
#[test]
fn the_held_out_part_comes_last_and_repeats_nothing_of_the_tuned_part() {
    let sets = sets();
    fn assert_last<T>(items: &[T], held_out: impl Fn(&T) -> bool, set: &str) {
        let first = items.iter().position(&held_out).unwrap();
        assert!(
            items[first..].iter().all(&held_out),
            "{set}: a tuned item after a held-out one"
        );
        assert!(items[..first].iter().all(|item| !held_out(item)), "{set}");
    }
    assert_last(&sets.questions, |q| q.held_out, "questions");
    assert_last(&sets.sentences, |s| s.held_out, "sentences");
    assert_last(&sets.titles, |t| t.held_out, "titles");
    assert_last(&sets.plans, |p| p.held_out, "plans");
    let tuned = sets.part(false);
    let held = sets.part(true);
    assert_eq!(
        (
            held.questions.len(),
            held.sentences.len(),
            held.titles.len(),
            held.plans.len()
        ),
        (48, 23, 16, 19)
    );
    for q in &held.questions {
        assert!(
            tuned
                .questions
                .iter()
                .all(|t| t.text.to_lowercase() != q.text.to_lowercase()),
            "{} repeats a tuned question",
            q.id
        );
    }
    for s in &held.sentences {
        assert!(tuned.sentences.iter().all(|t| t.text != s.text), "{}", s.id);
    }
    for t in &held.titles {
        assert!(tuned.titles.iter().all(|u| u.text != t.text), "{}", t.id);
    }
    // a plan is a question and what was planned for it: the pair is new, the question may recur
    for p in &held.plans {
        assert!(
            tuned
                .plans
                .iter()
                .all(|t| (&t.text, t.plan[0].args.to_string()) != (&p.text, p.plan[0].args.to_string())),
            "{}",
            p.id
        );
    }
}

/// The sentence and title sets hold both kinds of case, in English, for every check that has a rule
/// over them: a check measured only on its clean items says nothing.
#[test]
fn the_sentence_and_title_sets_hold_positive_cases_for_every_output_check() {
    let sets = sets();
    for check in eval::OUTPUT_CHECKS {
        let positive = sets
            .sentences
            .iter()
            .filter(|s| s.expect.fires.iter().any(|f| f == check))
            .count();
        assert!(positive >= 3, "{check}: {positive} positive sentences");
    }
    assert!(sets.sentences.iter().any(|s| s.expect.fires.is_empty()));
    for sentence in &sets.sentences {
        assert_eq!(sentence.lang, "en", "{}", sentence.id);
    }
    let unsafe_titles = sets.titles.iter().filter(|t| t.expect.is_unsafe).count();
    assert!(
        unsafe_titles >= 15 && sets.titles.len() - unsafe_titles >= 15,
        "{unsafe_titles} unsafe titles"
    );
    let mut ids: Vec<&str> = sets.sentences.iter().map(|s| s.id.as_str()).collect();
    ids.sort_unstable();
    ids.dedup();
    assert_eq!(ids.len(), sets.sentences.len(), "sentence ids are unique");
}

/// The plans: both tool checks have clean plans and plans that must fire, all in English, the
/// instruction plans fire both, and every plan's project has its modules (`projects.json`). The
/// code reader's plans, its question an argument, are among them: a clean one and one that fires
/// each check.
#[test]
fn the_plans_hold_clean_and_wrong_ones_for_both_tool_checks() {
    let sets = sets();
    assert_eq!(sets.plans.len(), 60);
    let reader: Vec<_> = sets
        .plans
        .iter()
        .filter(|p| p.plan.iter().any(|call| call.tool == "ask_code"))
        .collect();
    for plan in &reader {
        assert!(
            plan.plan
                .iter()
                .filter(|call| call.tool == "ask_code")
                .all(|call| call.args["question"].is_string()),
            "{}: the reader is asked a question",
            plan.id
        );
    }
    assert!(
        reader.iter().any(|p| p.expect.fires.is_empty() && !p.held_out),
        "a clean reader plan"
    );
    for check in eval::TOOL_CHECKS {
        assert!(
            reader
                .iter()
                .any(|p| p.expect.fires.iter().any(|f| f == check) && !p.held_out),
            "a reader plan that fires {check}"
        );
    }
    let fires = |check: &str| {
        sets.plans
            .iter()
            .filter(|p| p.expect.fires.iter().any(|f| f == check))
            .count()
    };
    assert!(
        fires("args_match_question") >= 20,
        "{}",
        fires("args_match_question")
    );
    assert!(
        fires("args_free_of_instructions") >= 6,
        "{}",
        fires("args_free_of_instructions")
    );
    assert!(sets.plans.iter().filter(|p| p.expect.fires.is_empty()).count() >= 30);
    for plan in &sets.plans {
        assert_eq!(plan.lang, "en", "{}", plan.id);
        assert!(
            sets.modules.contains_key(&plan.project),
            "{}: no modules of {}",
            plan.id,
            plan.project
        );
        assert!(
            plan.expect
                .fires
                .iter()
                .all(|c| eval::TOOL_CHECKS.contains(&c.as_str())),
            "{}",
            plan.id
        );
        // an instruction in an argument is no match either
        if plan.expect.fires.iter().any(|f| f == "args_free_of_instructions") {
            assert!(
                plan.expect.fires.iter().any(|f| f == "args_match_question"),
                "{}",
                plan.id
            );
        }
    }
    // the modules are the fixture's, with the labels its manifest gives them
    let delta: Vec<(&str, Vec<&str>)> = sets.modules["delta"]
        .iter()
        .map(|m| (m.name.as_str(), m.labels.iter().map(String::as_str).collect()))
        .collect();
    let manifest =
        std::fs::read_to_string(root().join("tests/fixtures/repo-delta/.mercury/probe.yaml")).unwrap();
    for (module, labels) in delta {
        for label in labels {
            assert!(
                manifest.contains(label),
                "{module}: the manifest has no label {label}"
            );
        }
    }
}

#[test]
fn the_sets_name_nothing_real() {
    let text = [
        "questions.jsonl",
        "sentences.jsonl",
        "titles.jsonl",
        "plans.jsonl",
        "projects.json",
    ]
    .map(|f| std::fs::read_to_string(root().join("tests/golden").join(f)).unwrap())
    .join("\n");
    let email = Regex::new(r"[\w.+-]+@([\w-]+\.[\w.-]+)").unwrap();
    for domain in email.captures_iter(&text).map(|c| c[1].to_owned()) {
        assert_eq!(domain, "example.com");
    }
    let url = Regex::new(r"https?://([\w.-]+)").unwrap();
    for host in url.captures_iter(&text).map(|c| c[1].to_owned()) {
        assert!(host.ends_with("example.com"), "{host}");
    }
}

/// The rules judge, the last fallback, refuses every question a check must refuse (A1, A11 and A13
/// among them) and none of the allowed ones, on the part the rules were written against. The held-out
/// part is read, not enforced: the rules are patterns, and what they do with phrasings they were not
/// written for is a measurement (`just judge-eval`), not something to fit them to.
#[tokio::test]
async fn the_rules_refuse_the_whole_forbidden_subset_and_nothing_else() {
    let catalogue = Catalogue::load(&root().join("config-examples/checks.yaml")).unwrap();
    let ctx = EvalContext {
        catalogue: &catalogue,
        clients: vec![Regex::new(r"(?i)\bmegabank\b").unwrap()],
        only: None,
        modules: true,
    };
    let report = eval::run("rules", None, &sets().part(false), &ctx).await;
    assert_eq!(report.errors, 0);
    for (lang, [forbidden, refused, allowed, wrong]) in &report.refusals {
        assert_eq!(refused, forbidden, "{lang}: refusal recall");
        assert_eq!(
            *wrong, 0,
            "{lang}: {wrong} of {allowed} allowed questions refused"
        );
    }
    let total = |at: usize| report.refusals.values().map(|row| row[at]).sum::<u32>();
    assert_eq!((total(0), total(2)), (19, 51), "refused by a check, and allowed");
    let rendered = eval::render("2026-10-02", &sets(), &[report], &[]);
    assert!(rendered.contains("| `asks_for_code` |"));
    assert!(rendered.contains("Class, expected (rows) × picked (columns)"));
}

/// The held-out part runs through the harness as a reading of its own, its denominators the labels': it is
/// measured, and no number of it is asserted beyond what the labels say.
#[tokio::test]
async fn the_held_out_part_is_read_apart_with_the_denominators_of_its_labels() {
    let catalogue = Catalogue::load(&root().join("config-examples/checks.yaml")).unwrap();
    let ctx = EvalContext {
        catalogue: &catalogue,
        clients: vec![Regex::new(r"(?i)megabank").unwrap()],
        only: None,
        modules: true,
    };
    let sets = sets();
    let report = eval::run("rules, held out", None, &sets.part(true), &ctx).await;
    assert_eq!(report.errors, 0);
    assert_eq!(report.requests, 48 + 23 + 16 + 19);
    for (lang, [forbidden, _refused, allowed, _wrong]) in &report.refusals {
        assert_eq!((*forbidden, *allowed), (12, 33), "{lang}");
    }
    let rendered = eval::render("2026-10-06", &sets, &[report], &[]);
    assert!(rendered.contains("## rules, held out"));
    assert!(rendered.contains("48 questions, 23 sentences, 16 titles and 19 plans are held out"));
}
