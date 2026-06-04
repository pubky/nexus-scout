//! `scout` — a thin HTTP client for the nexus-scout gateway.
//!
//! It holds no Neo4j credentials: it builds a request, POSTs it to the gateway's
//! public HTTP API, and prints the JSON response verbatim to stdout (always valid
//! JSON, so `... | jq` works). The process exit code is derived from the response
//! via the shared [`nexus_scout_types`] contract, so the client and server cannot
//! disagree: `0` ok, `2` rejected/bad-query, `3` timeout, `1` internal/transient.

use std::process::ExitCode;

use clap::{Parser, Subcommand};
use nexus_scout_types::{exit_code, ErrorCode, ErrorResponse, QueryRequest};
use serde_json::{Map, Value};

/// Client for the nexus-scout read-only Cypher gateway.
#[derive(Debug, Parser)]
#[command(name = "scout", version, about)]
struct Cli {
    /// Base URL of the gateway (overrides `NEXUS_SCOUT_URL`).
    #[arg(long, global = true)]
    server_url: Option<String>,

    #[command(subcommand)]
    command: Command,
}

#[derive(Debug, Subcommand)]
enum Command {
    /// Run a read-only Cypher query.
    Query(QueryArgs),
    /// Print the graph schema.
    Schema,
}

/// Arguments to `query`.
#[derive(Debug, clap::Args)]
struct QueryArgs {
    /// The Cypher query to run.
    cypher: String,

    /// A string parameter as `key=value` (repeatable). For typed parameters use
    /// `--params-json`.
    #[arg(long = "param", value_name = "KEY=VALUE")]
    params: Vec<String>,

    /// All parameters as one typed JSON object (merged over `--param`).
    #[arg(long)]
    params_json: Option<String>,

    /// Override the row limit (capped server-side).
    #[arg(long)]
    limit: Option<u32>,
}

fn main() -> ExitCode {
    let cli = Cli::parse();
    let base = resolve_url(cli.server_url);

    let (body, code) = match cli.command {
        Command::Query(args) => match build_params(&args.params, args.params_json.as_deref()) {
            // A bad `--param` is a local rejection (exit 2); it never reaches the wire.
            Err(message) => (envelope(ErrorCode::QueryRejected, &message), 2),
            Ok(params) => run_query(&base, args.cypher, params, args.limit),
        },
        Command::Schema => run_get(&format!("{base}/v1/schema")),
    };

    println!("{body}");
    ExitCode::from(code)
}

/// Resolves the gateway base URL: `--server-url` > `NEXUS_SCOUT_URL` >
/// `http://localhost:8080`. A trailing slash is trimmed so paths join cleanly.
fn resolve_url(flag: Option<String>) -> String {
    let url = flag
        .or_else(|| std::env::var("NEXUS_SCOUT_URL").ok())
        .unwrap_or_else(|| "http://localhost:8080".to_owned());
    url.trim_end_matches('/').to_owned()
}

/// Merges `--param key=value` (string) and `--params-json` (typed, wins on
/// collision) into a parameter map. Returns a human message on malformed input.
fn build_params(params: &[String], params_json: Option<&str>) -> Result<Map<String, Value>, String> {
    let mut map = Map::new();
    for entry in params {
        let (key, value) = entry
            .split_once('=')
            .ok_or_else(|| format!("invalid --param {entry:?}, expected key=value"))?;
        map.insert(key.to_owned(), Value::String(value.to_owned()));
    }
    if let Some(raw) = params_json {
        let parsed: Value = serde_json::from_str(raw).map_err(|e| format!("invalid --params-json: {e}"))?;
        let Value::Object(object) = parsed else {
            return Err("--params-json must be a JSON object".to_owned());
        };
        for (key, value) in object {
            map.insert(key, value);
        }
    }
    Ok(map)
}

fn run_query(base: &str, cypher: String, params: Map<String, Value>, limit: Option<u32>) -> (String, u8) {
    let request = QueryRequest::new(cypher, params, limit);
    // Serialize ourselves (ureq's `json` feature is intentionally not enabled).
    let body = serde_json::to_string(&request).unwrap_or_default();
    let result = ureq::post(&format!("{base}/v1/query"))
        .set("content-type", "application/json")
        .send_string(&body);
    handle(result)
}

fn run_get(url: &str) -> (String, u8) {
    handle(ureq::get(url).call())
}

