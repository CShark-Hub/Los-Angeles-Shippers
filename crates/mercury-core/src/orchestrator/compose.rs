//! Composing: the composer's prompt, an answer cut into sentences with the fact ids each cites,
//! and the template composer, which needs no model and still cites every sentence.

use std::sync::LazyLock;

use chrono::{DateTime, Utc};
use mercury_schema::facts::{BuildStatus, Status, Tested, WaitingOn};
use regex::Regex;
use serde_json::Value;

use super::facts::{Unit, UnitKind, WITHHELD};

/// The most words an answer may have; sentences past it are left out.
pub const MAX_WORDS: usize = 120;

/// The words of a delivered answer when so much was stripped that it no longer stands. The web
/// client tells it from the rest of an answer by its text, so a change to it is a change to
/// `answer-text.ts`.
pub const WITHHELD_NOTE: &str = "Some details were withheld.";

fn re(pattern: &str) -> Regex {
    #[allow(
        clippy::expect_used,
        reason = "the patterns are literals of this module, covered by its tests"
    )]
    Regex::new(pattern).expect("literal pattern")
}

/// Brackets that hold nothing but fact ids, separators, spaces and more brackets: what may follow
/// a stop and still belong to its sentence, nested citations included. Only where a sentence ends
/// reads this; what is a citation is decided by [`split`].
const BRACKETED_IDS: &str = r"\[(?:[\s,;\[\]]|f_[A-Za-z0-9]{4,16})*\]";
/// What may close a sentence after its stop: a closing quote or bracket, the marks of bold or
/// italic text, and the invisible characters the shown text drops.
const CLOSERS: &str = r#"["'”’»›)\]}*_`\p{Cf}\p{Default_Ignorable_Code_Point}\p{Variation_Selector}]*"#;
/// A sentence's end: a stop, a question or an exclamation mark, then what closes it and the
/// citations written after it, before a space or the end. `"Done." Next`, `**Done.** Next` and
/// `(Done.) Next` are two sentences each, so a head without a citation of its own can't ride on
/// the citation of the sentence after it.
static END: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"[.!?…]+{CLOSERS}(?:\s*{BRACKETED_IDS}{CLOSERS})*(?:\s+|$)"
    ))
});

/// The composer's prompt, from its file: `{facts_json}` and `{question}` filled in, the question
/// last.
pub fn prompt(template: &str, facts: &Value, question: &str) -> String {
    template
        .replace("{facts_json}", &facts.to_string())
        .replace("{question}", question)
}

/// The text cut into sentences, each keeping the citations written after its stop.
pub fn sentences(text: &str) -> Vec<String> {
    let text = text.trim();
    let mut sentences: Vec<String> = Vec::new();
    let mut start = 0;
    for end in END.find_iter(text) {
        push(&mut sentences, &text[start..end.end()]);
        start = end.end();
    }
    push(&mut sentences, &text[start..]);
    sentences
}

fn push(sentences: &mut Vec<String>, piece: &str) {
    let mut piece = piece.trim();
    // citations that open a piece belong to the sentence before it
    if let Some(last) = sentences.last_mut() {
        while let Some(cited) = leading_citation(piece) {
            last.push(' ');
            last.push_str(cited);
            piece = piece[cited.len()..].trim_start();
        }
    }
    if !piece.is_empty() {
        sentences.push(piece.to_owned());
    }
}

/// The citation a piece opens with, nested brackets and all: a bracket that leaves no text once
/// its citations are out.
fn leading_citation(piece: &str) -> Option<&str> {
    if !piece.starts_with('[') {
        return None;
    }
    let mut depth = 0_usize;
    for (at, c) in piece.char_indices() {
        match c {
            '[' => depth += 1,
            ']' => {
                depth -= 1;
                if depth == 0 {
                    let group = &piece[..=at];
                    return split(group).0.is_empty().then_some(group);
                }
            }
            _ => {}
        }
    }
    None
}

/// The fact ids a sentence cites, in the order they are written, each once.
pub fn citations(sentence: &str) -> Vec<String> {
    split(sentence).1
}

/// A sentence as the person will read it, then the ids it cites: the invisible characters gone,
/// the citations taken out and the text tidied as the answer shows it, and each cited id once at
/// the end. The output checks read this, and the answer shows it without the ids, so the text
/// that was checked is the text that is shown: a citation or an invisible character inside a word
/// can't join two harmless pieces into a name or a path after the checks ran.
///
/// Its text holds no citation any more, so [`without_citations`] of what this returns is that text
/// exactly, character for character: what the answer delivers is what the checks read, without
/// the ids.
pub fn as_shown(sentence: &str) -> String {
    let visible = mercury_judge::stages::without_invisible(sentence);
    let (text, ids) = split(&visible);
    if ids.is_empty() {
        return text;
    }
    let cited: Vec<String> = ids.iter().map(|id| format!("[{id}]")).collect();
    format!("{text} {}", cited.join(" "))
}

