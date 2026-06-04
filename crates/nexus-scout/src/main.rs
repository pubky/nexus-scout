//! `nexus-scout` binary entry point.
//!
//! Parses the CLI, resolves configuration (defaults < environment < flags), and
//! dispatches. JSON is written to stdout for both success and error envelopes;
//! diagnostics go to stderr via `tracing`; the exit code encodes the outcome.

use std::process::ExitCode as ProcExitCode;

use clap::Parser;
use nexus_scout::cli::{build_params, Cli, Command, ExitCode, QueryArgs, Transport};
use nexus_scout::{Config, Error, Response, Scout};

#[tokio::main]
async fn main() -> ProcExitCode {
    tracing_subscriber::fmt()
        .with_env_filter(tracing_subscriber::EnvFilter::from_default_env())
        .with_writer(std::io::stderr)
        .init();

    let cli = Cli::parse();
    let code = run(cli).await;
    ProcExitCode::from(code.code())
}

async fn run(cli: Cli) -> ExitCode {
    // Schema needs neither a database nor configuration, so dispatch it before
    // resolving config: a malformed env var must not break schema discovery.
    if matches!(cli.command, Command::Schema) {
        print_json(&nexus_scout::schema());
        return ExitCode::Ok;
    }

    let config = match load_config(cli.neo4j_uri) {
        Ok(c) => c,
        Err(e) => return fail(&e),
    };

    match cli.command {
        Command::Query(args) => run_query(config, args).await,
        Command::Serve(args) => serve(config, args.transport).await,
        Command::Schema => unreachable!("handled before config resolution"),
    }
}

async fn run_query(config: Config, args: QueryArgs) -> ExitCode {
    let params = match build_params(&args.params, args.params_json.as_deref()) {
        Ok(p) => p,
        Err(e) => return fail(&e),
    };
    let scout = match Scout::connect(config).await {
        Ok(s) => s,
        Err(e) => return fail(&e),
    };
    match scout.query(&args.cypher, params, args.limit).await {
        Ok(response) => {
            print_json(&Response::Ok(response));
            ExitCode::Ok
        }
        Err(e) => fail(&e),
    }
}

#[cfg(feature = "mcp")]
async fn serve(config: Config, transport: Transport) -> ExitCode {
    match transport {
        Transport::Stdio => match nexus_scout::serve_stdio(config).await {
            Ok(()) => ExitCode::Ok,
            Err(e) => fail(&e),
        },
        Transport::Sse => {
            eprintln!("error: the SSE transport is not yet supported; use --transport stdio");
            ExitCode::Internal
        }
    }
}

#[cfg(not(feature = "mcp"))]
#[expect(
    clippy::unused_async,
    reason = "must match the mcp-enabled serve() signature at the call site"
)]
async fn serve(_config: Config, _transport: Transport) -> ExitCode {
    eprintln!("error: this build was compiled without the `mcp` feature; `serve` is unavailable");
    ExitCode::Internal
}

fn load_config(neo4j_uri: Option<String>) -> Result<Config, Error> {
    Ok(Config::builder().apply_env()?.maybe_neo4j_uri(neo4j_uri).build())
}

/// Prints an error envelope to stdout and returns its exit code.
fn fail(err: &Error) -> ExitCode {
    print_json(&Response::Err(err.to_response()));
    ExitCode::for_error(err)
}

fn print_json<T: serde::Serialize>(value: &T) {
    match serde_json::to_string_pretty(value) {
        Ok(s) => println!("{s}"),
        Err(e) => eprintln!("error: failed to serialize output: {e}"),
    }
}
