//! The shape rules every `SafeText`, `DisplayName` and `RedactedQuestion` must meet. They only
//! check; building the types is left to the parent module.

use std::borrow::Cow;
use std::sync::LazyLock;

use regex::Regex;

/// Longest `SafeText`, in characters.
pub const SAFE_TEXT_MAX_CHARS: usize = 120;
/// Longest `DisplayName`, in characters.
pub const DISPLAY_NAME_MAX_CHARS: usize = 60;
/// Longest `RedactedQuestion`, in characters. A longer question is refused, never shortened.
pub const REDACTED_QUESTION_MAX_CHARS: usize = 1000;

/// Why a text was rejected. It names the rule, never the offending content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
pub enum TextRejection {
    #[error("the text is empty")]
    Empty,
    #[error("the text is longer than allowed")]
    TooLong,
    #[error("the text contains control or invisible formatting characters")]
    ControlCharacter,
    #[error("the text has leading, trailing or repeated spaces")]
    Spacing,
    #[error("the text contains a backtick")]
    Backtick,
    #[error("the text contains a URL")]
    Url,
    #[error("the text contains an e-mail address or a handle")]
    Email,
    #[error("the text looks like a file path")]
    Path,
    #[error("the text contains a file name")]
    FileName,
    #[error("the text contains a code identifier")]
    Identifier,
    #[error("the text contains code punctuation")]
    CodePunctuation,
    #[error("the text looks like it contains a secret")]
    Secret,
    #[error("the text contains characters that are not allowed in a name")]
    NameCharacters,
}

fn re(pattern: &str) -> Regex {
    #[allow(
        clippy::expect_used,
        reason = "the patterns are literals of this module, covered by its tests"
    )]
    Regex::new(pattern).expect("built-in pattern compiles")
}

/// A URL or a host: a scheme, `www.`, a known top-level domain, an IPv4 address, or a name that
/// starts with a letter followed by a port (`core:8081`; a time such as `10:30` has no letter).
static URL: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(?:https?|ftp|file|ssh|git|wss?)://|\bwww\.[a-z0-9-]+\.|\b[a-z0-9-]+\.(?:com|net|org|io|dev|app|pl|eu|co|ai|cloud|internal|local)\b|\b(?:\d{1,3}\.){3}\d{1,3}\b|\b[a-z][a-z0-9-]*(?:\.[a-z0-9-]+)*:\d{2,5}\b",
    )
});
/// An e-mail address or a handle: any `@` that runs into something other than a space. Not `\w`:
/// it leaves out letters and numerals such as `½`, and those must not pass either.
static EMAIL: LazyLock<Regex> = LazyLock::new(|| re(r"@\S"));
/// A slash or a backslash, and the characters that look like one.
const SLASHES: &str = r"/\\\x{2215}\x{2044}\x{FF0F}\x{29F8}\x{FF3C}";
/// A path: a slash with something other than a space on both sides, one that starts a word, one
/// that ends a word (`Invoices/ folder`), and a drive letter.
static PATH: LazyLock<Regex> = LazyLock::new(|| {
    re(&format!(
        r"\S[{SLASHES}]\S|(?:^|\s)[{SLASHES}~]\S|\S[{SLASHES}](?:\s|$)|\b[A-Za-z]:[{SLASHES}]"
    ))
});
/// Characters that control, hide or reorder text: every control and format character (the tag
/// characters, the bidirectional controls, the BOM, the soft hyphen, …), every default-ignorable
/// code point (the variation selectors and the Hangul fillers among them).
static INVISIBLE: LazyLock<Regex> =
    LazyLock::new(|| re(r"[\p{Cc}\p{Cf}\p{Default_Ignorable_Code_Point}\p{Variation_Selector}]"));
/// A display name: words of letters, hyphens and apostrophes, separated by single spaces, at least
/// one of them with a letter. The `JsonSchema` of `DisplayName` advertises exactly this pattern.
pub(super) const DISPLAY_NAME_PATTERN: &str =
    r"^(?:[\p{L}'’-]+ )*[\p{L}'’-]*\p{L}[\p{L}'’-]*(?: [\p{L}'’-]+)*$";
