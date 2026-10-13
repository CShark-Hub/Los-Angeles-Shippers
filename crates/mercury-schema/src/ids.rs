//! Identifiers with a fixed shape. None of them can carry free text.

use std::borrow::Cow;
use std::fmt;
use std::sync::LazyLock;

use regex::Regex;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};

/// A value that doesn't have the identifier's shape. It never repeats the value.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("invalid {kind}: {reason}")]
pub struct IdError {
    pub kind: &'static str,
    pub reason: &'static str,
}

fn re(pattern: &str) -> Regex {
    #[allow(
        clippy::expect_used,
        reason = "the patterns are literals of this module, covered by its tests"
    )]
    Regex::new(pattern).expect("built-in pattern compiles")
}

macro_rules! string_id {
    ($(#[$meta:meta])* $name:ident, $kind:literal, $pattern:literal, $format:literal) => {
        $(#[$meta])*
        #[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
        pub struct $name(String);

        impl $name {
            pub fn parse(raw: &str) -> Result<Self, IdError> {
                static PATTERN: LazyLock<Regex> = LazyLock::new(|| re($pattern));
                if PATTERN.is_match(raw) {
                    Ok(Self(raw.to_owned()))
                } else {
                    Err(IdError { kind: $kind, reason: "unexpected shape" })
                }
            }

            pub fn as_str(&self) -> &str {
                &self.0
            }
        }

        impl fmt::Display for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str(&self.0)
            }
        }

        impl fmt::Debug for $name {
            fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                write!(f, "{}({})", stringify!($name), self.0)
            }
        }

        impl Serialize for $name {
            fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
                serializer.serialize_str(&self.0)
            }
        }

        impl<'de> Deserialize<'de> for $name {
            fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
                let raw = String::deserialize(deserializer)?;
                Self::parse(&raw).map_err(serde::de::Error::custom)
            }
        }

        impl JsonSchema for $name {
            fn schema_name() -> Cow<'static, str> {
                stringify!($name).into()
            }

            fn json_schema(_: &mut SchemaGenerator) -> Schema {
                json_schema!({ "type": "string", "format": $format, "pattern": $pattern })
            }
        }
    };
}

string_id!(
    /// A project id, the key in `projects.yaml`, e.g. `delta`.
    ProjectId, "project id", r"^[a-z][a-z0-9-]{0,39}$", "project-id"
);

string_id!(
    /// A fact id, e.g. `f_7Qx2`. Answers cite these.
    FactId, "fact id", r"^f_[A-Za-z0-9]{4,16}$", "fact-id"
);

string_id!(
    /// The short hash of the indexed snapshot's commit.
    CommitShort, "commit hash", r"^[0-9a-f]{7,12}$", "commit-short"
);

string_id!(
    /// A question, `q_` and a ULID.
    QuestionId, "question id", r"^q_[0-9A-HJKMNP-TV-Z]{26}$", "question-id"
);

string_id!(
    /// A user, `u_` and a ULID.
    UserId, "user id", r"^u_[0-9A-HJKMNP-TV-Z]{26}$", "user-id"
);

string_id!(
    /// A held answer waiting for sign-off, `ap_` and a ULID.
    ApprovalId, "approval id", r"^ap_[0-9A-HJKMNP-TV-Z]{26}$", "approval-id"
);

string_id!(
    /// The public part of a token, used to look it up and to name it in the audit. Not a secret.
    TokenPublicId, "token id", r"^[A-Za-z0-9]{12}$", "token-public-id"
);

string_id!(
    /// A judge check from the check catalogue, e.g. `asks_for_code`.
    CheckId, "check id", r"^[a-z][a-z0-9_]{0,63}$", "check-id"
);

string_id!(
    /// The type of an audit event: an event of the stream, or an admin or probe event.
    EventType, "event type", r"^[a-z][a-z0-9_]{0,63}$", "event-type"
);

string_id!(
    /// An item of a probe's judge request, chosen by the probe and repeated by the reply, e.g.
    /// `f_pull412`. Never shown to the judge.
    JudgeItemId, "judge item id", r"^[A-Za-z0-9_-]{1,40}$", "judge-item-id"
);

