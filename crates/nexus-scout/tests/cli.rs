//! End-to-end CLI smoke tests that need no database. They assert the stdout
//! JSON contract and the exit-code contract for the DB-free paths.

use assert_cmd::Command;
use serde_json::Value;

fn nexus_scout() -> Command {
    Command::cargo_bin("nexus-scout").expect("binary builds")
}

#[test]
fn schema_prints_json_without_a_database() {
    let out = nexus_scout().arg("schema").assert().success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: Value = serde_json::from_str(&stdout).expect("schema is valid JSON");
    assert!(v["nodes"].is_array());
    assert!(v["relationships"].is_array());
    assert!(!v["examples"].as_array().unwrap().is_empty());
}

#[test]
fn schema_ignores_malformed_config_env() {
    // `schema` needs neither a database nor valid configuration, so a malformed
    // numeric env var must not break schema discovery.
    let out = nexus_scout()
        .env("MAX_RESULT_ROWS", "not-a-number")
        .arg("schema")
        .assert()
        .success();
    let stdout = String::from_utf8(out.get_output().stdout.clone()).unwrap();
    let v: Value = serde_json::from_str(&stdout).expect("schema is valid JSON");
    assert!(v["nodes"].is_array());
}

#[test]
fn rejected_query_emits_envelope_on_stdout_and_exits_2() {
    let assert = nexus_scout()
        .args(["query", "MATCH (u) DETACH DELETE u"])
        .assert()
        .code(2);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: Value = serde_json::from_str(&stdout).expect("error envelope is valid JSON");
    assert_eq!(v["error"], "QUERY_REJECTED");
    assert!(v["hint"].as_str().unwrap().contains("read-only"));
}

#[test]
fn bad_param_is_a_rejection_not_an_internal_error() {
    let assert = nexus_scout()
        .args(["query", "MATCH (n) RETURN n", "--param", "noequals"])
        .assert()
        .code(2);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: Value = serde_json::from_str(&stdout).unwrap();
    assert_eq!(v["error"], "QUERY_REJECTED");
}

#[test]
fn malformed_config_env_names_the_variable_and_is_not_retryable() {
    // A bad numeric env var on the `query` path must fail at config resolution
    // with a message that names the offending variable (not a generic, retryable
    // INTERNAL_ERROR with no actionable detail). Fails before any DB connection.
    let assert = nexus_scout()
        .env("MAX_RESULT_ROWS", "not-a-number")
        .args(["query", "RETURN 1"])
        .assert()
        .code(1);
    let stdout = String::from_utf8(assert.get_output().stdout.clone()).unwrap();
    let v: Value = serde_json::from_str(&stdout).expect("error envelope is valid JSON");
    assert_eq!(v["error"], "INTERNAL_ERROR");
    assert!(
        v["message"].as_str().unwrap().contains("MAX_RESULT_ROWS"),
        "message should name the offending variable: {}",
        v["message"]
    );
}

#[test]
fn sse_transport_is_not_yet_supported() {
    nexus_scout()
        .args(["serve", "--transport", "sse"])
        .assert()
        .code(1)
        .stderr(predicates::str::contains("not yet supported"));
}