/// The sentence without its citations, as the person reads it.
pub fn without_citations(sentence: &str) -> String {
    split(sentence).0
}

/// A sentence's text without its citations, tidied, and the ids they cite in the order they are
/// written, each once.
///
/// A citation is a bracket that holds fact ids separated by commas, semicolons or spaces
/// (`[f_7Qx2]`, `[f_7Qx2, f_9Kd4]`, `[f_7Qx2; f_9Kd4]`), or nothing at all (`[]`). Brackets inside
/// one go first, so `[[f_7Qx2]]` and `[f_7Qx2[f_9Kd4]]` are citations too, and leave neither an
/// empty bracket nor half of one. Anything else in a bracket keeps it in the text, where the output
/// checks read it. The text is taken apart again until nothing changes, so the text returned holds
/// no citation and is its own result.
fn split(sentence: &str) -> (String, Vec<String>) {
    let (mut text, mut ids) = taken_apart(sentence);
    loop {
        let (again, more) = taken_apart(&text);
        for id in more {
            if !ids.contains(&id) {
                ids.push(id);
            }
        }
        if again == text {
            return (text, ids);
        }
        text = again;
    }
}

/// One pass of [`split`]: the brackets read left to right, each citation taken out when its
/// bracket closes, so an inner one is out before the bracket around it is read; then the text tidied.
fn taken_apart(sentence: &str) -> (String, Vec<String>) {
    // the text kept so far, each character with where it stands in the sentence
    let mut kept: Vec<(usize, char)> = Vec::with_capacity(sentence.len());
    // where each bracket that is still open starts in `kept`
    let mut open: Vec<usize> = Vec::new();
    let mut cited: Vec<(usize, String)> = Vec::new();
    for (at, c) in sentence.char_indices() {
        match c {
            '[' => {
                open.push(kept.len());
                kept.push((at, c));
            }
            ']' => {
                if let Some(start) = open.pop()
                    && let Some(ids) = ids_inside(&kept[start + 1..])
                {
                    cited.extend(ids);
                    kept.truncate(start);
                } else {
                    // a bracket around something else, or one that never opened: text
                    kept.push((at, c));
                }
            }
            _ => kept.push((at, c)),
        }
    }
    cited.sort_by_key(|(at, _)| *at);
    let mut ids: Vec<String> = Vec::new();
    for (_, id) in cited {
        if !ids.contains(&id) {
            ids.push(id);
        }
    }
    let text: String = kept.into_iter().map(|(_, c)| c).collect();
    (tidy(&text), ids)
}

/// The ids a bracket holds, each with where it stands, when it holds nothing but ids and their
/// separators; `None` when anything else is in it.
fn ids_inside(inside: &[(usize, char)]) -> Option<Vec<(usize, String)>> {
    let mut ids = Vec::new();
    let mut token: Option<(usize, String)> = None;
    for &(at, c) in inside {
        if c.is_whitespace() || c == ',' || c == ';' {
            if let Some(found) = token.take() {
                ids.push(found);
            }
        } else {
            token.get_or_insert_with(|| (at, String::new())).1.push(c);
        }
    }
    ids.extend(token);
    ids.iter().all(|(_, id)| is_id(id)).then_some(ids)
}

fn is_id(token: &str) -> bool {
    token
        .strip_prefix("f_")
        .is_some_and(|rest| (4..=16).contains(&rest.len()) && rest.bytes().all(|b| b.is_ascii_alphanumeric()))
}

/// Spaces as one, and none before a stop or a comma a citation stood before.
fn tidy(text: &str) -> String {
    let text = text.split_whitespace().collect::<Vec<_>>().join(" ");
    text.replace(" .", ".")
        .replace(" ,", ",")
        .replace(" !", "!")
        .replace(" ?", "?")
}

pub fn words(text: &str) -> usize {
    without_citations(text).split_whitespace().count()
}

/// The sentences that fit in [`MAX_WORDS`], in order.
pub fn within_words(sentences: Vec<String>) -> Vec<String> {
    let mut total = 0;
    sentences
        .into_iter()
        .take_while(|sentence| {
            total += words(sentence);
            total <= MAX_WORDS
        })
        .collect()
}

