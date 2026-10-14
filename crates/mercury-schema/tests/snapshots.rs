//! Snapshots of example JSON for every boundary type, and of the JSON Schemas each tool
//! advertises. A snapshot that changes is a contract that changes: review it as one.

#![allow(
    clippy::unwrap_used,
    reason = "the fixtures are literals; a panic is the test failing"
)]

use chrono::{DateTime, NaiveDate, Utc};
use insta::assert_json_snapshot;
use mercury_schema::api::Usd;
use mercury_schema::facts::{
    AskCodeReply, BlockedReply, BuildStatus, ChangeSummary, ChangesReply, CiRun, DescribeReply, Evidence,
    EvidenceState, EvidenceType, Fact, Finding, HealthReply, Kind, LastRelease, ModuleStats, OpenPrs,
    ProgressReply, Ratio, SchemaId, Score, Snapshot, Status, Tested, TestedReply, TodoFixme, WaitingOn,
};
use mercury_schema::redact::{display_name, safe_text, withheld_title};
use mercury_schema::tools::{ChangesArgs, Limit, ProgressArgs, tool_schemas};
use mercury_schema::{
    AccessLimit, AccessMode, CommitShort, FactId, IssueNumber, ModuleName, ProbeManifest, ProjectId,
    Redactor, ToolName,
};
use serde_json::json;

fn at(s: &str) -> DateTime<Utc> {
    s.parse().unwrap()
}

fn day(s: &str) -> NaiveDate {
    s.parse().unwrap()
}

fn module(name: &str) -> ModuleName {
    ModuleName::parse(name).unwrap()
}

fn ratio(value: f64) -> Ratio {
    Ratio::new(value).unwrap()
}

fn done_fact() -> Fact {
    Fact {
        id: FactId::parse("f_7Qx2").unwrap(),
        kind: Kind::Feature,
        title: safe_text("Invoice list with filters", &Redactor::default()).unwrap(),
        module: module("Invoicing"),
        status: Status::Done,
        tested: Tested::Passing,
        confidence: ratio(0.93),
        waiting_on: None,
        people: vec![display_name("Anna Smith", &Redactor::default()).unwrap()],
        evidence: vec![
            Evidence {
                kind: EvidenceType::Pr,
                reference: Some(IssueNumber::new(412).unwrap()),
                state: EvidenceState::Merged,
                at: day("2026-10-04"),
            },
            Evidence {
                kind: EvidenceType::Ci,
                reference: None,
                state: EvidenceState::Passed,
                at: day("2026-10-04"),
            },
        ],
        title_withheld: false,
        updated_at: at("2026-10-04T15:12:00Z"),
    }
}

fn blocked_fact() -> Fact {
    Fact {
        id: FactId::parse("f_9Kd1").unwrap(),
        kind: Kind::Fix,
        title: withheld_title(&module("Export")),
        module: module("Export"),
        status: Status::Blocked,
        tested: Tested::Unknown,
        confidence: ratio(0.9),
        waiting_on: Some(WaitingOn::External),
        people: vec![],
        evidence: vec![Evidence {
            kind: EvidenceType::Issue,
            reference: None,
            state: EvidenceState::Open,
            at: day("2026-10-02"),
        }],
        title_withheld: true,
        updated_at: at("2026-10-03T09:00:00Z"),
    }
}

#[test]
fn describe() {
    let reply = DescribeReply {
        schema: SchemaId,
        project: ProjectId::parse("delta").unwrap(),
        display_name: safe_text("Delta — invoicing", &Redactor::default()).unwrap(),
        access: AccessMode::Code,
        access_limited_by: None,
        modules: vec![module("Invoicing"), module("Export"), ModuleName::other()],
        tools: ToolName::ALL.to_vec(),
        snapshot: Snapshot {
            commit_short: Some(CommitShort::parse("a1b2c3d").unwrap()),
            indexed_at: at("2026-10-04T15:12:30Z"),
            meta_at: at("2026-10-04T15:10:00Z"),
        },
    };
    assert_json_snapshot!("describe", reply);
}

