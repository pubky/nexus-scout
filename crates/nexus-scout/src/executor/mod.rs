//! Read-only Neo4j query execution.
//!
//! The executor owns a pooled `neo4rs::Graph` and runs each query inside an
//! explicit read transaction. Two timeout layers cooperate:
//!
//! 1. A client-side `tokio::time::timeout` bounds *liveness* (the caller never
//!    waits longer than the budget). On expiry the transaction is rolled back so
//!    a `RESET` is sent rather than silently dropping the stream.
//! 2. The real bound on *server* resources is the Neo4j `db.transaction.timeout`
//!    (and a transaction memory limit) configured for the reader role; see the
//!    reader-setup script. The driver provides no per-query server timeout.
//!
//! An explicit transaction (not `Graph::execute`) is used deliberately: only
//! `Graph::execute`/`run` are wrapped in the driver's transient-error retry loop
//! (backoff up to ~60s), which would defeat the liveness budget. `Txn::execute`
//! is single-shot.

mod shape;

use std::sync::Arc;
use std::time::Duration;

use neo4rs::{ConfigBuilder, Graph, Query};
use serde_json::{Map, Value};
use tokio::time::timeout;

use crate::config::{Config, Limits};
use crate::convert::{bolt_to_json, json_to_bolt};
use crate::error::Error;
use crate::response::QueryResponse;
use cypher_guard::SanitizedQuery;

use shape::RowShaper;

/// Reserved JSON key wrapping a single-column row's bare value (see `row_to_json`).
const KEY_VALUE: &str = "value";
/// Reserved JSON key for a row whose Bolt→JSON conversion failed (see `row_to_json`).
const KEY_ROW_ERROR: &str = "_row_error";

/// Logs a warning if `uri` connects to a non-local host without transport
/// security: plaintext `bolt://`/`neo4j://`, or a `+ssc` scheme that encrypts but
/// skips certificate validation (MITM-able). neo4rs derives encryption from the
/// scheme, and the gateway passes the URI through unchanged, so this is the only
/// place the insecure-for-remote case is surfaced.
fn warn_if_insecure_uri(uri: &str) {
    let (scheme, rest) = uri.split_once("://").unwrap_or(("", uri));
    let scheme = scheme.to_ascii_lowercase();
    // Extract the host exactly: drop any path/query, optional `user@`, and the
    // `:port`, handling a bracketed IPv6 literal (`[::1]:7687`).
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host_port.strip_prefix('[').map_or_else(
        || host_port.rsplit_once(':').map_or(host_port, |(h, _)| h),
        |v6| v6.split_once(']').map_or(host_port, |(h, _)| h),
    );
    let loopback = matches!(host, "localhost" | "127.0.0.1" | "::1") || host.starts_with("127.");
    let plaintext = scheme == "bolt" || scheme == "neo4j";
    let unvalidated_tls = scheme.ends_with("+ssc");
    if !loopback && (plaintext || unvalidated_tls) {
        tracing::warn!(
            scheme = %scheme,
            host = %host,
            "Neo4j URI is unencrypted or skips certificate validation for a non-local host; \
             use a 'bolt+s://' or 'neo4j+s://' URI for remote connections"
        );
    }
}

/// A cheaply-cloneable handle to a read-only Neo4j connection pool.
#[derive(Clone)]
pub(crate) struct Executor {
    inner: Arc<Inner>,
}

struct Inner {
    graph: Graph,
    limits: Limits,
    query_timeout: Duration,
}

impl std::fmt::Debug for Executor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Executor").finish_non_exhaustive()
    }
}

impl Executor {
    /// Connects to Neo4j using the gateway configuration.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the driver configuration is invalid or the
    /// connection cannot be established.
    pub(crate) async fn connect(config: &Config) -> Result<Self, Error> {
        warn_if_insecure_uri(&config.neo4j_uri);
        let driver_cfg = ConfigBuilder::default()
            .uri(&config.neo4j_uri)
            .user(&config.neo4j_user)
            .password(config.neo4j_password.expose_secret())
            .db("neo4j")
            // One batch covers the largest result we will ever return (the row
            // cap is the true upper bound on returned rows), so even a maximal
            // request needs a single PULL; +1 lets a row-cap be detected.
            .fetch_size(config.limits.max_result_rows as usize + 1)
            .build()
            // Connect-time failures are operational, not query syntax errors, so
            // they map straight to Internal (from_neo4rs is execution-only).
            .map_err(Error::internal)?;
        let graph = Graph::connect(driver_cfg).await.map_err(Error::internal)?;
        Ok(Self {
            inner: Arc::new(Inner {
                graph,
                limits: config.limits,
                query_timeout: config.query_timeout,
            }),
        })
    }