static DISPLAY_NAME: LazyLock<Regex> = LazyLock::new(|| re(DISPLAY_NAME_PATTERN));
static FILE_NAME: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b[\w-]+\.(?:rs|cs|csproj|sln|fs|vb|ts|tsx|js|jsx|mjs|cjs|py|java|kt|kts|go|rb|php|cpp|cc|cxx|c|h|hpp|swift|m|mm|scala|sql|json|ya?ml|toml|xml|html?|css|scss|less|sh|bash|ps1|psm1|bat|cmd|md|lock|env|ini|cfg|conf|config|dll|exe|so|dylib|jar|war|gradle|pom|dockerfile|tf|proto|graphql|svelte|vue)\b",
    )
});
static SNAKE: LazyLock<Regex> =
    LazyLock::new(|| re(r"\b[A-Za-z][A-Za-z0-9]*_[A-Za-z0-9_]+\b|\b_[A-Za-z0-9]"));
static CAMEL: LazyLock<Regex> = LazyLock::new(|| re(r"\b[a-z]+[A-Z][A-Za-z0-9]*\b"));
static PASCAL: LazyLock<Regex> = LazyLock::new(|| re(r"\b[A-Z][a-z0-9]+[A-Z][A-Za-z0-9]*\b"));
static UPPER_PREFIX: LazyLock<Regex> = LazyLock::new(|| re(r"\b[A-Z]{2,}[a-z]{2,}[A-Za-z0-9]*\b"));
static DOTTED: LazyLock<Regex> = LazyLock::new(|| re(r"\b[A-Za-z_][A-Za-z0-9_]+\.[A-Za-z_][A-Za-z0-9_]+\b"));
static CODE_PUNCT: LazyLock<Regex> =
    LazyLock::new(|| re(r"::|->|=>|\(\)|\[\]|[{};<>$|\\^=]|&&|\|\||#(?:include|define|region|pragma)\b"));
static SECRET: LazyLock<Regex> = LazyLock::new(|| {
    re(
        r"(?i)\b(?:ghp|gho|ghu|ghs|ghr)_[A-Za-z0-9]{8,}|github_pat_|\bsk-[A-Za-z0-9_-]{8,}|\bsk_(?:live|test)_|\bAKIA[0-9A-Z]{12,}|\bxox[abprs]-|\bmcy_[A-Za-z0-9]|\bAIza[0-9A-Za-z_-]{10,}|-----BEGIN|\bpassword\s*[:=]|\b[A-Fa-f0-9]{16,}\b",
    )
});
static LONG_TOKEN: LazyLock<Regex> = LazyLock::new(|| re(r"[A-Za-z0-9+/=_-]{24,}"));

/// A long token that mixes letters and digits reads like a key, a hash or an id.
fn looks_like_secret_token(text: &str) -> bool {
    LONG_TOKEN.find_iter(text).any(|m| {
        let s = m.as_str();
        s.chars().any(|c| c.is_ascii_digit()) && s.chars().any(|c| c.is_ascii_alphabetic())
    })
}

/// Collapses every run of whitespace, newlines included, into one space and trims the ends.
pub(super) fn normalize_whitespace(raw: &str) -> String {
    raw.split_whitespace().collect::<Vec<_>>().join(" ")
}

/// Shortens `text` to at most `max` characters, cutting at a word boundary and adding an ellipsis.
pub(super) fn truncate_chars(text: &str, max: usize) -> Cow<'_, str> {
    if text.chars().count() <= max {
        return Cow::Borrowed(text);
    }
    let cut: String = text.chars().take(max.saturating_sub(1)).collect();
    let cut = match cut.rfind(' ') {
        Some(pos) if pos > max / 2 => cut[..pos].to_owned(),
        _ => cut,
    };
    Cow::Owned(format!("{}…", cut.trim_end()))
}

