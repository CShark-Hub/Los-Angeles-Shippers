//! Redaction, and the only place [`SafeText`], [`DisplayName`] and [`RedactedQuestion`] are built.
//!
//! The three types keep their field private to this module, so no other code, in this crate or
//! outside it, can wrap a raw string in them. The order is fixed: normalise whitespace → redact
//! → shorten → check. A text that fails the check is never repaired into something acceptable;
//! the caller gets an error and puts a neutral placeholder in its place. A question is never
//! shortened: cut short, it may ask something else.

mod rules;

use std::borrow::Cow;
use std::fmt;
use std::sync::LazyLock;

use regex::{NoExpand, Regex, RegexBuilder};
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

use crate::ids::ModuleName;
pub use rules::{
    DISPLAY_NAME_MAX_CHARS, REDACTED_QUESTION_MAX_CHARS, SAFE_TEXT_MAX_CHARS, TextRejection,
    validate_display_name, validate_redacted_question, validate_safe_text,
};
use rules::{DISPLAY_NAME_PATTERN, normalize_whitespace, truncate_chars};

/// One redaction rule: a regular expression and the literal text that replaces each match.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, utoipa::ToSchema)]
#[serde(deny_unknown_fields)]
pub struct RedactRule {
    pub pattern: String,
    pub replace: String,
}

/// A redaction rule whose pattern doesn't compile. It names the rule by position, never its text.
#[derive(Debug, thiserror::Error)]
#[error("redaction rule {index} is not a valid regular expression")]
pub struct RedactorError {
    pub index: usize,
    #[source]
    pub source: regex::Error,
}

/// E-mail addresses are always redacted, whatever the configuration says.
static BUILTIN_EMAIL: LazyLock<Regex> = LazyLock::new(|| {
    #[allow(clippy::expect_used, reason = "a literal pattern, covered by the tests below")]
    Regex::new(r"[\w.+-]+@[\w-]+\.[\w.-]+").expect("built-in pattern compiles")
});

/// A compiled, ordered list of redaction rules. Build it once per project from the global rules
/// followed by the manifest's, and share it with every path that emits text.
#[derive(Debug, Clone, Default)]
pub struct Redactor {
    rules: Vec<(Regex, String)>,
}

impl Redactor {
    /// Compiles the rules in order. Each pattern is size-limited; matching runs in linear time.
    pub fn new(rules: &[RedactRule]) -> Result<Self, RedactorError> {
        let rules = rules
            .iter()
            .enumerate()
            .map(|(index, rule)| {
                RegexBuilder::new(&rule.pattern)
                    .size_limit(1 << 20)
                    .build()
                    .map(|re| (re, rule.replace.clone()))
                    .map_err(|source| RedactorError { index, source })
            })
            .collect::<Result<_, _>>()?;
        Ok(Self { rules })
    }

    /// Applies the built-in e-mail rule, then every configured rule in order.
    pub fn apply<'a>(&self, text: &'a str) -> Cow<'a, str> {
        let mut out = Cow::Borrowed(text);
        if BUILTIN_EMAIL.is_match(&out) {
            out = Cow::Owned(BUILTIN_EMAIL.replace_all(&out, NoExpand("[email]")).into_owned());
        }
        for (re, replacement) in &self.rules {
            if re.is_match(&out) {
                out = Cow::Owned(re.replace_all(&out, NoExpand(replacement)).into_owned());
            }
        }
        out
    }
}

/// One line of redacted, checked text: at most 120 characters, and no code, paths, identifiers,
/// URLs, e-mail addresses or secrets. Built only by [`safe_text`] and [`withheld_title`].
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SafeText(String);