#[test]
fn describe_in_metadata_mode() {
    let reply = DescribeReply {
        schema: SchemaId,
        project: ProjectId::parse("omega").unwrap(),
        display_name: safe_text("Omega — mobile app", &Redactor::default()).unwrap(),
        access: AccessMode::Metadata,
        access_limited_by: Some(AccessLimit::Token),
        modules: vec![module("Login"), module("Payments")],
        tools: ToolName::ALL.to_vec(),
        snapshot: Snapshot {
            commit_short: None,
            indexed_at: at("2026-10-04T15:12:30Z"),
            meta_at: at("2026-10-04T15:10:00Z"),
        },
    };
    assert_json_snapshot!("describe_metadata", reply);
}

#[test]
fn describe_in_code_model_mode() {
    let reply = DescribeReply {
        schema: SchemaId,
        project: ProjectId::parse("delta").unwrap(),
        display_name: safe_text("Delta — invoicing", &Redactor::default()).unwrap(),
        access: AccessMode::CodeModel,
        access_limited_by: None,
        modules: vec![module("Invoicing"), module("Export"), ModuleName::other()],
        tools: ToolName::CODE_MODEL.to_vec(),
        snapshot: Snapshot {
            commit_short: Some(CommitShort::parse("a1b2c3d").unwrap()),
            indexed_at: at("2026-10-04T15:12:30Z"),
            meta_at: at("2026-10-04T15:10:00Z"),
        },
    };
    assert_json_snapshot!("describe_code_model", reply);
}

#[test]
fn progress() {
    let reply = ProgressReply {
        schema: SchemaId,
        as_of: at("2026-10-04T15:12:30Z"),
        items: vec![done_fact()],
    };
    assert_json_snapshot!("progress", reply);
}

#[test]
fn blocked() {
    let reply = BlockedReply {
        schema: SchemaId,
        as_of: at("2026-10-04T15:12:30Z"),
        items: vec![blocked_fact()],
    };
    assert_json_snapshot!("blocked", reply);
}

#[test]
fn tested() {
    let reply = TestedReply {
        schema: SchemaId,
        as_of: at("2026-10-04T15:12:30Z"),
        modules: vec![
            ModuleStats {
                module: module("Invoicing"),
                tests_total: Some(128),
                tests_changed_14d: Some(9),
                last_ci: CiRun {
                    status: BuildStatus::Passed,
                    at: Some(at("2026-10-04T14:58:00Z")),
                },
                pass_rate_7d: ratio(0.96),
                flaky_7d: ratio(0.02),
            },
            ModuleStats {
                module: module("Export"),
                tests_total: Some(0),
                tests_changed_14d: Some(0),
                last_ci: CiRun {
                    status: BuildStatus::None,
                    at: None,
                },
                pass_rate_7d: ratio(0.0),
                flaky_7d: ratio(0.0),
            },
        ],
    };
    assert_json_snapshot!("tested", reply);
}

#[test]
fn tested_in_metadata_mode() {
    let reply = TestedReply {
        schema: SchemaId,
        as_of: at("2026-10-04T15:12:30Z"),
        modules: vec![ModuleStats {
            module: module("Login"),
            tests_total: None,
            tests_changed_14d: None,
            last_ci: CiRun {
                status: BuildStatus::Failed,
                at: Some(at("2026-10-04T14:58:00Z")),
            },
            pass_rate_7d: ratio(0.75),
            flaky_7d: ratio(0.1),
        }],
    };
    assert_json_snapshot!("tested_metadata", reply);
}

