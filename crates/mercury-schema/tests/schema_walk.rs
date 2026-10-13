//! Walks the JSON Schema of every probe reply and every tool's arguments, and fails on any value
//! that could hold free text. A string is allowed only as an enum or a constant, a date or a
//! timestamp, `SafeText` or `DisplayName`, or an identifier with a fixed shape; a tool's
//! arguments may also carry the asker's question as a `RedactedQuestion`, which never passes in a
//! reply. Every object must refuse extra fields. The probe's judge requests and the core's replies
//! to them are walked the same way, with their own allowances.

use mercury_schema::probe_judge::{JudgeReply, JudgeRequest};
use mercury_schema::tools::tool_schemas;
use mercury_schema::{AccessMode, RedactedQuestion, ToolName};
use schemars::JsonSchema;
use serde_json::{Map, Value};

const DATE_FORMATS: &[&str] = &["date", "date-time"];

/// What a schema may hold besides enums, constants and dates.
struct Allowed {
    /// The formats of the bounded text types it may carry.
    text: &'static [&'static str],
    /// The formats of the identifiers it may carry; each needs its `pattern` too.
    ids: &'static [&'static str],
    /// Whether every object must refuse extra fields.
    closed: bool,
}

/// A probe reply or a tool's arguments: titles and names only as `SafeText` and `DisplayName`.
const PROBE: Allowed = Allowed {
    text: &["safe-text", "display-name"],
    ids: &[
        "fact-id",
        "project-id",
        "module-name",
        "issue-number",
        "commit-short",
    ],
    closed: true,
};

/// A tool's arguments: what a reply may hold, and the asker's question after redaction, which goes
/// into a tool and never comes out of one.
const ARGS: Allowed = Allowed {
    text: &["safe-text", "display-name", "redacted-question"],
    ids: PROBE.ids,
    closed: true,
};

fn is_type(map: &Map<String, Value>, name: &str) -> bool {
    match map.get("type") {
        Some(Value::String(t)) => t == name,
        Some(Value::Array(types)) => types.iter().any(|t| t == name),
        _ => false,
    }
}

fn format_in(map: &Map<String, Value>, formats: &[&str]) -> bool {
    map.get("format")
        .and_then(Value::as_str)
        .is_some_and(|f| formats.contains(&f))
}

fn walk(node: &Value, path: &str, allowed: &Allowed, violations: &mut Vec<String>) {
    let Some(map) = node.as_object() else {
        if node == &Value::Bool(true) {
            violations.push(format!("{path}: accepts any value"));
        }
        return;
    };
    if map.contains_key("$ref") {
        // Checked where it points, under `$defs`.
        return;
    }
    if map.keys().all(|k| k == "description" || k == "title") {
        violations.push(format!("{path}: accepts any value"));
    }
    if is_type(map, "string") {
        let allowed = map.contains_key("enum")
            || map.contains_key("const")
            || format_in(map, DATE_FORMATS)
            || format_in(map, allowed.text)
            || (format_in(map, allowed.ids) && map.contains_key("pattern"));
        if !allowed {
            violations.push(format!("{path}: free-text string"));
        }
    }
    if allowed.closed
        && is_type(map, "object")
        && map.get("additionalProperties") != Some(&Value::Bool(false))
    {
        violations.push(format!("{path}: accepts extra fields"));
    }
    for key in ["properties", "$defs"] {
        if let Some(Value::Object(children)) = map.get(key) {
            for (name, child) in children {
                walk(child, &format!("{path}/{key}/{name}"), allowed, violations);
            }
        }
    }
    for key in ["items", "additionalProperties", "not"] {
        if let Some(child) = map.get(key) {
            walk(child, &format!("{path}/{key}"), allowed, violations);
        }
    }
    for key in ["anyOf", "oneOf", "allOf", "prefixItems"] {
        if let Some(Value::Array(children)) = map.get(key) {
            for (i, child) in children.iter().enumerate() {
                walk(child, &format!("{path}/{key}/{i}"), allowed, violations);
            }
        }
    }
}

fn violations_of(schema: &Value, root: &str) -> Vec<String> {
    violations_under(schema, root, &PROBE)
}

fn violations_under(schema: &Value, root: &str, allowed: &Allowed) -> Vec<String> {
    let mut violations = Vec::new();
    walk(schema, root, allowed, &mut violations);
    violations
}

#[test]
fn probe_replies_have_no_free_text_field() {
    let violations: Vec<_> = ToolName::CODE_MODEL
        .iter()
        .flat_map(|&tool| violations_of(&tool_schemas(tool).1, tool.as_str()))
        .collect();
    assert!(
        violations.is_empty(),
        "free text in probe replies: {violations:#?}"
    );
}

#[test]
fn tool_arguments_have_no_free_text_field() {
    let violations: Vec<_> = ToolName::CODE_MODEL
        .iter()
        .flat_map(|&tool| violations_under(&tool_schemas(tool).0, tool.as_str(), &ARGS))
        .collect();
    assert!(
        violations.is_empty(),
        "free text in tool arguments: {violations:#?}"
    );
}

