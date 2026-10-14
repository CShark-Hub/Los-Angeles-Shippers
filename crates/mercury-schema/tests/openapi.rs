//! The OpenAPI document: it describes every body of the core's API contract, and the copy at the
//! repository root (`openapi.json`, which `just types` reads) is the one the code produces.

#![allow(
    clippy::unwrap_used,
    reason = "the document is generated in the test; a panic is the test failing"
)]

use std::path::Path;

use mercury_schema::openapi::{document, document_json};
use serde_json::Value;

/// Every request and response body of the API contract, the stream's events, and the probe's
/// registration and judge requests.
const BODIES: &[&str] = &[
    "ServiceHealth",
    "SessionRequest",
    "Me",
    "ProjectList",
    "NewQuestion",
    "Accepted",
    "QuestionState",
    "Feedback",
    "ApprovalList",
    "ApprovalDecision",
    "AuditPage",
    "AuditItem",
    "AdminOverview",
    "AdminPeople",
    "AdminRepositories",
    "ErrorBody",
    "Event",
    "ProbeRegistration",
    "JudgeRequest",
    "JudgeReply",
];

fn schemas() -> serde_json::Map<String, Value> {
    let doc = serde_json::to_value(document()).unwrap();
    doc["components"]["schemas"].as_object().unwrap().clone()
}

#[test]
fn every_body_of_the_contract_has_a_schema() {
    let schemas = schemas();
    let missing: Vec<_> = BODIES.iter().filter(|b| !schemas.contains_key(**b)).collect();
    assert!(missing.is_empty(), "no schema for {missing:?}");
}

#[test]
fn every_reference_resolves() {
    let schemas = schemas();
    let text = serde_json::to_string(&schemas).unwrap();
    let dangling: Vec<_> = text
        .split("\"$ref\":\"#/components/schemas/")
        .skip(1)
        .filter_map(|rest| rest.split('"').next())
        .filter(|name| !schemas.contains_key(*name))
        .collect();
    assert!(dangling.is_empty(), "references to missing schemas: {dangling:?}");
}

#[test]
fn every_stream_event_is_in_the_document() {
    let event = &schemas()["Event"];
    let names: Vec<&str> = event["oneOf"]
        .as_array()
        .unwrap()
        .iter()
        .map(|variant| variant["properties"]["event"]["enum"][0].as_str().unwrap())
        .collect();
    assert_eq!(names, mercury_schema::Event::NAMES);
}

#[test]
fn the_committed_document_is_current() {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../openapi.json");
    let committed = std::fs::read_to_string(&path).unwrap_or_default();
    // Compared as JSON, so a CRLF checkout doesn't count as a change.
    let committed: Value = serde_json::from_str(&committed).unwrap_or(Value::Null);
    let current: Value = serde_json::from_str(&document_json()).unwrap();
    assert!(
        committed == current,
        "openapi.json is not what the types describe: run `just openapi`, then `just types`"
    );
}
