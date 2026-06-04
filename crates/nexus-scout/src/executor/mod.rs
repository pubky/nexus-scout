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

use crate::config::{Config, Limits, Profile};
use crate::convert::{bolt_to_json, json_to_bolt};
use crate::error::Error;
use crate::response::QueryResponse;
use cypher_guard::SanitizedQuery;

use shape::RowShaper;

/// Reserved JSON key wrapping a single-column row's bare value (see `row_to_json`).
const KEY_VALUE: &str = "value";
/// Reserved JSON key for a row whose Bolt→JSON conversion failed (see `row_to_json`).
const KEY_ROW_ERROR: &str = "_row_error";

/// A query slower than this is logged at `WARN` (with a fingerprint, never the
/// text) so abuse can be triaged without logging user content.
const SLOW_QUERY_THRESHOLD: Duration = Duration::from_secs(2);

/// Neo4j settings that bound per-query cost. Verified at startup/readiness so a
/// forgotten `neo4j.conf` line cannot leave the public endpoint unbounded.
const REQUIRED_BOUNDS: [&str; 3] = [
    "db.transaction.timeout",
    "db.memory.transaction.max",
    "dbms.memory.transaction.total.max",
];

/// Returns a reason if `uri` is insecure for a non-local host: plaintext
/// `bolt://`/`neo4j://`, or a `+ssc` scheme that encrypts but skips certificate
/// validation (MITM-able). neo4rs derives encryption from the scheme and the
/// gateway passes the URI through unchanged, so this is the one place the
/// insecure-for-remote case is detected. Loopback hosts are always fine.
fn insecure_remote_reason(uri: &str) -> Option<&'static str> {
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
    if loopback {
        return None;
    }
    if scheme == "bolt" || scheme == "neo4j" {
        Some("is unencrypted (plaintext)")
    } else if scheme.ends_with("+ssc") {
        Some("skips TLS certificate validation (+ssc)")
    } else {
        None
    }
}

/// A non-loopback Neo4j URI used over an insecure scheme in production.
#[derive(Debug)]
struct InsecureUri(&'static str);

impl std::fmt::Display for InsecureUri {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "NEO4J_URI {} for a non-local host; use bolt+s:// for remote connections",
            self.0
        )
    }
}

impl std::error::Error for InsecureUri {}

/// Whether a `SHOW SETTINGS` value represents a real bound (`10s`, `512MiB`) as
/// opposed to the unlimited sentinel (`0s`, `0B`, empty).
fn is_bounded(value: &str) -> bool {
    let num: String = value
        .trim()
        .chars()
        .take_while(|c| c.is_ascii_digit() || *c == '.')
        .collect();
    num.parse::<f64>().is_ok_and(|n| n > 0.0)
}

/// A stable fingerprint of a query, for slow-query logs that must not contain the
/// query text or parameters.
fn query_fingerprint(cypher: &str) -> String {
    use std::hash::{Hash, Hasher};
    let mut h = std::collections::hash_map::DefaultHasher::new();
    cypher.hash(&mut h);
    format!("{:016x}", h.finish())
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
        if let Some(reason) = insecure_remote_reason(&config.neo4j_uri) {
            if config.profile == Profile::Production {
                tracing::error!(
                    reason,
                    "refusing to start: insecure NEO4J_URI for a non-local host in production"
                );
                return Err(Error::bad_config("NEO4J_URI", InsecureUri(reason)));
            }
            tracing::warn!(
                reason,
                "insecure Neo4j URI for a non-local host; use bolt+s:// for remote connections"
            );
        }
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
        let elapsed = started.elapsed();
        tracing::debug!(elapsed_ms = duration_ms(elapsed), ok = result.is_ok(), "query executed");
        if elapsed >= SLOW_QUERY_THRESHOLD {
            // Fingerprint only: never log the query text or parameters.
            tracing::warn!(
                query_fingerprint = %query_fingerprint(query.cypher()),
                elapsed_ms = duration_ms(elapsed),
                row_count = result.as_ref().map_or(0, |r| r.count),
                "slow query"
            );
        }
        result
    }

    /// Returns the names of any [`REQUIRED_BOUNDS`] server settings that are unset
    /// or unbounded (empty = all configured). Runs `SHOW SETTINGS` as a **direct**
    /// driver call: this is gateway-originated, not user input, so it deliberately
    /// does not pass through the sanitizer (which denies `SHOW`).
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the check itself cannot run (e.g. the connecting user
    /// lacks permission to read settings); the caller treats that as "degraded",
    /// not "unbounded".
    pub(crate) async fn verify_server_bounds(&self) -> Result<Vec<&'static str>, Error> {
        let query = neo4rs::query("SHOW SETTINGS YIELD name, value WHERE name IN $names RETURN name, value")
            .param("names", REQUIRED_BOUNDS.to_vec());
        let mut stream = self.inner.graph.execute(query).await.map_err(Error::from_neo4rs)?;
        let mut bounded: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
        while let Some(row) = stream.next().await.map_err(Error::from_neo4rs)? {
            let name = row.get::<String>("name").unwrap_or_default();
            let value = row.get::<String>("value").unwrap_or_default();
            if let Some(known) = REQUIRED_BOUNDS.iter().find(|b| **b == name) {
                if is_bounded(&value) {
                    bounded.insert(*known);
                }
            }
        }
        Ok(REQUIRED_BOUNDS
            .iter()
            .copied()
            .filter(|b| !bounded.contains(b))
            .collect())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn loopback_uris_are_never_insecure() {
        for uri in [
            "bolt://localhost:7687",
            "bolt://127.0.0.1:7687",
            "neo4j://[::1]:7687",
            "bolt://127.5.5.5:7687",
        ] {
            assert_eq!(insecure_remote_reason(uri), None, "{uri}");
        }
    }

    #[test]
    fn remote_plaintext_and_ssc_are_flagged_but_tls_is_fine() {
        assert!(insecure_remote_reason("bolt://neo4j.example.com:7687").is_some());
        assert!(insecure_remote_reason("neo4j://db.internal:7687").is_some());
        assert!(insecure_remote_reason("bolt+ssc://db.example.com:7687").is_some());
        assert_eq!(insecure_remote_reason("bolt+s://db.example.com:7687"), None);
        assert_eq!(insecure_remote_reason("neo4j+s://db.example.com:7687"), None);
    }

    #[test]
    fn bounds_sentinels_are_unbounded() {
        for unbounded in ["0s", "0B", "0", "0.00MiB", "", "  "] {
            assert!(!is_bounded(unbounded), "{unbounded:?} should read as unbounded");
        }
        for bounded in ["10s", "512.00MiB", "64MiB", "1m30s"] {
            assert!(is_bounded(bounded), "{bounded:?} should read as bounded");
        }
    }

    #[test]
    fn fingerprint_is_stable_and_leaks_no_text() {
        let q = "MATCH (u:User {secret:'hunter2'}) RETURN u";
        let fp = query_fingerprint(q);
        assert_eq!(fp, query_fingerprint(q));
        assert!(!fp.contains("hunter2"));
        assert_ne!(fp, query_fingerprint("MATCH (u:User) RETURN u"));
    }
}