impl SafeText {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A person's display name: redacted, at most 60 characters, letters, spaces, hyphens and
/// apostrophes only. Never an e-mail address or a login. Built only by [`display_name`].
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct DisplayName(String);

impl DisplayName {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// The asker's question after the project's redaction rules, on one line of at most 1,000
/// characters: the argument of `ask_code`, never part of a reply. Built only by
/// [`redacted_question`]. `Debug` prints its length, never the question, so a plan written to a
/// log carries no question text.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct RedactedQuestion(String);

impl RedactedQuestion {
    /// The question as the probe passes it to the code reader.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Builds a [`SafeText`] from raw repository text.
pub fn safe_text(raw: &str, redactor: &Redactor) -> Result<SafeText, TextRejection> {
    let redacted = normalize_whitespace(&redactor.apply(&normalize_whitespace(raw)));
    let shortened = truncate_chars(&redacted, SAFE_TEXT_MAX_CHARS);
    validate_safe_text(&shortened)?;
    Ok(SafeText(shortened.into_owned()))
}

/// Builds a [`DisplayName`] from a profile name. Logins and e-mail addresses never pass.
pub fn display_name(raw: &str, redactor: &Redactor) -> Result<DisplayName, TextRejection> {
    let redacted = normalize_whitespace(&redactor.apply(&normalize_whitespace(raw)));
    validate_display_name(&redacted)?;
    Ok(DisplayName(redacted))
}

/// Builds a [`RedactedQuestion`] from the asker's question: whitespace normalised (a newline
/// becomes a space), the redaction rules applied, then checked. A question that is too long after
/// redaction is refused, never shortened.
pub fn redacted_question(raw: &str, redactor: &Redactor) -> Result<RedactedQuestion, TextRejection> {
    let redacted = normalize_whitespace(&redactor.apply(&normalize_whitespace(raw)));
    validate_redacted_question(&redacted)?;
    Ok(RedactedQuestion(redacted))
}

/// The title of an item whose own title failed a rule or a check: `Work item in <module>`.
pub fn withheld_title(module: &ModuleName) -> SafeText {
    let title = format!("Work item in {module}");
    match validate_safe_text(&title) {
        Ok(()) => SafeText(title),
        Err(_) => SafeText("Work item".to_owned()),
    }
}

impl fmt::Display for SafeText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for SafeText {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "SafeText({:?})", self.0)
    }
}

impl Serialize for SafeText {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// Checks the shape rules again, so a probe can't smuggle raw text through JSON. That is all it
/// can check: the project's redaction rules aren't known here, so the core applies them again
/// when it takes a reply in.
impl<'de> Deserialize<'de> for SafeText {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        validate_safe_text(&raw)
            .map_err(|rule| serde::de::Error::custom(format!("invalid SafeText: {rule}")))?;
        Ok(Self(raw))
    }
}

impl JsonSchema for SafeText {
    fn schema_name() -> Cow<'static, str> {
        "SafeText".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "format": "safe-text",
            "minLength": 1,
            "maxLength": SAFE_TEXT_MAX_CHARS,
            "description": "One line of redacted text: no code, paths, identifiers, URLs, e-mail addresses or secrets."
        })
    }
}

impl fmt::Debug for RedactedQuestion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "RedactedQuestion(<{} characters>)", self.0.chars().count())
    }
}

impl Serialize for RedactedQuestion {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// Checks the shape rules again, so a question that skipped redaction can't pass as one through
/// JSON. The project's redaction rules aren't known here; the core applies them when it builds
/// the question.
impl<'de> Deserialize<'de> for RedactedQuestion {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        validate_redacted_question(&raw)
            .map_err(|rule| serde::de::Error::custom(format!("invalid RedactedQuestion: {rule}")))?;
        Ok(Self(raw))
    }
}

impl JsonSchema for RedactedQuestion {
    fn schema_name() -> Cow<'static, str> {
        "RedactedQuestion".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "format": "redacted-question",
            "minLength": 1,
            "maxLength": REDACTED_QUESTION_MAX_CHARS,
            "description": "The asker's question after the redaction rules, on one line: a tool argument, never part of a reply."
        })
    }
}

impl fmt::Display for DisplayName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for DisplayName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DisplayName({:?})", self.0)
    }
}

impl Serialize for DisplayName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

