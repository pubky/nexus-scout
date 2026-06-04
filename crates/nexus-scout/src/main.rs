//! `nexus-scout` server binary: runs the gateway over HTTP (public) or stdio MCP.
//!
//! Read-only Cypher requests are sent by *clients* (the `scout` CLI, an HTTP
//! agent, or an MCP-native agent), not by this binary. Configuration is resolved
//! from the environment (with the Neo4j URI optionally overridden by a flag);
//! operational diagnostics go to stderr via `tracing`, and a non-zero exit code
//! signals a startup failure.

use std::process::ExitCode;

use clap::{Parser, Subcommand, ValueEnum};
use nexus_scout::{Config, Error};

/// Read-only Cypher query gateway server for the Pubky social graph.
#[derive(Debug, Parser)]
#[command(name = "nexus-scout", version, about)]
struct Cli {
    /// Neo4j Bolt URI (overrides `NEO4J_URI`).
    #[arg(long, global = true)]
    neo4j_uri: Option<String>,

    #[command(subcommand)]
    command: Command,
}

/// The available subcommands.
#[derive(Debug, Subcommand)]
enum Command {
    /// Run the gateway server.
    Serve(ServeArgs),
}

/// Arguments to `serve`.
#[derive(Debug, clap::Args)]
struct ServeArgs {
    /// Transport to serve on.
    #[arg(long, value_enum, default_value_t = Transport::Http)]
    transport: Transport,
}

/// Supported server transports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, ValueEnum)]
enum Transport {
    /// Public HTTP API (Axum).
    Http,
    /// Model Context Protocol over stdio.
    Stdio,
    /// Server-sent events (not yet supported).
    Sse,
}

#[tokio::main]
async fn main() -> ExitCode {
    // Default to the gateway's own info/warn output so operational logs are
    // visible out of the box; `RUST_LOG` still overrides when set.
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("nexus_scout=info,warn"));
    tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .init();

    match run(Cli::parse()).await {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("error: {e}");
            ExitCode::FAILURE
        }
    }
}

async fn run(cli: Cli) -> Result<(), Error> {
    match cli.command {
        Command::Serve(args) => {
            // Config is resolved before the server starts, so a misconfiguration
            // fails closed with a message naming the offending variable.
            let config = Config::builder().apply_env()?.maybe_neo4j_uri(cli.neo4j_uri).build();
            serve(config, args.transport).await
        }
    }
}

async fn serve(config: Config, transport: Transport) -> Result<(), Error> {
    match transport {
        Transport::Http => serve_http_dispatch(config).await,
        Transport::Stdio => serve_stdio_dispatch(config).await,
        Transport::Sse => Err(Error::bad_request(
            "the SSE transport is not yet supported; use --transport http or stdio",
        )),
    }
}

#[cfg(feature = "http")]
async fn serve_http_dispatch(config: Config) -> Result<(), Error> {
    nexus_scout::serve_http(config).await
}

#[cfg(not(feature = "http"))]
#[expect(clippy::unused_async, reason = "must match the http-enabled signature")]
async fn serve_http_dispatch(_config: Config) -> Result<(), Error> {
    Err(Error::bad_request("this build was compiled without the `http` feature"))
}

#[cfg(feature = "mcp")]
async fn serve_stdio_dispatch(config: Config) -> Result<(), Error> {
    nexus_scout::serve_stdio(config).await
}

#[cfg(not(feature = "mcp"))]
#[expect(clippy::unused_async, reason = "must match the mcp-enabled signature")]
async fn serve_stdio_dispatch(_config: Config) -> Result<(), Error> {
    Err(Error::bad_request("this build was compiled without the `mcp` feature"))
}
