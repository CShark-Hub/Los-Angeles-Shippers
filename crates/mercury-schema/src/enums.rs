//! The shared vocabulary: question classes, tools, roles, judge modes and verdicts, channels and
//! access modes. Every name travels in `snake_case`.

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use utoipa::ToSchema;

/// What kind of question it is. One judge choice over these classifies every question.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum QuestionClass {
    Progress,
    Tested,
    Blocked,
    Health,
    Changes,
    /// About the project, and none of the five above.
    Open,
    /// Across the caller's projects; never listed in a manifest.
    Portfolio,
    Forbidden,
    Other,
}

impl QuestionClass {
    /// The classes a single project answers, the only ones a manifest may list.
    pub const PROJECT: [QuestionClass; 6] = [
        QuestionClass::Progress,
        QuestionClass::Tested,
        QuestionClass::Blocked,
        QuestionClass::Health,
        QuestionClass::Changes,
        QuestionClass::Open,
    ];
}

/// A probe tool. The set is fixed by the probe's access mode; a probe that offers a different one
/// is quarantined.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum ToolName {
    Describe,
    Progress,
    Tested,
    Blocked,
    Health,
    Changes,
    AskCode,
}

impl ToolName {
    /// The tools every `mercury.facts/0.1` probe serves, in a stable order.
    pub const ALL: [ToolName; 6] = [
        ToolName::Describe,
        ToolName::Progress,
        ToolName::Tested,
        ToolName::Blocked,
        ToolName::Health,
        ToolName::Changes,
    ];

    /// The tools of a probe in `code_model`: the six, then `ask_code`.
    pub const CODE_MODEL: [ToolName; 7] = [
        ToolName::Describe,
        ToolName::Progress,
        ToolName::Tested,
        ToolName::Blocked,
        ToolName::Health,
        ToolName::Changes,
        ToolName::AskCode,
    ];

    /// The tools a probe in `access` serves: `ask_code` only in `code_model`.
    pub fn for_access(access: AccessMode) -> &'static [ToolName] {
        match access {
            AccessMode::Metadata | AccessMode::Code => &Self::ALL,
            AccessMode::CodeModel => &Self::CODE_MODEL,
        }
    }

    /// The tool's name in MCP.
    pub fn as_str(self) -> &'static str {
        match self {
            ToolName::Describe => "describe",
            ToolName::Progress => "progress",
            ToolName::Tested => "tested",
            ToolName::Blocked => "blocked",
            ToolName::Health => "health",
            ToolName::Changes => "changes",
            ToolName::AskCode => "ask_code",
        }
    }
}

/// Who a token belongs to. Approving is a flag on a user, not a role.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Role {
    TechLead,
    Pm,
    Board,
    /// Runs the server; asks nothing.
    Admin,
}

/// Which judge decided, from the strongest to the most degraded.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum JudgeMode {
    Jev,
    Chat,
    Rules,
}

/// A check's outcome, ordered so the strictest of several is the `max`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Verdict {
    Pass,
    Review,
    Block,
}

/// Where a question came from: its `questions.channel`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum Channel {
    /// `POST /v1/questions` with a Bearer token: the CLI, a script.
    Cli,
    /// `POST /v1/questions` with the web client's session.
    Web,
    /// The tool `ask` at `/mcp`, with a Bearer token: an AI agent.
    Mcp,
}

impl Channel {
    /// Every channel, in a stable order.
    pub const ALL: [Channel; 3] = [Channel::Cli, Channel::Web, Channel::Mcp];

    /// The name the database and the contract give it, the same as on the wire.
    pub fn as_str(self) -> &'static str {
        match self {
            Channel::Cli => "cli",
            Channel::Web => "web",
            Channel::Mcp => "mcp",
        }
    }
}

/// How much of a repository the probe reads, ordered so the strictest of several is the `min`.
#[derive(
    Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize, JsonSchema, ToSchema,
)]
#[serde(rename_all = "snake_case")]
pub enum AccessMode {
    /// Pull requests, issues, CI runs and releases; no file contents.
    Metadata,
    /// The above, plus a snapshot of the default branch analysed by rules.
    Code,
    /// The above, plus a code reader that reads the snapshot for one question and answers with
    /// findings (`ask_code`).
    CodeModel,
}

