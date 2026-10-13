//! Every file in `config-examples/` parses into its type, and its parsed form is a snapshot: a
//! change to an example or to a config type shows up here for review.

#![allow(
    clippy::unwrap_used,
    clippy::expect_used,
    reason = "the examples are files of this repository; a panic is the test failing"
)]

use std::path::Path;

use insta::assert_json_snapshot;
use mercury_schema::config::{ModelsConfig, Policies, ProjectsConfig};
use serde::de::DeserializeOwned;

fn example<T: DeserializeOwned>(name: &str) -> T {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../config-examples")
        .join(name);
    let text = std::fs::read_to_string(&path).expect("the example exists");
    serde_norway::from_str(&text).unwrap_or_else(|err| panic!("config-examples/{name}: {err}"))
}

#[test]
fn projects_example() {
    let projects: ProjectsConfig = example("projects.yaml");
    assert_json_snapshot!("config_projects", projects);
}

#[test]
fn policies_example() {
    let policies: Policies = example("policies.yaml");
    // through serde_json: the role keys are enum variants, which insta writes only as strings this way
    assert_json_snapshot!("config_policies", serde_json::to_value(policies).unwrap());
}

#[test]
fn models_example() {
    let models: ModelsConfig = example("models.yaml");
    assert_json_snapshot!("config_models", models);
}
