//! Server-binary smoke tests (no database).

use assert_cmd::Command;

fn nexus_scout() -> Command {
    Command::cargo_bin("nexus-scout").expect("binary builds")
}

#[test]
fn sse_transport_is_not_yet_supported() {
    nexus_scout()
        .args(["serve", "--transport", "sse"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("not yet supported"));
}

#[test]
fn malformed_config_env_names_the_variable() {
    // A bad numeric env var fails closed before the server starts, naming the offending variable.
    nexus_scout()
        .env("MAX_RESULT_ROWS", "not-a-number")
        .args(["serve", "--transport", "sse"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("MAX_RESULT_ROWS"));
}