/// Checks the shape rules again, so a probe can't smuggle raw text through JSON. That is all it
/// can check: the project's redaction rules aren't known here, so the core applies them again
/// when it takes a reply in.
impl<'de> Deserialize<'de> for DisplayName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        validate_display_name(&raw)
            .map_err(|rule| serde::de::Error::custom(format!("invalid DisplayName: {rule}")))?;
        Ok(Self(raw))
    }
}

impl JsonSchema for DisplayName {
    fn schema_name() -> Cow<'static, str> {
        "DisplayName".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "format": "display-name",
            "minLength": 1,
            "maxLength": DISPLAY_NAME_MAX_CHARS,
            "pattern": DISPLAY_NAME_PATTERN
        })
    }
}

#[cfg(test)]
mod tests {
    use proptest::prelude::*;

    use super::*;

    fn client_rules() -> Redactor {
        Redactor::new(&[
            RedactRule {
                pattern: r"(?i)\bmegabank\b".into(),
                replace: "the client".into(),
            },
            RedactRule {
                pattern: r"(?i)\bacme(\s+corp)?\b".into(),
                replace: "the client".into(),
            },
        ])
        .unwrap()
    }

    #[test]
    fn redacts_client_names_and_emails() {
        let r = client_rules();
        let t = safe_text("Export for MegaBank, ask ops@example.com", &r).unwrap();
        assert_eq!(t.as_str(), "Export for the client, ask [email]");
        let t = safe_text("Acme Corp   onboarding\nflow", &r).unwrap();
        assert_eq!(t.as_str(), "the client onboarding flow");
    }

    #[test]
    fn replacement_is_literal() {
        let r = Redactor::new(&[RedactRule {
            pattern: "(x)".into(),
            replace: "$1$1".into(),
        }])
        .unwrap();
        assert_eq!(r.apply("x"), "$1$1");
    }

    #[test]
    fn a_bad_pattern_is_named_by_position() {
        let err = Redactor::new(&[
            RedactRule {
                pattern: "ok".into(),
                replace: "fine".into(),
            },
            RedactRule {
                pattern: "(unclosed".into(),
                replace: "x".into(),
            },
        ])
        .unwrap_err();
        assert_eq!(err.index, 1);
        assert!(!err.to_string().contains("unclosed"));
    }

    #[test]
    fn rejects_rather_than_repairs() {
        let r = Redactor::default();
        assert_eq!(safe_text("Fix src/Invoices/Totals", &r), Err(TextRejection::Path));
        assert_eq!(
            safe_text("Rename CsvExporter", &r),
            Err(TextRejection::Identifier)
        );
        assert_eq!(safe_text("   ", &r), Err(TextRejection::Empty));
    }

    #[test]
    fn long_titles_are_shortened() {
        let raw = "Improve the monthly invoice overview screen ".repeat(5);
        let t = safe_text(&raw, &Redactor::default()).unwrap();
        assert!(t.as_str().chars().count() <= SAFE_TEXT_MAX_CHARS);
        assert!(t.as_str().ends_with('…'));
    }

    #[test]
    fn display_names_never_carry_logins() {
        let r = client_rules();
        assert_eq!(display_name(" Anna  Smith ", &r).unwrap().as_str(), "Anna Smith");
        assert_eq!(display_name("anna.smith", &r), Err(TextRejection::NameCharacters));
        assert_eq!(
            display_name("anna@example.com", &r),
            Err(TextRejection::NameCharacters)
        );
        assert_eq!(display_name(&"Anna ".repeat(13), &r), Err(TextRejection::TooLong));
    }

    #[test]
    fn withheld_titles_name_the_module() {
        let m = ModuleName::parse("Invoicing").unwrap();
        assert_eq!(withheld_title(&m).as_str(), "Work item in Invoicing");
        let odd = ModuleName::parse("GitHub Sync").unwrap();
        assert_eq!(withheld_title(&odd).as_str(), "Work item");
    }

    #[test]
    fn deserialize_checks_again() {
        assert!(serde_json::from_str::<SafeText>("\"Invoice list\"").is_ok());
        assert!(serde_json::from_str::<SafeText>("\"src/main.rs\"").is_err());
        assert!(serde_json::from_str::<SafeText>("\"line one\\nline two\"").is_err());
        assert!(serde_json::from_str::<DisplayName>("\"root@host\"").is_err());
    }