/// The template composer: one plain sentence per unit, each citing it. Less fluent than a model,
/// and nothing in it was written by one.
pub fn template(units: &[Unit]) -> String {
    units
        .iter()
        .map(|unit| capitalized(&sentence(unit)))
        // a finding is no sentence of the template's: nothing a model wrote reaches an answer through it
        .filter(|sentence| !sentence.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

/// A sentence starting with a capital, though a title may start lower case once redacted.
fn capitalized(sentence: &str) -> String {
    let mut chars = sentence.chars();
    match chars.next() {
        Some(first) => first.to_uppercase().chain(chars).collect(),
        None => String::new(),
    }
}

fn sentence(unit: &Unit) -> String {
    let id = &unit.id;
    let json = &unit.json;
    let int = |key: &str| json.get(key).and_then(Value::as_u64).unwrap_or(0);
    match (&unit.kind, &unit.fact) {
        (UnitKind::Fact, Some(fact)) => {
            let tests = match fact.tested {
                Tested::Passing => ", with passing tests",
                Tested::Failing => ", with failing tests",
                Tested::Missing => ", without tests",
                Tested::Unknown => "",
            };
            let waiting = fact
                .waiting_on
                .map(|w| format!(", waiting on {}", waiting_on(w)))
                .unwrap_or_default();
            // a withheld title already names the module, or is only [withheld]
            let subject = if fact.title_withheld || fact.title.as_str() == WITHHELD {
                format!("A work item in {}", fact.module)
            } else {
                format!("{} in {}", fact.title, fact.module)
            };
            format!("{subject} is {}{tests}{waiting} [{id}].", status(fact.status))
        }
        (UnitKind::Tests(module), _) => {
            let total = json.get("tests_total").and_then(Value::as_u64);
            let (build, at) = run(json, "/last_ci");
            if total == Some(0) {
                // no tests is the whole answer: what CI did ran no tests of this module
                return format!("{module} has no tests [{id}].");
            }
            let tests = total.map_or("an unknown number of tests".to_owned(), |n| {
                format!("{n} {}", one_or_many(n, "test", "tests"))
            });
            // "it" for one test, "they" for any other number, an unknown one included
            let failing = if total == Some(1) { "it is" } else { "they are" };
            match build {
                BuildStatus::Failed => format!(
                    "{module} has {tests}, and {failing} failing: the last CI run failed{} [{id}].",
                    at.map(|at| format!(" on {at}")).unwrap_or_default()
                ),
                _ => format!(
                    "{module} has {tests}, and its last CI run {} [{id}].",
                    build_words(build)
                ),
            }
        }
        (UnitKind::Health, _) => {
            let (build, at) = run(json, "/build");
            // the build's time, once it has ended
            let at = at.filter(|_| matches!(build, BuildStatus::Passed | BuildStatus::Failed));
            let rate = json.get("ci_pass_rate_7d").and_then(Value::as_f64).unwrap_or(0.0) * 100.0;
            let prs = json
                .pointer("/open_prs/count")
                .and_then(Value::as_u64)
                .unwrap_or(0);
            let score = int("score");
            format!(
                "The last build {}{}, {rate:.0}% of CI runs passed in the last 7 days, {prs} {} open, and the health score is {score} of 100 [{id}].",
                build_words(build),
                at.map(|at| format!(" on {at}")).unwrap_or_default(),
                one_or_many(prs, "pull request is", "pull requests are")
            )
        }
        (UnitKind::Changes(module), _) => {
            let (commits, merged) = (int("commits"), int("merged_prs"));
            format!(
                "{module} had {commits} {} and {merged} merged {} [{id}].",
                one_or_many(commits, "commit", "commits"),
                one_or_many(merged, "pull request", "pull requests")
            )
        }
        // a fact unit always carries its fact; a finding is the reader's line, which no template writes
        (UnitKind::Fact, None) | (UnitKind::Finding, _) => String::new(),
    }
}

/// The words that follow a number: `one` after 1, `many` after any other, 0 included.
fn one_or_many<'a>(n: u64, one: &'a str, many: &'a str) -> &'a str {
    if n == 1 { one } else { many }
}

fn status(status: Status) -> &'static str {
    match status {
        Status::Done => "done",
        Status::InReview => "in review",
        Status::InProgress => "in progress",
        Status::Blocked => "blocked",
        Status::NotStarted => "not started",
        Status::Unclear => "of unclear status",
    }
}

fn waiting_on(waiting: WaitingOn) -> &'static str {
    match waiting {
        WaitingOn::Review => "a review",
        WaitingOn::Dependency => "a dependency",
        WaitingOn::Environment => "an environment",
        WaitingOn::External => "someone outside the team",
        WaitingOn::Decision => "a decision",
        WaitingOn::Unknown => "something not recorded",
    }
}

