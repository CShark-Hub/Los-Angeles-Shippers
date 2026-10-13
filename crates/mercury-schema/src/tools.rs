//! The arguments of each probe tool, and the JSON Schemas a probe advertises for its tools.
//! Unknown arguments are rejected, not guessed.

use std::borrow::Cow;

use chrono::NaiveDate;
use schemars::generate::SchemaSettings;
use schemars::{JsonSchema, Schema, SchemaGenerator, json_schema};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use utoipa::ToSchema;

use crate::enums::ToolName;
use crate::facts::{
    AskCodeReply, BlockedReply, ChangesReply, DescribeReply, HealthReply, ProgressReply, TestedReply,
};
use crate::ids::ModuleName;
use crate::redact::RedactedQuestion;

/// How many items a list tool returns, 1 to 50.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct Limit(u8);

impl Limit {
    pub const MAX: u8 = 50;

    pub fn new(value: u8) -> Option<Self> {
        (1..=Self::MAX).contains(&value).then_some(Self(value))
    }

    pub fn get(self) -> u8 {
        self.0
    }
}

impl Serialize for Limit {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        serializer.serialize_u8(self.0)
    }
}

impl<'de> Deserialize<'de> for Limit {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let raw = u64::deserialize(deserializer)?;
        u8::try_from(raw)
            .ok()
            .and_then(Limit::new)
            .ok_or_else(|| serde::de::Error::custom("the limit is between 1 and 50"))
    }
}

impl JsonSchema for Limit {
    fn schema_name() -> Cow<'static, str> {
        "Limit".into()
    }

    fn json_schema(_: &mut SchemaGenerator) -> Schema {
        json_schema!({ "type": "integer", "minimum": 1, "maximum": Limit::MAX })
    }
}

/// A module in the arguments that isn't one of the probe's modules.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
#[error("unknown module")]
pub struct UnknownModule;

fn check_module(module: Option<&ModuleName>, modules: &[ModuleName]) -> Result<(), UnknownModule> {
    match module {
        Some(m) if !modules.contains(m) => Err(UnknownModule),
        _ => Ok(()),
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct DescribeArgs {}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ProgressArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<NaiveDate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<ModuleName>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<Limit>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct TestedArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<ModuleName>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct BlockedArgs {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub since: Option<NaiveDate>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct HealthArgs {}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct ChangesArgs {
    pub since: NaiveDate,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub module: Option<ModuleName>,
}

/// The arguments of `ask_code`: the asker's question after the project's redaction rules, and
/// nothing else.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, JsonSchema, ToSchema)]
#[serde(deny_unknown_fields)]
pub struct AskCodeArgs {
    pub question: RedactedQuestion,
}

impl ProgressArgs {
    /// Checks the module against the probe's list: a well-formed but unknown name is an error.
    pub fn check_module(&self, modules: &[ModuleName]) -> Result<(), UnknownModule> {
        check_module(self.module.as_ref(), modules)
    }
}

impl TestedArgs {
    /// Checks the module against the probe's list: a well-formed but unknown name is an error.
    pub fn check_module(&self, modules: &[ModuleName]) -> Result<(), UnknownModule> {
        check_module(self.module.as_ref(), modules)
    }
}

impl ChangesArgs {
    /// Checks the module against the probe's list: a well-formed but unknown name is an error.
    pub fn check_module(&self, modules: &[ModuleName]) -> Result<(), UnknownModule> {
        check_module(self.module.as_ref(), modules)
    }
}

/// The input and output JSON Schemas of a tool, for MCP `tools/list`. The input is what the
/// probe accepts; the output is what it sends, so every nullable field is listed as required.
pub fn tool_schemas(tool: ToolName) -> (serde_json::Value, serde_json::Value) {
    fn input<T: JsonSchema>() -> serde_json::Value {
        SchemaSettings::draft2020_12()
            .into_generator()
            .into_root_schema_for::<T>()
            .to_value()
    }
    fn output<T: JsonSchema>() -> serde_json::Value {
        SchemaSettings::draft2020_12()
            .for_serialize()
            .into_generator()
            .into_root_schema_for::<T>()
            .to_value()
    }
    match tool {
        ToolName::Describe => (input::<DescribeArgs>(), output::<DescribeReply>()),
        ToolName::Progress => (input::<ProgressArgs>(), output::<ProgressReply>()),
        ToolName::Tested => (input::<TestedArgs>(), output::<TestedReply>()),
        ToolName::Blocked => (input::<BlockedArgs>(), output::<BlockedReply>()),
        ToolName::Health => (input::<HealthArgs>(), output::<HealthReply>()),
        ToolName::Changes => (input::<ChangesArgs>(), output::<ChangesReply>()),
        ToolName::AskCode => (input::<AskCodeArgs>(), output::<AskCodeReply>()),
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn arguments_reject_unknown_fields_and_bad_values() {
        assert!(serde_json::from_value::<ProgressArgs>(json!({ "limit": 10 })).is_ok());
        assert!(serde_json::from_value::<ProgressArgs>(json!({ "limit": 0 })).is_err());
        assert!(serde_json::from_value::<ProgressArgs>(json!({ "limit": 51 })).is_err());
        assert!(serde_json::from_value::<ProgressArgs>(json!({ "limit": 300 })).is_err());
        assert!(serde_json::from_value::<ProgressArgs>(json!({ "path": "src" })).is_err());
        assert!(serde_json::from_value::<ProgressArgs>(json!({ "module": "src/Export" })).is_err());
        assert!(serde_json::from_value::<ProgressArgs>(json!({ "since": "last week" })).is_err());
        assert!(serde_json::from_value::<ChangesArgs>(json!({})).is_err());
        assert!(serde_json::from_value::<HealthArgs>(json!({ "verbose": true })).is_err());
    }

    #[test]
    fn a_module_must_be_one_of_the_probes() {
        let modules = [ModuleName::parse("Invoicing").unwrap(), ModuleName::other()];
        let args: TestedArgs = serde_json::from_value(json!({ "module": "Invoicing" })).unwrap();
        assert_eq!(args.check_module(&modules), Ok(()));
        let args: TestedArgs = serde_json::from_value(json!({ "module": "Payroll" })).unwrap();
        assert_eq!(args.check_module(&modules), Err(UnknownModule));
        assert_eq!(ProgressArgs::default().check_module(&modules), Ok(()));
    }

    #[test]
    fn ask_code_takes_only_its_question() {
        let args: AskCodeArgs =
            serde_json::from_value(json!({ "question": "How does the export pick the file format?" }))
                .unwrap();
        assert_eq!(
            args.question.as_str(),
            "How does the export pick the file format?"
        );
        assert!(serde_json::from_value::<AskCodeArgs>(json!({ "question": "Why?", "path": "src" })).is_err());
        assert!(serde_json::from_value::<AskCodeArgs>(json!({})).is_err());
        let long = "a".repeat(1001);
        assert!(serde_json::from_value::<AskCodeArgs>(json!({ "question": long })).is_err());
    }

    #[test]
    fn empty_optional_arguments_serialize_as_an_empty_object() {
        assert_eq!(serde_json::to_value(ProgressArgs::default()).unwrap(), json!({}));
    }
}