    #[test]
    fn a_redacted_question_is_redacted_and_one_line() {
        let r = client_rules();
        let q = redacted_question("What did MegaBank ask\nops@example.com   about the export?", &r).unwrap();
        assert_eq!(q.as_str(), "What did the client ask [email] about the export?");
        // a question may name code, unlike a SafeText: the reader answers it, nobody reads it back
        let q = redacted_question(" Where is  src/Export/CsvWriter.cs used?\r\n", &r).unwrap();
        assert_eq!(q.as_str(), "Where is src/Export/CsvWriter.cs used?");
        assert_eq!(format!("{q:?}"), "RedactedQuestion(<38 characters>)");
        let longest = "a".repeat(REDACTED_QUESTION_MAX_CHARS);
        assert_eq!(
            redacted_question(&longest, &r).unwrap().as_str().chars().count(),
            REDACTED_QUESTION_MAX_CHARS
        );
    }

    #[test]
    fn a_redacted_question_is_refused_not_cut() {
        let r = Redactor::default();
        let long = "a".repeat(REDACTED_QUESTION_MAX_CHARS + 1);
        assert_eq!(redacted_question(&long, &r), Err(TextRejection::TooLong));
        assert_eq!(redacted_question("", &r), Err(TextRejection::Empty));
        assert_eq!(redacted_question(" \n\t ", &r), Err(TextRejection::Empty));
        assert_eq!(
            redacted_question("Is it done\u{0007}?", &r),
            Err(TextRejection::ControlCharacter)
        );
        assert_eq!(
            redacted_question("Is it \u{202E}done?", &r),
            Err(TextRejection::ControlCharacter)
        );
        // redaction that makes a question too long refuses it as well
        let growing = Redactor::new(&[RedactRule {
            pattern: "x".into(),
            replace: "the client".into(),
        }])
        .unwrap();
        assert_eq!(
            redacted_question(&"x".repeat(200), &growing),
            Err(TextRejection::TooLong)
        );
    }