/// A CI run's status and, when it has one, its time as the answers write it.
fn run(json: &Value, pointer: &str) -> (BuildStatus, Option<String>) {
    let status = json
        .pointer(&format!("{pointer}/status"))
        .and_then(|v| serde_json::from_value::<BuildStatus>(v.clone()).ok())
        .unwrap_or(BuildStatus::None);
    let at = json
        .pointer(&format!("{pointer}/at"))
        .and_then(|v| serde_json::from_value::<DateTime<Utc>>(v.clone()).ok())
        .map(super::texts::at_minute);
    (status, at)
}

/// What a build did, after "the last build" or "its last CI run".
fn build_words(build: BuildStatus) -> &'static str {
    match build {
        BuildStatus::Passed => "passed",
        BuildStatus::Failed => "failed",
        BuildStatus::Running => "is still running",
        BuildStatus::None => "isn't recorded",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sentences_keep_the_citations_after_their_stop() {
        let text = "The CSV export was merged [f_7Qx2]. Numbering was fixed. [f_9Kd4] Is anything blocked? No [f_c0h0]";
        assert_eq!(
            sentences(text),
            [
                "The CSV export was merged [f_7Qx2].",
                "Numbering was fixed. [f_9Kd4]",
                "Is anything blocked?",
                "No [f_c0h0]"
            ]
        );
        assert_eq!(
            sentences("Version 1.5 shipped [f_aaaa]."),
            ["Version 1.5 shipped [f_aaaa]."]
        );
        assert!(sentences("  ").is_empty());
    }

    #[test]
    fn citations_are_read_once_and_removed_for_the_reader() {
        let sentence = "Export and numbering are done [f_7Qx2][f_9Kd4] [f_7Qx2].";
        assert_eq!(citations(sentence), ["f_7Qx2", "f_9Kd4"]);
        assert_eq!(without_citations(sentence), "Export and numbering are done.");
        assert_eq!(without_citations("Done [f_7Qx2], mostly."), "Done, mostly.");
        assert!(citations("Nothing cited here.").is_empty());
    }

    #[test]
    fn several_ids_in_one_bracket_are_a_citation_of_each() {
        let sentence = "Three items are blocked [f_wxhuz68kgg, f_4cidsczy9t,f_h7ffkzdrkj].";
        assert_eq!(
            citations(sentence),
            ["f_wxhuz68kgg", "f_4cidsczy9t", "f_h7ffkzdrkj"]
        );
        assert_eq!(without_citations(sentence), "Three items are blocked.");
        assert_eq!(
            as_shown(sentence),
            "Three items are blocked. [f_wxhuz68kgg] [f_4cidsczy9t] [f_h7ffkzdrkj]"
        );
        // a bracket opening the next piece still belongs to the sentence before it
        assert_eq!(
            sentences("Two are done. [f_7Qx2, f_9Kd4] One is blocked [f_c0h0]."),
            ["Two are done. [f_7Qx2, f_9Kd4]", "One is blocked [f_c0h0]."]
        );
        // anything else in the bracket is no citation, and stays in the text the checks read
        assert!(citations("Done [f_7Qx2 and the rest].").is_empty());
        assert!(citations("Done [f_7Qx2, src/main.rs].").is_empty());
        assert_eq!(
            without_citations("Done [f_7Qx2 and the rest]."),
            "Done [f_7Qx2 and the rest]."
        );
    }

    #[test]
    fn ids_after_semicolons_or_spaces_are_a_citation_of_each() {
        for sentence in [
            "Both are done [f_7Qx2; f_9Kd4].",
            "Both are done [f_7Qx2 f_9Kd4].",
            "Both are done [f_7Qx2;f_9Kd4 , ].",
        ] {
            assert_eq!(citations(sentence), ["f_7Qx2", "f_9Kd4"], "{sentence}");
            assert_eq!(without_citations(sentence), "Both are done.", "{sentence}");
            assert_eq!(as_shown(sentence), "Both are done. [f_7Qx2] [f_9Kd4]");
        }
    }

    /// A model that nests its brackets still cites: the brackets inside go first, and nothing of
    /// the citation is left in the text, neither an empty bracket nor half of one.
    #[test]
    fn a_nested_citation_leaves_no_bracket_behind() {
        for (sentence, text, ids) in [
            ("Export is done [[f_7Qx2]].", "Export is done.", &["f_7Qx2"][..]),
            ("Export is done [[[f_7Qx2]]].", "Export is done.", &["f_7Qx2"]),
            ("ab[f_7Qx2[f_9Kd4]]cd", "abcd", &["f_7Qx2", "f_9Kd4"]),
            (
                "Both are done [f_7Qx2, [f_9Kd4]].",
                "Both are done.",
                &["f_7Qx2", "f_9Kd4"],
            ),
            (
                "Both are done [[f_7Qx2], f_9Kd4].",
                "Both are done.",
                &["f_7Qx2", "f_9Kd4"],
            ),
            (
                "Both are done [[f_7Qx2][f_9Kd4]].",
                "Both are done.",
                &["f_7Qx2", "f_9Kd4"],
            ),
        ] {
            assert_eq!(without_citations(sentence), text, "{sentence}");
            assert_eq!(citations(sentence), ids, "{sentence}");
            let shown = as_shown(sentence);
            let checked = shown.split(" [f_").next().unwrap_or_default();
            assert_eq!(checked, text, "{sentence}");
            assert!(!checked.contains(['[', ']']), "{sentence}: {shown}");
        }
        // a nested citation after a stop, with or without a space, belongs to the sentence before
        for text in [
            "Export is done. [[f_7Qx2]] Numbering is fixed [f_9Kd4].",
            "Export is done.[[f_7Qx2]] Numbering is fixed [f_9Kd4].",
        ] {
            let found = sentences(text);
            assert_eq!(found.len(), 2, "{found:?}");
            assert_eq!(citations(&found[0]), ["f_7Qx2"], "{found:?}");
            assert_eq!(found[1], "Numbering is fixed [f_9Kd4].");
        }
        // words in a bracket keep it, and the checks read them
        assert_eq!(without_citations("Done [see [f_7Qx2]]."), "Done [see ].");
        assert_eq!(citations("Done [see [f_7Qx2]]."), ["f_7Qx2"]);
    }

    /// An empty bracket cites nothing and is no part of the text either: a sentence with only an
    /// empty citation is uncited, and the citation rule drops it.
    #[test]
    fn an_empty_bracket_cites_nothing_and_leaves_nothing() {
        for sentence in [
            "Export is done [].",
            "Export is done [ ].",
            "Export is done [[]].",
        ] {
            assert!(citations(sentence).is_empty(), "{sentence}");
            assert_eq!(without_citations(sentence), "Export is done.", "{sentence}");
            assert_eq!(as_shown(sentence), "Export is done.", "{sentence}");
        }
        assert_eq!(
            as_shown("Export is done [] [f_7Qx2]."),
            "Export is done. [f_7Qx2]"
        );
        // after its stop, it stays with the sentence before
        assert_eq!(
            sentences("Export is done [f_7Qx2]. [] Numbering is fixed [f_9Kd4]."),
            ["Export is done [f_7Qx2]. []", "Numbering is fixed [f_9Kd4]."]
        );
    }

    /// Every id of a bracket is a citation, a known one and an invented one alike, so the
    /// citation rule sees the invented one and drops the sentence.
    #[test]
    fn a_bracket_of_known_and_unknown_ids_cites_each() {
        assert_eq!(
            citations("Billing waits on a review [f_7Qx2, f_zzzz99]."),
            ["f_7Qx2", "f_zzzz99"]
        );
        assert_eq!(
            citations("Billing waits on a review [f_7Qx2; f_zzzz99] [f_9Kd4]."),
            ["f_7Qx2", "f_zzzz99", "f_9Kd4"]
        );
    }

    /// A stop followed by a closing quote, a closing bracket or the end of bold text ends its
    /// sentence, so a head that cites nothing can't ride on the next sentence's citation.
    #[test]
    fn a_stop_before_a_quote_a_bracket_or_bold_ends_the_sentence() {
        for (text, head) in [
            (
                r#"He wrote "Export is done." Numbering is fixed [f_9Kd4]."#,
                r#"He wrote "Export is done.""#,
            ),
            (
                "He wrote “Export is done.” Numbering is fixed [f_9Kd4].",
                "He wrote “Export is done.”",
            ),
            (
                "He wrote 'Export is done.' Numbering is fixed [f_9Kd4].",
                "He wrote 'Export is done.'",
            ),
            (
                "**Export is done.** Numbering is fixed [f_9Kd4].",
                "**Export is done.**",
            ),
            (
                "*Export is done!* Numbering is fixed [f_9Kd4].",
                "*Export is done!*",
            ),
            (
                "(Export is done.) Numbering is fixed [f_9Kd4].",
                "(Export is done.)",
            ),
            (
                "[Export is done?] Numbering is fixed [f_9Kd4].",
                "[Export is done?]",
            ),
            (
                "Is export done?\") Numbering is fixed [f_9Kd4].",
                "Is export done?\")",
            ),
            (
                "Export is done.\u{200B} Numbering is fixed [f_9Kd4].",
                "Export is done.\u{200B}",
            ),
        ] {
            let found = sentences(text);
            assert_eq!(found, [head, "Numbering is fixed [f_9Kd4]."], "{text}");
            assert!(citations(&found[0]).is_empty(), "{text}");
        }
        // citations after the closing mark still belong to the sentence they close
        assert_eq!(
            sentences(r#""Export is done." [f_7Qx2] Numbering is fixed [f_9Kd4]."#),
            [r#""Export is done." [f_7Qx2]"#, "Numbering is fixed [f_9Kd4]."]
        );
        assert_eq!(
            sentences("**Export is done.**[f_7Qx2] Numbering is fixed [f_9Kd4]."),
            ["**Export is done.**[f_7Qx2]", "Numbering is fixed [f_9Kd4]."]
        );
        assert_eq!(
            sentences("**Export is done.** [f_7Qx2]** Numbering is fixed [f_9Kd4]."),
            ["**Export is done.** [f_7Qx2]**", "Numbering is fixed [f_9Kd4]."]
        );
        // a stop inside a word or a number is no end
        assert_eq!(
            sentences("Version 1.5 of export.csv shipped [f_7Qx2]."),
            ["Version 1.5 of export.csv shipped [f_7Qx2]."]
        );
    }

    /// The output checks read `as_shown`, and the answer delivers `without_citations` of it: the two
    /// are the same text, whatever the brackets were, and its ids are the sentence's citations.
    #[test]
    fn the_text_the_checks_read_is_the_text_delivered() {
        for sentence in [
            "Export is done [f_7Qx2].",
            "Export is done [[f_7Qx2]].",
            "ab[f_7Qx2[f_9Kd4]]cd [f_7Qx2].",
            "Mega[f_7Qx2]bank wants the export [f_7Qx2].",
            "Mega\u{200B}bank wants the export [f_7Qx2].",
            "The writer is in src[f_7Qx2]/Export [f_7Qx2].",
            "Both [f_7Qx2; f_9Kd4] and more [f_7Qx2 f_9Kd4].",
            "Done [] [ ] [[]] [f_7Qx2].",
            "Done [[f_7Qx2] [f_9Kd4].",
            "Done [f_7Qx2]] [f_9Kd4].",
            "Done [ [f_7Qx2].",
            "Done [f_7Qx2, [f_9Kd4].",
            "Done [f_7Qx2 and the rest] [f_9Kd4].",
            "Done [see [f_7Qx2]] [f_9Kd4].",
            "Done ][f_7Qx2][ [f_9Kd4].",
            "Done\u{00A0}[f_7Qx2]\u{00A0}.\n[f_9Kd4]",
            "Done , [f_7Qx2] . ! ? [f_9Kd4]",
            "[f_7Qx2]",
            "[f_7Qx2] Done.",
            "[f_aaaaaaaaaaaaaaaaaaaa] is too long [f_7Qx2].",
        ] {
            let visible = mercury_judge::stages::without_invisible(sentence);
            let shown = as_shown(sentence);
            let delivered = without_citations(&shown);
            let ids = citations(&visible);
            assert_eq!(citations(&shown), ids, "{sentence}: {shown}");
            let cited: Vec<String> = ids.iter().map(|id| format!("[{id}]")).collect();
            let expected = if ids.is_empty() {
                delivered.clone()
            } else {
                format!("{delivered} {}", cited.join(" "))
            };
            assert_eq!(shown, expected, "{sentence}");
            assert_eq!(without_citations(&delivered), delivered, "{sentence}");
            assert!(citations(&delivered).is_empty(), "{sentence}: {delivered}");
        }
    }

    #[test]
    fn an_answer_stops_at_its_word_limit() {
        let long: Vec<String> = (0..20)
            .map(|i| format!("Sentence {i} has seven words in it [f_aaaa]."))
            .collect();
        let kept = within_words(long);
        assert_eq!(kept.len(), 17, "17 × 7 = 119 words, the 18th would pass 120");
    }

    fn fact_unit(title: &str, withheld: bool) -> Unit {
        let fact: mercury_schema::Fact = serde_json::from_value(serde_json::json!({
            "id": "f_aaaa", "kind": "feature", "title": title, "module": "Export", "status": "done",
            "tested": "unknown", "confidence": 0.8, "waiting_on": null, "people": [], "evidence": [],
            "title_withheld": withheld, "updated_at": "2026-10-01T10:00:00Z" }))
        .unwrap();
        Unit {
            id: "f_aaaa".to_owned(),
            json: serde_json::Value::Null,
            fact: Some(fact),
            confidence: 0.8,
            kind: UnitKind::Fact,
            as_of: None,
        }
    }

    fn figures_unit(kind: UnitKind, json: serde_json::Value) -> Unit {
        Unit {
            id: "f_c0t0".to_owned(),
            json,
            fact: None,
            confidence: 1.0,
            kind,
            as_of: None,
        }
    }

    fn tests_unit(module: &str, total: Option<u32>, ci: &str, at: Option<&str>) -> Unit {
        figures_unit(
            UnitKind::Tests(serde_json::from_value(serde_json::json!(module)).unwrap()),
            serde_json::json!({ "module": module, "tests_total": total, "tests_changed_14d": 0,
                "last_ci": { "status": ci, "at": at }, "pass_rate_7d": 0.5, "flaky_7d": 0.0 }),
        )
    }

    #[test]
    fn a_module_without_tests_says_it_has_none() {
        // no tests counted: whatever CI did, it ran none of this module's
        for ci in ["passed", "failed", "none"] {
            let unit = tests_unit("Auth", Some(0), ci, Some("2026-09-29T08:09:41Z"));
            assert_eq!(template(&[unit]), "Auth has no tests [f_c0t0].");
        }
        // not counted is not none
        let unknown = tests_unit("Auth", None, "passed", None);
        assert_eq!(
            template(&[unknown]),
            "Auth has an unknown number of tests, and its last CI run passed [f_c0t0]."
        );
    }

    #[test]
    fn failing_tests_say_when_the_last_run_failed() {
        let unit = tests_unit("Billing", Some(14), "failed", Some("2026-09-29T08:09:41Z"));
        assert_eq!(
            template(&[unit]),
            "Billing has 14 tests, and they are failing: the last CI run failed on 2026-09-29 08:09 UTC [f_c0t0]."
        );
        let undated = tests_unit("Billing", Some(14), "failed", None);
        assert_eq!(
            template(&[undated]),
            "Billing has 14 tests, and they are failing: the last CI run failed [f_c0t0]."
        );
        let passing = tests_unit("Billing", Some(14), "passed", Some("2026-09-29T08:09:41Z"));
        assert_eq!(
            template(&[passing]),
            "Billing has 14 tests, and its last CI run passed [f_c0t0]."
        );
    }

    #[test]
    fn the_health_sentence_gives_the_builds_time() {
        let health = |status: &str, at: Option<&str>| {
            figures_unit(
                UnitKind::Health,
                serde_json::json!({ "as_of": "2026-10-02T08:00:00Z",
                    "build": { "status": status, "at": at }, "ci_pass_rate_7d": 0.9,
                    "open_prs": { "count": 3, "oldest_days": 5 }, "todo_fixme": null,
                    "last_release": { "name": null, "at": null }, "score": 82 }),
            )
        };
        let failed = health("failed", Some("2026-10-02T07:00:00Z"));
        assert_eq!(
            template(&[failed]),
            "The last build failed on 2026-10-02 07:00 UTC, 90% of CI runs passed in the last 7 days, 3 pull requests are open, and the health score is 82 of 100 [f_c0t0]."
        );
        // a run still going has no time of its own to give
        assert!(
            template(&[health("running", Some("2026-10-02T07:00:00Z"))])
                .starts_with("The last build is still running, ")
        );
        assert!(template(&[health("none", None)]).starts_with("The last build isn't recorded, "));
    }

    #[test]
    fn the_words_after_a_number_agree_with_it() {
        for (n, en) in [
            (0, "tests"),
            (1, "test"),
            (2, "tests"),
            (5, "tests"),
            (12, "tests"),
            (22, "tests"),
        ] {
            assert_eq!(one_or_many(n, "test", "tests"), en, "{n}");
        }
    }

    /// Every count of a template answer agrees with its number: one test, two tests, 0 tests.
    #[test]
    fn every_count_of_a_template_answer_agrees_with_its_number() {
        // the module's tests; none is said in words
        let none = tests_unit("Billing", Some(0), "passed", None);
        assert_eq!(template(&[none]), "Billing has no tests [f_c0t0].");
        for (n, en) in [
            (1, "1 test"),
            (2, "2 tests"),
            (5, "5 tests"),
            (12, "12 tests"),
            (22, "22 tests"),
        ] {
            let unit = tests_unit("Billing", Some(n), "passed", None);
            assert_eq!(
                template(&[unit]),
                format!("Billing has {en}, and its last CI run passed [f_c0t0].")
            );
        }
        // one failing test fails on its own
        let one = tests_unit("Billing", Some(1), "failed", None);
        assert_eq!(
            template(&[one]),
            "Billing has 1 test, and it is failing: the last CI run failed [f_c0t0]."
        );
        let two = tests_unit("Billing", Some(2), "failed", None);
        assert_eq!(
            template(&[two]),
            "Billing has 2 tests, and they are failing: the last CI run failed [f_c0t0]."
        );

        // the open pull requests of the project's health
        let health = |prs: u64| {
            figures_unit(
                UnitKind::Health,
                serde_json::json!({ "build": { "status": "passed", "at": null },
                    "ci_pass_rate_7d": 1.0, "open_prs": { "count": prs, "oldest_days": 1 },
                    "score": 90 }),
            )
        };
        for (n, en) in [
            (0, "0 pull requests are open"),
            (1, "1 pull request is open"),
            (2, "2 pull requests are open"),
            (5, "5 pull requests are open"),
            (12, "12 pull requests are open"),
            (22, "22 pull requests are open"),
        ] {
            let english = template(&[health(n)]);
            assert!(english.contains(&format!(" 7 days, {en}, and ")), "{english}");
        }

        // a module's commits and merged pull requests
        let changes = |n: u64| {
            figures_unit(
                UnitKind::Changes(serde_json::from_value(serde_json::json!("Export")).unwrap()),
                serde_json::json!({ "module": "Export", "commits": n, "merged_prs": n }),
            )
        };
        for (n, en) in [
            (0, "Export had 0 commits and 0 merged pull requests [f_c0t0]."),
            (1, "Export had 1 commit and 1 merged pull request [f_c0t0]."),
            (2, "Export had 2 commits and 2 merged pull requests [f_c0t0]."),
            (5, "Export had 5 commits and 5 merged pull requests [f_c0t0]."),
            (12, "Export had 12 commits and 12 merged pull requests [f_c0t0]."),
            (22, "Export had 22 commits and 22 merged pull requests [f_c0t0]."),
        ] {
            assert_eq!(template(&[changes(n)]), en);
        }
        // each count by its own number
        let mixed = figures_unit(
            UnitKind::Changes(serde_json::from_value(serde_json::json!("Export")).unwrap()),
            serde_json::json!({ "module": "Export", "commits": 1, "merged_prs": 3 }),
        );
        assert_eq!(
            template(&[mixed]),
            "Export had 1 commit and 3 merged pull requests [f_c0t0]."
        );
    }

    /// The sentence of a work item says how it is tested, then what it waits on.
    #[test]
    fn a_fact_sentence_says_how_it_is_tested() {
        for (tested, en) in [
            (
                Tested::Passing,
                "CSV export in Export is done, with passing tests [f_aaaa].",
            ),
            (
                Tested::Failing,
                "CSV export in Export is done, with failing tests [f_aaaa].",
            ),
            (
                Tested::Missing,
                "CSV export in Export is done, without tests [f_aaaa].",
            ),
            (Tested::Unknown, "CSV export in Export is done [f_aaaa]."),
        ] {
            let mut unit = fact_unit("CSV export", false);
            unit.fact.as_mut().unwrap().tested = tested;
            assert_eq!(template(&[unit]), en);
        }
        // and what it waits on after that
        let mut unit = fact_unit("CSV export", false);
        let fact = unit.fact.as_mut().unwrap();
        fact.status = Status::Blocked;
        fact.tested = Tested::Failing;
        fact.waiting_on = Some(WaitingOn::Review);
        assert_eq!(
            template(&[unit]),
            "CSV export in Export is blocked, with failing tests, waiting on a review [f_aaaa]."
        );
    }

    #[test]
    fn four_modules_failing_fit_in_one_answer() {
        let units: Vec<Unit> = ["Auth", "Billing", "Export", "Invoicing"]
            .iter()
            .map(|m| tests_unit(m, Some(140), "failed", Some("2026-09-29T08:09:41Z")))
            .collect();
        let text = template(&units);
        assert_eq!(within_words(sentences(&text)).len(), 4, "{text}");
    }

    #[test]
    fn a_template_sentence_names_the_module_once_and_starts_with_a_capital() {
        assert_eq!(
            template(&[fact_unit("Work item in Export", true)]),
            "A work item in Export is done [f_aaaa]."
        );
        assert_eq!(
            template(&[fact_unit(WITHHELD, false)]),
            "A work item in Export is done [f_aaaa]."
        );
        assert_eq!(
            template(&[fact_unit("the client wants a digest", false)]),
            "The client wants a digest in Export is done [f_aaaa]."
        );
    }

    #[test]
    fn the_prompt_carries_the_facts_and_the_question_last() {
        let text = prompt(
            "FACTS: {facts_json}\nQUESTION: {question}",
            &serde_json::json!([{ "id": "f_aaaa" }]),
            "what's done?",
        );
        assert_eq!(text, "FACTS: [{\"id\":\"f_aaaa\"}]\nQUESTION: what's done?");
    }
}