/// What lowered the probe's access mode below the one asked for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(rename_all = "snake_case")]
pub enum AccessLimit {
    /// The git host token's permissions.
    Token,
    /// The manifest's `access`.
    Manifest,
    /// The admin's cap for the project.
    AccessMax,
    /// The snapshot is above the probe's size cap.
    SizeCap,
    /// `code_model` was asked for, and no code reader is configured.
    NoCodeReader,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_strictest_access_mode_is_the_min() {
        assert_eq!(AccessMode::Code.min(AccessMode::Metadata), AccessMode::Metadata);
        assert_eq!(AccessMode::CodeModel.min(AccessMode::Code), AccessMode::Code);
        let modes = [AccessMode::CodeModel, AccessMode::Metadata, AccessMode::Code];
        assert_eq!(modes.into_iter().min(), Some(AccessMode::Metadata));
    }

    #[test]
    fn the_strictest_verdict_is_the_max() {
        assert_eq!(Verdict::Pass.max(Verdict::Review), Verdict::Review);
        assert_eq!(
            [Verdict::Review, Verdict::Block, Verdict::Pass].into_iter().max(),
            Some(Verdict::Block)
        );
    }

    #[test]
    fn names_travel_in_snake_case() {
        let json = serde_json::to_value((
            QuestionClass::Portfolio,
            QuestionClass::Open,
            ToolName::Changes,
            ToolName::AskCode,
            Role::TechLead,
            JudgeMode::Rules,
            Verdict::Review,
            Channel::Mcp,
            AccessMode::CodeModel,
            AccessLimit::NoCodeReader,
        ))
        .unwrap();
        assert_eq!(
            json,
            serde_json::json!([
                "portfolio",
                "open",
                "changes",
                "ask_code",
                "tech_lead",
                "rules",
                "review",
                "mcp",
                "code_model",
                "no_code_reader"
            ])
        );
        assert!(serde_json::from_str::<Channel>("\"app\"").is_err());
    }

    #[test]
    fn tool_names_match_their_wire_names() {
        for tool in ToolName::CODE_MODEL {
            assert_eq!(serde_json::to_value(tool).unwrap(), tool.as_str());
        }
    }

    #[test]
    fn the_open_class_is_a_project_class() {
        assert!(QuestionClass::PROJECT.contains(&QuestionClass::Open));
        assert_eq!(QuestionClass::PROJECT.last(), Some(&QuestionClass::Open));
        for not_a_project_class in [
            QuestionClass::Portfolio,
            QuestionClass::Forbidden,
            QuestionClass::Other,
        ] {
            assert!(!QuestionClass::PROJECT.contains(&not_a_project_class));
        }
        assert_eq!(
            serde_json::from_str::<QuestionClass>("\"open\"").unwrap(),
            QuestionClass::Open
        );
    }

    #[test]
    fn ask_code_is_served_only_under_code_model() {
        assert_eq!(ToolName::for_access(AccessMode::Metadata), ToolName::ALL);
        assert_eq!(ToolName::for_access(AccessMode::Code), ToolName::ALL);
        assert_eq!(ToolName::for_access(AccessMode::CodeModel), ToolName::CODE_MODEL);
        assert!(!ToolName::ALL.contains(&ToolName::AskCode));
        let (six, last) = ToolName::CODE_MODEL.split_at(ToolName::ALL.len());
        assert_eq!(six, ToolName::ALL);
        assert_eq!(last, [ToolName::AskCode]);
    }

    #[test]
    fn channel_names_match_their_wire_names() {
        for channel in Channel::ALL {
            assert_eq!(serde_json::to_value(channel).unwrap(), channel.as_str());
        }
        assert_eq!(Channel::ALL.map(Channel::as_str), ["cli", "web", "mcp"]);
    }
}