/// Checks the rules every `SafeText` must meet. Redaction runs before this, never after.
pub fn validate_safe_text(text: &str) -> Result<(), TextRejection> {
    if text.trim().is_empty() {
        return Err(TextRejection::Empty);
    }
    if text.chars().count() > SAFE_TEXT_MAX_CHARS {
        return Err(TextRejection::TooLong);
    }
    if INVISIBLE.is_match(text) {
        return Err(TextRejection::ControlCharacter);
    }
    if text.trim() != text || text.contains("  ") || text.chars().any(|c| c.is_whitespace() && c != ' ') {
        return Err(TextRejection::Spacing);
    }
    if text.contains('`') {
        return Err(TextRejection::Backtick);
    }
    if SECRET.is_match(text) || looks_like_secret_token(text) {
        return Err(TextRejection::Secret);
    }
    if EMAIL.is_match(text) {
        return Err(TextRejection::Email);
    }
    if URL.is_match(text) {
        return Err(TextRejection::Url);
    }
    if FILE_NAME.is_match(text) {
        return Err(TextRejection::FileName);
    }
    if PATH.is_match(text) {
        return Err(TextRejection::Path);
    }
    if CODE_PUNCT.is_match(text) {
        return Err(TextRejection::CodePunctuation);
    }
    if [&SNAKE, &CAMEL, &PASCAL, &UPPER_PREFIX, &DOTTED]
        .iter()
        .any(|rule| rule.is_match(text))
    {
        return Err(TextRejection::Identifier);
    }
    Ok(())
}

/// Checks the rules every `RedactedQuestion` must meet: one line of 1 to 1,000 characters, single
/// spaces only, and no control or invisible formatting character. Redaction runs before this.
pub fn validate_redacted_question(text: &str) -> Result<(), TextRejection> {
    if text.trim().is_empty() {
        return Err(TextRejection::Empty);
    }
    if text.chars().count() > REDACTED_QUESTION_MAX_CHARS {
        return Err(TextRejection::TooLong);
    }
    if INVISIBLE.is_match(text) {
        return Err(TextRejection::ControlCharacter);
    }
    if text.trim() != text || text.contains("  ") || text.chars().any(|c| c.is_whitespace() && c != ' ') {
        return Err(TextRejection::Spacing);
    }
    Ok(())
}

