//! Command-line interface definition and the pure helpers it relies on.
//!
//! The CLI is a first-class interface: shell agents call `nexus-scout query`
//! directly. JSON always goes to stdout (so `... | jq` works for both success
//! and error), human logs go to stderr, and the process exit code encodes the
//! outcome (see [`ExitCode`]).

use clap::{Parser, Subcommand};
use serde_json::{Map, Value};

use crate::Error;

/// Read-only Cypher query gateway for the Pubky social graph.
#[derive(Debug, Parser)]
#[command(name = "nexus-scout", version, about)]
pub struct Cli {
    /// Neo4j Bolt URI (overrides `NEO4J_URI`).
    #[arg(long, global = true)]
    pub neo4j_uri: Option<String>,

    /// The subcommand to run.
    #[command(subcommand)]
    pub command: Command,
}

/// The available subcommands.
#[derive(Debug, Subcommand)]
pub enum Command {
    /// Execute a read-only Cypher query and print the JSON result.
    Query(QueryArgs),
    /// Print the graph schema as JSON.
    Schema,
    /// Run as a Model Context Protocol server.
    Serve(ServeArgs),
}

/// Arguments to `query`.
#[derive(Debug, clap::Args)]
pub struct QueryArgs {
    /// The Cypher query to run.
    pub cypher: String,

    /// A string parameter as `key=value` (repeatable). For typed parameters
    /// (numbers, arrays, objects) use `--params-json`.
    #[arg(long = "param", value_name = "KEY=VALUE")]
    pub params: Vec<String>,

    /// All parameters as a single JSON object (typed). Merged over `--param`.
    #[arg(long)]
    pub params_json: Option<String>,

    /// Override the result row limit (capped at the configured maximum).
    #[arg(long)]
    pub limit: Option<u32>,
}

/// Arguments to `serve`.
#[derive(Debug, clap::Args)]
pub struct ServeArgs {
    /// Transport to serve on.
    #[arg(long, value_enum, default_value_t = Transport::Stdio)]
    pub transport: Transport,
}

/// Supported MCP transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, clap::ValueEnum)]
pub enum Transport {
    /// Standard input/output (for local agents).
    Stdio,
    /// Server-sent events (deferred; not yet supported).
    Sse,
}

/// Process exit codes, distinct per outcome so scripts can branch.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum ExitCode {
    /// Success.
    Ok = 0,
    /// An unexpected internal error.
    Internal = 1,
    /// The query (or its parameters) was rejected by the sanitizer.
    Rejected = 2,
    /// The query timed out.
    Timeout = 3,
}

impl ExitCode {
    /// Returns the process exit status byte.
    #[must_use]
    pub fn code(self) -> u8 {
        self as u8
    }

    /// Maps a gateway error to its exit code.
    #[must_use]
    pub fn for_error(err: &Error) -> Self {
        if err.is_rejected() {
            Self::Rejected
        } else if err.is_timeout() {
            Self::Timeout
        } else {
            Self::Internal
        }
    }
}

/// Builds the parameter map from the `--param` and `--params-json` flags.
///
/// `--param key=value` entries are string-valued; `--params-json` supplies a
/// typed object that is merged on top (so it wins on key collisions).
///
/// # Errors
///
/// Returns [`Error`] if a `--param` entry is not `key=value`, or `--params-json`
/// is not a JSON object.
pub fn build_params(params: &[String], params_json: Option<&str>) -> Result<Map<String, Value>, Error> {
    let mut map = Map::new();
    for entry in params {
        let (key, value) = entry
            .split_once('=')
            .ok_or_else(|| Error::bad_request(format!("invalid --param {entry:?}, expected key=value")))?;
        map.insert(key.to_owned(), Value::String(value.to_owned()));
    }
    if let Some(raw) = params_json {
        let parsed: Value =
            serde_json::from_str(raw).map_err(|e| Error::bad_request(format!("invalid --params-json: {e}")))?;
        let Value::Object(obj) = parsed else {
            return Err(Error::bad_request("--params-json must be a JSON object"));
        };
        for (k, v) in obj {
            map.insert(k, v);
        }
    }
    Ok(map)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn string_params_parse() {
        let m = build_params(&["id=pk:abc".to_owned(), "name=Alice".to_owned()], None).unwrap();
        assert_eq!(m["id"], json!("pk:abc"));
        assert_eq!(m["name"], json!("Alice"));
    }

    #[test]
    fn invalid_param_is_rejected() {
        assert!(build_params(&["noequals".to_owned()], None).is_err());
    }

    #[test]
    fn json_params_are_typed_and_win_collisions() {
        let m = build_params(
            &["since=string".to_owned()],
            Some(r#"{"since": 1709251200000, "tags": ["a", "b"]}"#),
        )
        .unwrap();
        assert_eq!(m["since"], json!(1_709_251_200_000_i64));
        assert_eq!(m["tags"], json!(["a", "b"]));
    }

    #[test]
    fn non_object_params_json_is_rejected() {
        assert!(build_params(&[], Some("[1,2,3]")).is_err());
    }

    #[test]
    fn exit_codes_map_from_errors() {
        assert_eq!(ExitCode::Ok.code(), 0);
        assert_eq!(ExitCode::for_error(&Error::timeout(10)), ExitCode::Timeout);
    }
}