#[test]
fn a_judge_request_carries_text_only_as_bounded_judge_text() {
    let allowed = Allowed {
        text: &["judge-text"],
        ids: &["judge-item-id"],
        closed: true,
    };
    let schema = schemars::schema_for!(JudgeRequest).to_value();
    let violations = violations_under(&schema, "judge_request", &allowed);
    assert!(
        violations.is_empty(),
        "free text in a judge request: {violations:#?}"
    );
    let text = &schema["$defs"]["JudgeText"];
    assert_eq!(text["maxLength"], 500, "{text}");
}

#[test]
fn a_judge_reply_carries_no_text_at_all() {
    // a reply, like every response, may gain fields; none of them may be text
    let allowed = Allowed {
        text: &[],
        ids: &["judge-item-id"],
        closed: false,
    };
    let schema = schemars::schema_for!(JudgeReply).to_value();
    let violations = violations_under(&schema, "judge_reply", &allowed);
    assert!(violations.is_empty(), "text in a judge reply: {violations:#?}");
}

#[test]
fn the_walk_catches_free_text_and_open_objects() {
    #[derive(JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Leaky {
        details: String,
        extra: serde_json::Value,
    }
    #[derive(JsonSchema)]
    #[allow(dead_code)]
    struct Open {
        count: u32,
    }
    let leaky = violations_of(&schemars::schema_for!(Leaky).to_value(), "leaky");
    assert!(
        leaky
            .iter()
            .any(|v| v.contains("details") && v.contains("free-text")),
        "{leaky:#?}"
    );
    assert!(
        leaky
            .iter()
            .any(|v| v.contains("extra") && v.contains("any value")),
        "{leaky:#?}"
    );
    let open = violations_of(&schemars::schema_for!(Open).to_value(), "open");
    assert!(open.iter().any(|v| v.contains("extra fields")), "{open:#?}");
}

#[test]
fn a_redacted_question_never_passes_in_a_reply() {
    #[derive(JsonSchema)]
    #[serde(deny_unknown_fields)]
    #[allow(dead_code)]
    struct Echo {
        question: RedactedQuestion,
    }
    let schema = schemars::schema_for!(Echo).to_value();
    let in_a_reply = violations_of(&schema, "echo");
    assert!(
        in_a_reply
            .iter()
            .any(|v| v.contains("RedactedQuestion") && v.contains("free-text")),
        "{in_a_reply:#?}"
    );
    let in_arguments = violations_under(&schema, "echo", &ARGS);
    assert!(in_arguments.is_empty(), "{in_arguments:#?}");
}

/// A field's schema, from the root object or from one of its `$defs`.
fn field<'a>(schema: &'a Value, def: Option<&str>, name: &str) -> (&'a Value, bool) {
    let object = match def {
        Some(def) => &schema["$defs"][def],
        None => schema,
    };
    let required = object["required"]
        .as_array()
        .is_some_and(|r| r.iter().any(|f| f == name));
    (&object["properties"][name], required)
}

fn accepts_null(node: &Value) -> bool {
    node.as_object().is_some_and(|map| {
        is_type(map, "null")
            || ["anyOf", "oneOf"].iter().any(|k| {
                map.get(*k)
                    .and_then(Value::as_array)
                    .is_some_and(|alts| alts.iter().any(accepts_null))
            })
    })
}

#[test]
fn the_spec_nullable_fields_are_nullable_and_still_required() {
    let tested = tool_schemas(ToolName::Tested).1;
    let health = tool_schemas(ToolName::Health).1;
    let describe = tool_schemas(ToolName::Describe).1;
    let ask_code = tool_schemas(ToolName::AskCode).1;
    let cases = [
        (&tested, Some("ModuleStats"), "tests_total", "integer"),
        (&tested, Some("ModuleStats"), "tests_changed_14d", "integer"),
        (&health, None, "todo_fixme", "TodoFixme"),
        (&describe, Some("Snapshot"), "commit_short", "CommitShort"),
        (&ask_code, Some("Finding"), "module", "ModuleName"),
    ];
    for (schema, def, name, inner) in cases {
        let (node, required) = field(schema, def, name);
        assert!(accepts_null(node), "{name} is not nullable: {node}");
        assert!(required, "{name} is not required");
        assert!(
            node.to_string().contains(inner),
            "{name} is not a nullable {inner}: {node}"
        );
    }
}

/// `describe` names the tools of every access mode; which of them a probe offers is its mode's.
#[test]
fn describe_names_the_tools_of_every_access_mode() {
    let describe = tool_schemas(ToolName::Describe).1;
    let names = &describe["$defs"]["ToolName"]["enum"];
    let expected: Vec<Value> = ToolName::CODE_MODEL
        .iter()
        .map(|t| Value::from(t.as_str()))
        .collect();
    assert_eq!(names, &Value::Array(expected));
    for access in [AccessMode::Metadata, AccessMode::Code, AccessMode::CodeModel] {
        for tool in ToolName::for_access(access) {
            assert!(
                names
                    .as_array()
                    .is_some_and(|n| n.contains(&Value::from(tool.as_str()))),
                "{tool:?} of {access:?} is not in {names}"
            );
        }
    }
}