/// Turns a `ureq` outcome into `(stdout JSON, exit code)`. Always returns valid
/// JSON on stdout: a transport failure or a non-JSON body becomes a locally
/// synthesized envelope.
fn handle(result: Result<ureq::Response, ureq::Error>) -> (String, u8) {
    match result {
        Ok(response) => normalize(response.status(), &read_body(response)),
        Err(ureq::Error::Status(status, response)) => normalize(status, &read_body(response)),
        Err(ureq::Error::Transport(transport)) => (
            envelope(ErrorCode::InternalError, &format!("could not reach the gateway: {transport}")),
            1,
        ),
    }
}

fn read_body(response: ureq::Response) -> String {
    response.into_string().unwrap_or_default()
}

/// Re-emits the gateway's JSON and derives the exit code from it. A 2xx is exit
/// `0`; otherwise the `error` field is mapped via the shared contract. A body
/// that is not JSON (an infra-level bare response) becomes an internal envelope.
fn normalize(status: u16, body: &str) -> (String, u8) {
    let Ok(value) = serde_json::from_str::<Value>(body) else {
        return (
            envelope(ErrorCode::InternalError, &format!("gateway returned a non-JSON {status} response")),
            1,
        );
    };
    (pretty(&value), exit_for(status, &value))
}

/// The exit code for a parsed response: `0` on 2xx, else the `error` field mapped
/// through [`exit_code`] (unknown/absent → `1`).
fn exit_for(status: u16, value: &Value) -> u8 {
    if (200..300).contains(&status) {
        return 0;
    }
    value
        .get("error")
        .and_then(Value::as_str)
        .and_then(ErrorCode::from_wire)
        .map_or(1, exit_code)
}

fn pretty(value: &Value) -> String {
    serde_json::to_string_pretty(value).unwrap_or_else(|_| value.to_string())
}

/// A locally-synthesized error envelope, matching the gateway's wire shape.
fn envelope(code: ErrorCode, message: &str) -> String {
    let hint = match code {
        ErrorCode::QueryRejected => "Check the request: parameters must be key=value or valid JSON.",
        _ => "Retry; if it persists, check --server-url / NEXUS_SCOUT_URL and that the gateway is running.",
    };
    let response = ErrorResponse::new(code, message, hint);
    serde_json::to_string_pretty(&response).unwrap_or_else(|_| format!("{{\"error\":\"{code}\"}}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn resolve_url_precedence_and_trailing_slash() {
        assert_eq!(resolve_url(Some("http://x:9/".to_owned())), "http://x:9");
        // No flag and no env -> default (the env var is not set in this test).
        std::env::remove_var("NEXUS_SCOUT_URL");
        assert_eq!(resolve_url(None), "http://localhost:8080");
    }

    #[test]
    fn build_params_merges_and_rejects() {
        let m = build_params(&["id=pk:a".to_owned()], Some(r#"{"n":1}"#)).unwrap();
        assert_eq!(m["id"], json!("pk:a"));
        assert_eq!(m["n"], json!(1));
        assert!(build_params(&["noequals".to_owned()], None).is_err());
        assert!(build_params(&[], Some("[1,2]")).is_err());
    }

    #[test]
    fn exit_codes_follow_the_shared_contract() {
        assert_eq!(exit_for(200, &json!({"results": []})), 0);
        assert_eq!(exit_for(400, &json!({"error": "QUERY_REJECTED"})), 2);
        assert_eq!(exit_for(400, &json!({"error": "QUERY_SYNTAX_ERROR"})), 2);
        assert_eq!(exit_for(504, &json!({"error": "QUERY_TIMEOUT"})), 3);
        assert_eq!(exit_for(429, &json!({"error": "RATE_LIMITED"})), 1);
        assert_eq!(exit_for(500, &json!({"error": "INTERNAL_ERROR"})), 1);
        // Bare/non-envelope error body -> internal.
        assert_eq!(exit_for(503, &json!({})), 1);
    }

    #[test]
    fn synthesized_envelope_is_valid_json_with_the_code() {
        let s = envelope(ErrorCode::InternalError, "boom");
        let v: Value = serde_json::from_str(&s).unwrap();
        assert_eq!(v["error"], "INTERNAL_ERROR");
        assert_eq!(v["message"], "boom");
    }
}
