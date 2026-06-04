//! End-to-end `scout` client tests against a tiny mock HTTP server (no gateway,
//! no Neo4j). They lock the stdout-envelope + exit-code contract the client
//! derives from an HTTP response.

use std::io::{Read, Write};
use std::net::TcpListener;

use assert_cmd::Command;

/// Spawns a mock server that answers every request with a fixed status + body,
/// returning its base URL. The daemon thread dies with the test process.
fn mock_server(status: u16, body: &'static str) -> String {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind mock");
    let addr = listener.local_addr().expect("addr");
    std::thread::spawn(move || {
        for stream in listener.incoming() {
            let Ok(mut stream) = stream else { continue };
            let mut buf = [0u8; 8192];
            let _ = stream.read(&mut buf); // consume the request; small + on localhost
            let reason = if (200..300).contains(&status) { "OK" } else { "ERR" };
            let response = format!(
                "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
            let _ = stream.write_all(response.as_bytes());
        }
    });
    format!("http://{addr}")
}

fn scout(url: &str) -> Command {
    let mut cmd = Command::cargo_bin("scout").expect("binary builds");
    cmd.arg("--server-url").arg(url);
    cmd
}

#[test]
fn happy_query_prints_results_and_exits_zero() {
    let url = mock_server(200, r#"{"results":[{"name":"Alice"}],"count":1,"truncated":false}"#);
    scout(&url)
        .args(["query", "MATCH (u:User) RETURN u.name AS name"])
        .assert()
        .success()
        .stdout(predicates::str::contains("Alice"));
}

#[test]
fn rejected_query_prints_envelope_and_exits_two() {
    let url = mock_server(400, r#"{"error":"QUERY_REJECTED","message":"no writes","hint":"read only"}"#);
    scout(&url)
        .args(["query", "MATCH (u) DETACH DELETE u"])
        .assert()
        .code(2)
        .stdout(predicates::str::contains("QUERY_REJECTED"));
}

#[test]
fn timeout_maps_to_exit_three() {
    let url = mock_server(504, r#"{"error":"QUERY_TIMEOUT","message":"too slow","hint":"add LIMIT"}"#);
    scout(&url).args(["query", "MATCH (n) RETURN n"]).assert().code(3);
}

#[test]
fn rate_limited_maps_to_exit_one() {
    let url = mock_server(429, r#"{"error":"RATE_LIMITED","message":"slow down","hint":"retry"}"#);
    scout(&url).args(["query", "MATCH (n) RETURN n"]).assert().code(1);
}

#[test]
fn schema_is_fetched_from_the_service() {
    let url = mock_server(200, r#"{"nodes":[],"relationships":[],"examples":[]}"#);
    scout(&url).arg("schema").assert().success().stdout(predicates::str::contains("nodes"));
}

#[test]
fn unreachable_gateway_yields_internal_envelope_on_stdout() {
    // Bind then drop to obtain a port nothing is listening on.
    let addr = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    scout(&format!("http://{addr}"))
        .args(["query", "RETURN 1"])
        .assert()
        .code(1)
        .stdout(predicates::str::contains("INTERNAL_ERROR"));
}

#[test]
fn bad_param_is_a_local_rejection_and_never_hits_the_network() {
    // Points at a closed port: if the bad --param were sent, it would be a
    // transport error (exit 1); instead it is a local rejection (exit 2).
    let addr = TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap();
    scout(&format!("http://{addr}"))
        .args(["query", "MATCH (n) RETURN n", "--param", "noequals"])
        .assert()
        .code(2)
        .stdout(predicates::str::contains("QUERY_REJECTED"));
}