    #[test]
    fn a_redacted_question_is_checked_again_when_read() {
        let q: RedactedQuestion = serde_json::from_str(r#""How is the export built?""#).unwrap();
        assert_eq!(q.as_str(), "How is the export built?");
        assert_eq!(
            serde_json::to_string(&q).unwrap(),
            r#""How is the export built?""#
        );
        let long = serde_json::to_string(&"a".repeat(REDACTED_QUESTION_MAX_CHARS + 1)).unwrap();
        for bad in [
            r#""""#.to_owned(),
            r#""line one\nline two""#.to_owned(),
            "\"line one\u{2028}line two\"".to_owned(),
            r#"" padded""#.to_owned(),
            "\"hidden\u{202E}text\"".to_owned(),
            r#""bell\u0007""#.to_owned(),
            long,
        ] {
            assert!(serde_json::from_str::<RedactedQuestion>(&bad).is_err(), "{bad:?}");
        }
    }

    /// Raw text with a forbidden shape planted in it, so the property below meets them often.
    fn planted() -> impl Strategy<Value = String> {
        let shape = prop_oneof![
            "https?://[a-z]{1,8}\\.(com|dev|pl)(/[a-z]{0,6})?",
            "www\\.[a-z]{2,8}\\.[a-z]{2,3}",
            "[a-z]{1,8}@[a-z]{1,8}\\.[a-z]{2,3}",
            "@[a-z]{2,10}",
            "(src|lib|app)/[A-Za-z]{1,10}(/[A-Za-z]{1,10})?(\\.rs|\\.cs)?",
            "[A-Za-z]:\\\\[a-z]{1,8}\\\\[a-z]{1,8}",
            "`[a-z ]{0,10}`",
            "[A-Z][a-z]{2,8}[A-Z][a-z]{2,8}",
            "[a-z]{2,8}_[a-z]{2,8}",
            "[a-z]{2,8}\n[a-z]{2,8}",
            "[0-9a-f]{32}",
        ];
        ("[A-Za-z ,.]{0,40}", shape, "[A-Za-z ,.]{0,40}").prop_map(|(a, s, b)| format!("{a}{s}{b}"))
    }

    /// The forbidden shapes, stated independently of the rules they test.
    fn assert_no_forbidden_shape(text: &SafeText) -> Result<(), TestCaseError> {
        static URL: LazyLock<Regex> = LazyLock::new(|| Regex::new(r"(?i)://|\bwww\.\w+\.\w").unwrap());
        let s = text.as_str();
        prop_assert!(!s.contains(['\n', '\r', '`']), "newline or backtick in {s:?}");
        let invisible = |c: char| {
            matches!(c,
                '\u{00AD}' | '\u{061C}' | '\u{115F}' | '\u{1160}' | '\u{180E}' | '\u{200B}'..='\u{200F}'
                | '\u{202A}'..='\u{202E}' | '\u{2060}'..='\u{206F}' | '\u{3164}' | '\u{FE00}'..='\u{FE0F}'
                | '\u{FEFF}' | '\u{FFA0}' | '\u{E0000}'..='\u{E007F}' | '\u{E0100}'..='\u{E01EF}')
        };
        prop_assert!(!s.chars().any(invisible), "invisible character in {s:?}");
        prop_assert!(!URL.is_match(s), "URL in {s:?}");
        let chars: Vec<char> = s.chars().collect();
        let wordish = |c: char| c.is_alphanumeric() || matches!(c, '_' | '.' | '~' | '-');
        let slash = |c: char| {
            matches!(
                c,
                '/' | '\\' | '\u{2215}' | '\u{2044}' | '\u{FF0F}' | '\u{29F8}' | '\u{FF3C}'
            )
        };
        let path_like = chars
            .windows(3)
            .any(|w| wordish(w[0]) && slash(w[1]) && wordish(w[2]));
        prop_assert!(!path_like, "path-like token in {s:?}");
        let address = chars
            .windows(2)
            .any(|w| w[0] == '@' && (w[1].is_alphanumeric() || w[1] == '_'));
        prop_assert!(!address, "e-mail address or handle in {s:?}");
        prop_assert!(s.chars().count() <= SAFE_TEXT_MAX_CHARS);
        Ok(())
    }

    proptest! {
        #[test]
        // printable text plus every invisible kind: `\PC` alone leaves out all of category C
        fn random_text_never_yields_a_forbidden_shape(
            raw in "[\\PC\\p{Cf}\\p{Variation_Selector}\\x{115F}\\x{1160}\\x{3164}\\x{FFA0}]{0,200}"
        ) {
            if let Ok(text) = safe_text(&raw, &Redactor::default()) {
                assert_no_forbidden_shape(&text)?;
            }
        }

        #[test]
        fn planted_shapes_never_survive(raw in planted()) {
            if let Ok(text) = safe_text(&raw, &client_rules()) {
                assert_no_forbidden_shape(&text)?;
            }
        }

        /// The advertised schema and the check accept the same names.
        #[test]
        fn display_name_check_matches_its_schema(raw in "[\\p{L}\\p{M}\\p{Nl} '’-]{0,12}") {
            let schema = serde_json::to_value(schemars::schema_for!(DisplayName)).unwrap();
            let pattern = Regex::new(schema["pattern"].as_str().unwrap()).unwrap();
            let length_ok = (1..=DISPLAY_NAME_MAX_CHARS).contains(&raw.chars().count());
            prop_assert_eq!(validate_display_name(&raw).is_ok(), length_ok && pattern.is_match(&raw), "{:?}", raw);
        }

        #[test]
        fn safe_text_survives_a_json_round_trip(raw in "[A-Za-z ,.!?-]{1,150}") {
            if let Ok(text) = safe_text(&raw, &Redactor::default()) {
                let back: SafeText = serde_json::from_str(&serde_json::to_string(&text).unwrap()).unwrap();
                prop_assert_eq!(back, text);
            }
        }
    }
}
