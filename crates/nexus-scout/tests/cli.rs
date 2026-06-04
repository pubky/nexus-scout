//! Server-binary smoke tests (no database). The binary is serve-only; the
//! `scout` client and the HTTP/MCP transports are tested in their own suites.

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
    // Config is resolved before the server starts; a bad numeric env var fails
    // closed and the message names the offending variable. (A misconfigured
    // public server is worse than a misconfigured client, so this stays tested.)
    nexus_scout()
        .env("MAX_RESULT_ROWS", "not-a-number")
        .args(["serve", "--transport", "sse"])
        .assert()
        .failure()
        .stderr(predicates::str::contains("MAX_RESULT_ROWS"));
}