string_id!(
    /// A SHA-256 digest in lowercase hex.
    Sha256Hex, "SHA-256 digest", r"^[0-9a-f]{64}$", "sha256"
);

string_id!(
    /// A JSON Pointer to a value inside a tool's reply, e.g. `/items/3/title`.
    FieldPointer, "field pointer", r"^(/[a-z0-9_]{1,40}){1,8}$", "json-pointer"
);

string_id!(
    /// A person's e-mail address, as the admin entered it.
    Email, "e-mail address", r"^[^@\s]{1,64}@[^@\s]{1,253}\.[^@\s.]{1,63}$", "email"
);

string_id!(
    /// A model as a backend names it, e.g. `typesafe/jev-1.13`.
    ModelSlug, "model", r"^[A-Za-z0-9][A-Za-z0-9._:/-]{0,127}$", "model-slug"
);

/// A business name for a part of the code base, from the repository manifest (`Invoicing`,
/// `Export`), or `Other` for work that matches no module.
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ModuleName(String);

const MODULE_NAME_PATTERN: &str = r"^\p{L}[\p{L}\p{N} &+-]{0,39}$";

impl ModuleName {
    pub const OTHER: &'static str = "Other";

    pub fn parse(raw: &str) -> Result<Self, IdError> {
        static PATTERN: LazyLock<Regex> = LazyLock::new(|| re(MODULE_NAME_PATTERN));
        let raw = raw.trim();
        if !PATTERN.is_match(raw) || raw.contains("  ") {
            return Err(IdError {
                kind: "module name",
                reason: "use letters, digits, spaces, '&', '+' or '-'",
            });
        }
        Ok(Self(raw.to_owned()))
    }

    pub fn other() -> Self {
        Self(Self::OTHER.to_owned())
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for ModuleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl fmt::Debug for ModuleName {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "ModuleName({})", self.0)
    }
}

impl Serialize for ModuleName {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ModuleName {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        Self::parse(&raw).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for ModuleName {
    fn schema_name() -> Cow<'static, str> {
        "ModuleName".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({
            "type": "string",
            "format": "module-name",
            "pattern": MODULE_NAME_PATTERN,
            "description": "A module name from the probe's describe tool, or Other."
        })
    }
}

/// A pull request or issue number, written `#412`.
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct IssueNumber(u32);

const ISSUE_NUMBER_MAX_DIGITS: usize = 9;

impl IssueNumber {
    pub fn new(number: u32) -> Result<Self, IdError> {
        if number == 0 || number.to_string().len() > ISSUE_NUMBER_MAX_DIGITS {
            return Err(IdError {
                kind: "issue number",
                reason: "must be between 1 and 999999999",
            });
        }
        Ok(Self(number))
    }

    pub fn get(self) -> u32 {
        self.0
    }
}

impl fmt::Display for IssueNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "#{}", self.0)
    }
}

impl fmt::Debug for IssueNumber {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IssueNumber(#{})", self.0)
    }
}

impl Serialize for IssueNumber {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.collect_str(self)
    }
}

impl<'de> Deserialize<'de> for IssueNumber {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = String::deserialize(deserializer)?;
        let digits = raw
            .strip_prefix('#')
            .filter(|d| {
                !d.is_empty() && d.len() <= ISSUE_NUMBER_MAX_DIGITS && d.bytes().all(|b| b.is_ascii_digit())
            })
            .ok_or_else(|| serde::de::Error::custom("an issue number is # followed by digits"))?;
        let number = digits.parse().map_err(serde::de::Error::custom)?;
        Self::new(number).map_err(serde::de::Error::custom)
    }
}