    /// Executes a sanitized query with the given bound parameters.
    ///
    /// Parameters are bound natively (never interpolated). The result is capped
    /// at the configured row and byte budgets.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] for a driver/syntax failure or a timeout.
    pub(crate) async fn execute(
        &self,
        query: &SanitizedQuery,
        params: &Map<String, Value>,
        requested_limit: Option<u32>,
    ) -> Result<QueryResponse, Error> {
        let budget = row_budget(&self.inner.limits, requested_limit);
        let mut neo_query = Query::new(query.cypher().to_owned());
        for (key, value) in params {
            neo_query = neo_query.param(key, json_to_bolt(value)?);
        }

        // Own the transaction and stream outside the timeout future so their
        // borrows end when the future resolves, leaving `txn` free to move into
        // commit/rollback.
        let mut txn = self.inner.graph.start_txn().await.map_err(Error::from_neo4rs)?;
        let mut stream = txn.execute(neo_query).await.map_err(Error::from_neo4rs)?;
        let byte_cap = self.inner.limits.max_result_bytes;
        let timeout_budget = self.inner.query_timeout;

        let started = std::time::Instant::now();
        let read = read_rows(&mut stream, &mut txn, budget, byte_cap);
        let result = match timeout(timeout_budget, read).await {
            Ok(Ok(response)) => {
                // A read transaction has nothing to commit; this just releases it.
                let _ = txn.commit().await;
                Ok(response)
            }
            Ok(Err(e)) => {
                rollback_quietly(txn, timeout_budget).await;
                Err(e)
            }
            Err(_elapsed) => {
                rollback_quietly(txn, timeout_budget).await;
                Err(Error::timeout(duration_ms(timeout_budget)))
            }
        };
        tracing::debug!(
            elapsed_ms = duration_ms(started.elapsed()),
            ok = result.is_ok(),
            "query executed"
        );
        result
    }
}

/// Drains rows from the stream into a shaper, converting each immediately.
async fn read_rows(
    stream: &mut neo4rs::RowStream,
    txn: &mut neo4rs::Txn,
    budget: usize,
    byte_cap: usize,
) -> Result<QueryResponse, Error> {
    let mut shaper = RowShaper::new(budget, byte_cap);
    while let Some(row) = stream.next(txn.handle()).await.map_err(Error::from_neo4rs)? {
        let json = row_to_json(&row);
        if !shaper.push(json) {
            break;
        }
    }
    let (rows, truncated) = shaper.finish();
    Ok(QueryResponse::new(rows, truncated))
}

/// Converts a Neo4j row into a JSON object keyed by column name. The driver
/// yields columns from a `HashMap` in nondeterministic order, so keys are sorted
/// alphabetically for stable output.
fn row_to_json(row: &neo4rs::Row) -> Map<String, Value> {
    match row.to::<neo4rs::BoltType>() {
        Ok(neo4rs::BoltType::Map(map)) => {
            let mut pairs: Vec<(String, Value)> = map
                .value
                .iter()
                .map(|(k, v)| (k.value.clone(), bolt_to_json(v)))
                .collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            pairs.into_iter().collect()
        }
        // A single-column row deserializes to the bare value; wrap it.
        Ok(other) => single(KEY_VALUE, bolt_to_json(&other)),
        // Converting a row to the universal `BoltType` does not fail in practice;
        // if it ever does, surface an observable marker rather than a silent empty
        // row, so dropped data is never mistaken for an absent result.
        Err(e) => {
            tracing::warn!(error = %e, "row conversion failed; returning an error marker row");
            single(KEY_ROW_ERROR, Value::String("row conversion failed".to_owned()))
        }
    }
}

/// A one-key JSON object.
fn single(key: &str, value: Value) -> Map<String, Value> {
    let mut m = Map::new();
    m.insert(key.to_owned(), value);
    m
}

async fn rollback_quietly(txn: neo4rs::Txn, budget: Duration) {
    // The connection may still be busy with a timed-out query, so bound the
    // rollback too; on failure the transaction is dropped (which returns the
    // connection to the pool) and the server-side timeout remains the backstop.
    match timeout(budget, txn.rollback()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "transaction rollback failed; relying on server-side timeout"),
        Err(_) => tracing::warn!("transaction rollback timed out; relying on server-side timeout"),
    }
}

fn row_budget(limits: &Limits, requested: Option<u32>) -> usize {
    let effective = requested.unwrap_or(limits.default_limit).min(limits.max_result_rows);
    effective as usize
}

fn duration_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}