#[test]
fn health() {
    let reply = HealthReply {
        schema: SchemaId,
        as_of: at("2026-10-04T15:12:30Z"),
        build: CiRun {
            status: BuildStatus::Passed,
            at: Some(at("2026-10-04T14:58:00Z")),
        },
        ci_pass_rate_7d: ratio(0.94),
        open_prs: OpenPrs {
            count: 4,
            oldest_days: 6,
        },
        todo_fixme: Some(TodoFixme {
            count: 37,
            trend_14d: -3,
        }),
        last_release: LastRelease {
            name: Some(safe_text("Release 1.4", &Redactor::default()).unwrap()),
            at: Some(at("2026-09-28T12:00:00Z")),
        },
        score: Score::new(86).unwrap(),
    };
    assert_json_snapshot!("health", reply);
}

#[test]
fn health_in_metadata_mode() {
    let reply = HealthReply {
        schema: SchemaId,
        as_of: at("2026-10-04T15:12:30Z"),
        build: CiRun {
            status: BuildStatus::None,
            at: None,
        },
        ci_pass_rate_7d: ratio(0.0),
        open_prs: OpenPrs {
            count: 0,
            oldest_days: 0,
        },
        todo_fixme: None,
        last_release: LastRelease { name: None, at: None },
        score: Score::new(40).unwrap(),
    };
    assert_json_snapshot!("health_metadata", reply);
}

#[test]
fn changes() {
    let reply = ChangesReply {
        schema: SchemaId,
        as_of: at("2026-10-04T15:12:30Z"),
        modules: vec![ChangeSummary {
            module: module("Invoicing"),
            commits: 14,
            merged_prs: 3,
            notable: vec![safe_text("Invoice list with filters", &Redactor::default()).unwrap()],
        }],
    };
    assert_json_snapshot!("changes", reply);
}

#[test]
fn ask_code() {
    let reply = AskCodeReply {
        schema: SchemaId,
        as_of: at("2026-10-04T15:12:30Z"),
        findings: vec![
            Finding {
                text: safe_text(
                    "The export writes one file per invoice batch",
                    &Redactor::default(),
                )
                .unwrap(),
                module: Some(module("Export")),
                confidence: 82,
            },
            Finding {
                text: safe_text(
                    "Totals are rounded once, after the discounts",
                    &Redactor::default(),
                )
                .unwrap(),
                module: None,
                confidence: 64,
            },
        ],
        withheld: 1,
        cost_usd: Usd::new(0.0031).unwrap(),
        prompt_version: 1,
    };
    assert_json_snapshot!("ask_code", reply);
}

#[test]
fn arguments() {
    let progress = ProgressArgs {
        since: Some(day("2026-09-28")),
        module: Some(module("Export")),
        limit: Limit::new(20),
    };
    let changes = ChangesArgs {
        since: day("2026-09-28"),
        module: None,
    };
    assert_json_snapshot!("arguments", json!({ "progress": progress, "changes": changes }));
}

#[test]
fn manifest() {
    let yaml = r#"
version: 0.1
project: delta
display_name: "Delta — invoicing"
access: code
modules:
  Invoicing: ["src/Invoices/**", "src/Billing/**"]
  Export:    ["src/Export/**"]
module_labels:
  Invoicing: [invoicing]
exclude: ["docs/customers/**", "**/*.pem"]
tests:
  frameworks: [xunit]
blocked_labels: [blocked, waiting]
allow: [progress, tested, blocked, health, changes]
deny: [people_evaluation]
redact:
  - { pattern: '(?i)\bmegabank\b', replace: 'the client' }
judge_context: [titles, labels]
"#;
    let manifest: ProbeManifest = serde_norway::from_str(yaml).unwrap();
    assert_json_snapshot!("manifest", manifest);
}

#[test]
fn tool_schemas_as_advertised() {
    for tool in ToolName::CODE_MODEL {
        let (input, output) = tool_schemas(tool);
        assert_json_snapshot!(
            format!("schema_{}", tool.as_str()),
            json!({ "input": input, "output": output })
        );
    }
}