impl JsonSchema for IssueNumber {
    fn schema_name() -> Cow<'static, str> {
        "IssueNumber".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "string", "format": "issue-number", "pattern": "^#[1-9][0-9]{0,8}$" })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_have_fixed_shapes() {
        assert!(ProjectId::parse("delta").is_ok());
        assert!(ProjectId::parse("Delta").is_err());
        assert!(ProjectId::parse("../etc").is_err());
        assert!(FactId::parse("f_7Qx2").is_ok());
        assert!(FactId::parse("f_../x").is_err());
        assert!(FactId::parse("f_1").is_err());
        assert!(CommitShort::parse("a1b2c3d").is_ok());
        assert!(CommitShort::parse("main").is_err());
    }

    #[test]
    fn api_ids_have_fixed_shapes() {
        assert!(QuestionId::parse("q_01JB2M3N4P5Q6R7S8T9V0W1X2Y").is_ok());
        assert!(QuestionId::parse("q_01jb2m3n4p5q6r7s8t9v0w1x2y").is_err());
        assert!(QuestionId::parse("u_01JB2M3N4P5Q6R7S8T9V0W1X2Y").is_err());
        // Crockford base32 has no I, L, O or U
        assert!(UserId::parse("u_01JB2M3N4P5Q6R7S8T9V0W1XIL").is_err());
        assert!(ApprovalId::parse("ap_01JB2M3N4P5Q6R7S8T9V0W1X2Y").is_ok());
        assert!(TokenPublicId::parse("k3Xq9Lm2Pz7A").is_ok());
        assert!(TokenPublicId::parse("k3Xq9Lm2Pz7_").is_err());
        assert!(TokenPublicId::parse("k3Xq9").is_err());
        assert!(CheckId::parse("asks_for_code").is_ok());
        assert!(CheckId::parse("Asks-For-Code").is_err());
        assert!(Sha256Hex::parse(&"0".repeat(64)).is_ok());
        assert!(Sha256Hex::parse(&"A".repeat(64)).is_err());
        assert!(FieldPointer::parse("/items/3/title").is_ok());
        assert!(FieldPointer::parse("items/3/title").is_err());
        assert!(FieldPointer::parse("/items/../title").is_err());
        assert!(Email::parse("pm@example.com").is_ok());
        assert!(Email::parse("pm@example").is_err());
        assert!(Email::parse("p m@example.com").is_err());
        assert!(ModelSlug::parse("typesafe/jev-1.13").is_ok());
        assert!(ModelSlug::parse("llama3.1:8b").is_ok());
        assert!(ModelSlug::parse("jev 1.13").is_err());
        assert!(JudgeItemId::parse("f_pull412").is_ok());
        assert!(JudgeItemId::parse("issue-7").is_ok());
        assert!(JudgeItemId::parse("").is_err());
        assert!(JudgeItemId::parse("pull 412").is_err());
        assert!(JudgeItemId::parse("../412").is_err());
        assert!(JudgeItemId::parse(&"a".repeat(41)).is_err());
    }

    #[test]
    fn module_names() {
        assert!(ModuleName::parse("Invoicing").is_ok());
        assert!(ModuleName::parse("Billing & Payments").is_ok());
        assert!(ModuleName::parse("Résumés").is_ok());
        assert_eq!(ModuleName::parse(" Export ").unwrap().as_str(), "Export");
        assert!(ModuleName::parse("src/Export").is_err());
        assert!(ModuleName::parse("export.rs").is_err());
        assert!(ModuleName::parse("Two  spaces").is_err());
        assert!(ModuleName::parse("").is_err());
        assert!(ModuleName::parse(&"a".repeat(41)).is_err());
        assert_eq!(ModuleName::other().as_str(), "Other");
    }

    #[test]
    fn issue_numbers_round_trip() {
        let n: IssueNumber = serde_json::from_str("\"#412\"").unwrap();
        assert_eq!(n.get(), 412);
        assert_eq!(serde_json::to_string(&n).unwrap(), "\"#412\"");
        for bad in ["\"412\"", "\"#0\"", "\"#12a\"", "\"#\"", "\"#1234567890\""] {
            assert!(serde_json::from_str::<IssueNumber>(bad).is_err(), "{bad}");
        }
        assert!(IssueNumber::new(0).is_err());
        assert!(IssueNumber::new(1_000_000_000).is_err());
    }
}