/// Checks the rules every `DisplayName` must meet.
pub fn validate_display_name(text: &str) -> Result<(), TextRejection> {
    if text.trim().is_empty() {
        return Err(TextRejection::Empty);
    }
    if text.chars().count() > DISPLAY_NAME_MAX_CHARS {
        return Err(TextRejection::TooLong);
    }
    if text.trim() != text || text.contains("  ") {
        return Err(TextRejection::Spacing);
    }
    if !DISPLAY_NAME.is_match(text) {
        return Err(TextRejection::NameCharacters);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_plain_titles() {
        for ok in [
            "Invoice list with filters",
            "Fix rounding in totals for #412",
            "Export to spreadsheet, second pass",
            "Invoices: filter by date",
            "Add login throttling (phase 2)",
            "Merge the open PRs and APIs",
            "e.g. monthly summary",
            "Version 1.2 release notes",
            "Delta — invoicing",
            // a time and a three-part version are neither a port nor an address
            "Batch moved to 10:30",
            "Release 1.4.0 notes",
            "Fix totals 50 / 50",
        ] {
            assert_eq!(validate_safe_text(ok), Ok(()), "{ok}");
        }
    }

    #[test]
    fn rejects_code_paths_and_secrets() {
        let cases = [
            ("See `ExportCsv` for details", TextRejection::Backtick),
            ("Move src/Export/CsvWriter", TextRejection::Path),
            ("Update C:\\work\\notes", TextRejection::Path),
            ("Refactor InvoiceExporter", TextRejection::Identifier),
            ("Rename export_csv_rows", TextRejection::Identifier),
            ("Call exportRows twice", TextRejection::Identifier),
            ("Split HTTPServer in two", TextRejection::Identifier),
            ("Touch settings.json", TextRejection::FileName),
            ("Use invoice.total directly", TextRejection::Identifier),
            ("fn main() { }", TextRejection::CodePunctuation),
            ("Read https://example.com/x", TextRejection::Url),
            ("See www.example.org", TextRejection::Url),
            ("Ping anna@example.com", TextRejection::Email),
            ("Thanks @someone", TextRejection::Email),
            ("Key ghp_abcdefghijklmnop", TextRejection::Secret),
            ("Hash 9f86d081884c7d659a2feaa0c55ad015", TextRejection::Secret),
            ("Token Zm9vYmFyYmF6cXV4MTIzNDU2Nzg5", TextRejection::Secret),
            ("two\nlines", TextRejection::ControlCharacter),
            ("hidden\u{202E}text", TextRejection::ControlCharacter),
            // characters outside `\w` on either side of a slash or after an `@`
            ("Split \u{11FC0}/A", TextRejection::Path),
            ("Half ½/b done", TextRejection::Path),
            ("Serve x://y", TextRejection::Path),
            ("Ask (/) twice", TextRejection::Path),
            ("Ping a@½x", TextRejection::Email),
            (" padded", TextRejection::Spacing),
            ("double  space", TextRejection::Spacing),
            ("", TextRejection::Empty),
        ];
        for (text, expected) in cases {
            assert_eq!(validate_safe_text(text), Err(expected), "{text:?}");
        }
    }

    #[test]
    fn rejects_format_characters_variation_selectors_and_fillers() {
        // tag characters are invisible, yet they spell text: here a path after an innocent title
        let tags: String = "src/main"
            .chars()
            .map(|c| char::from_u32(0xE0000 + c as u32).unwrap())
            .collect();
        let mut cases = vec![format!("Fix export{tags}")];
        for c in [
            '\u{061C}',
            '\u{180E}',
            '\u{206A}',
            '\u{206F}',
            '\u{E0001}',
            '\u{E007F}',
            '\u{FE00}',
            '\u{FE0F}',
            '\u{E0100}',
            '\u{E01EF}',
            '\u{115F}',
            '\u{1160}',
            '\u{3164}',
            '\u{FFA0}',
        ] {
            cases.push(format!("Fix{c} export"));
        }
        for text in cases {
            assert_eq!(
                validate_safe_text(&text),
                Err(TextRejection::ControlCharacter),
                "{text:?}"
            );
        }
    }

    #[test]
    fn rejects_addresses_ports_trailing_and_look_alike_slashes() {
        let cases = [
            ("Deploy to 10.0.12.5", TextRejection::Url),
            ("Reach 192.168.0.1 first", TextRejection::Url),
            ("Restart localhost:8080", TextRejection::Url),
            ("Probe talks to core:8081", TextRejection::Url),
            ("Move the Invoices/ folder", TextRejection::Path),
            ("Tidy the docs/", TextRejection::Path),
            ("Split src\u{2215}main", TextRejection::Path),
            ("Split src\u{2044}main", TextRejection::Path),
            ("Split src\u{FF0F}main", TextRejection::Path),
            ("Split src\u{29F8}main", TextRejection::Path),
            ("Split src\u{FF3C}main", TextRejection::Path),
        ];
        for (text, expected) in cases {
            assert_eq!(validate_safe_text(text), Err(expected), "{text:?}");
        }
    }

    #[test]
    fn display_names_reject_combining_marks() {
        // U+0345 is alphabetic to Rust, but a mark, not a letter, to the schema's \p{L}
        assert_eq!(
            validate_display_name("Ana\u{0345}"),
            Err(TextRejection::NameCharacters)
        );
        // a Devanagari vowel sign is a spacing mark: the same, the schema has no room for it
        assert_eq!(
            validate_display_name("\u{0905}\u{0928}\u{093F}\u{0932}"),
            Err(TextRejection::NameCharacters)
        );
    }

    #[test]
    fn rejects_overlong_text() {
        let long = "word ".repeat(40);
        assert_eq!(validate_safe_text(long.trim()), Err(TextRejection::TooLong));
    }

    #[test]
    fn display_names() {
        assert_eq!(validate_display_name("Anna Smith"), Ok(()));
        assert_eq!(validate_display_name("Jean-Luc O'Neil"), Ok(()));
        assert_eq!(validate_display_name("Zoë Brontë"), Ok(()));
        assert_eq!(
            validate_display_name("anna@example.com"),
            Err(TextRejection::NameCharacters)
        );
        assert_eq!(
            validate_display_name("dev123"),
            Err(TextRejection::NameCharacters)
        );
        assert_eq!(validate_display_name("---"), Err(TextRejection::NameCharacters));
        assert_eq!(
            validate_display_name(&"a".repeat(61)),
            Err(TextRejection::TooLong)
        );
    }

    #[test]
    fn truncation_keeps_words() {
        let short = truncate_chars("alpha beta gamma delta epsilon zeta eta theta", 20);
        assert!(short.chars().count() <= 20);
        assert!(short.ends_with('…'));
        assert_eq!(truncate_chars("short", 20), "short");
    }
}
