//! Read-only Neo4j query execution inside an explicit read transaction. Two
//! timeouts cooperate: a client-side `tokio::time::timeout` bounds liveness; the
//! real bound on server resources is the Neo4j `db.transaction.timeout`/memory
//! limit for the reader role. An explicit transaction (not `Graph::execute`) is
//! used deliberately: only `execute`/`run` are wrapped in the driver's
//! transient-error retry loop, which would defeat the liveness budget.

mod shape;

use std::net::IpAddr;
use std::sync::Arc;
use std::time::Duration;

use neo4rs::{ConfigBuilder, Graph, Query};
use serde_json::{Map, Value};
use tokio::time::timeout;

use crate::config::{Config, Limits, Profile};
use crate::convert::{json_to_bolt, row_to_json};
use crate::error::Error;
use crate::response::QueryResponse;
use cypher_guard::{ResultLimit, SanitizedQuery};

use shape::{RowShaper, Truncation};

/// A query slower than this is logged at `WARN` (with a fingerprint, never the
/// text) so abuse can be triaged without logging user content.
const SLOW_QUERY_THRESHOLD: Duration = Duration::from_secs(2);

/// Short fixed cap on the post-query rollback. Rollback is cleanup, not the query,
/// so it must not re-spend the query budget: a timed-out query already waited the
/// full budget, and giving the rollback the same budget doubled the time-to-error.
/// If it cannot finish in time the connection is dropped (and reset by the pool).
const ROLLBACK_TIMEOUT: Duration = Duration::from_secs(2);

/// Neo4j settings that bound per-query cost. Verified at startup/readiness so a
/// forgotten `neo4j.conf` line cannot leave the public endpoint unbounded.
const REQUIRED_BOUNDS: [&str; 3] = [
    "db.transaction.timeout",
    "db.memory.transaction.max",
    "dbms.memory.transaction.total.max",
];

/// Returns a reason if `uri` is insecure for a non-local host: plaintext
/// `bolt`/`neo4j`, a `+ssc` scheme (encrypts but skips cert validation), or any
/// unknown/missing scheme. Only `bolt+s`/`neo4j+s` and loopback hosts are accepted
/// silently.
fn insecure_remote_reason(uri: &str) -> Option<&'static str> {
    let (scheme, rest) = uri.split_once("://").unwrap_or(("", uri));
    let scheme = scheme.to_ascii_lowercase();
    // Extract the host: drop path/query, optional `user@`, and `:port` (handle `[::1]`).
    let authority = rest.split(['/', '?', '#']).next().unwrap_or(rest);
    let host_port = authority.rsplit_once('@').map_or(authority, |(_, h)| h);
    let host = host_port.strip_prefix('[').map_or_else(
        || host_port.rsplit_once(':').map_or(host_port, |(h, _)| h),
        |v6| v6.split_once(']').map_or(host_port, |(h, _)| h),
    );
    // Loopback by IP (127.0.0.0/8, ::1), not string prefix: `127.evil.com` is not loopback.
    let loopback = host
        .parse::<IpAddr>()
        .map_or(host == "localhost", |ip| ip.is_loopback());
    if loopback {
        return None;
    }
    // Allow-list only encrypted-and-validated schemes; everything else is insecure for a remote.
    match scheme.as_str() {
        "bolt+s" | "neo4j+s" => None,
        "bolt" | "neo4j" => Some("is unencrypted (plaintext)"),
        s if s.ends_with("+ssc") => Some("skips TLS certificate validation (+ssc)"),
        _ => Some("uses an unrecognized or missing scheme"),
    }
}

/// What to do about a URI already known to be insecure for a non-local host,
/// given the profile and the explicit override. Pure, so the policy is
/// unit-tested without a live connection.
#[derive(Debug, PartialEq, Eq)]
enum TransportPolicy {
    /// Tolerated in development: proceed with a warning.
    WarnDevelopment,
    /// Explicitly permitted via the override: proceed, warning loudly.
    WarnOverride,
    /// Refused (production, no override): fail closed.
    Reject,
}

fn transport_policy(profile: Profile, allow_insecure: bool) -> TransportPolicy {
    match (profile, allow_insecure) {
        (Profile::Production, false) => TransportPolicy::Reject,
        (Profile::Production, true) => TransportPolicy::WarnOverride,
        (Profile::Development, _) => TransportPolicy::WarnDevelopment,
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

/// A process-local fingerprint of a query, for slow-query logs that must not
/// contain the query text or parameters. Stable only within one process lifetime.
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
        if let Some(detail) = insecure_remote_reason(&config.neo4j_uri) {
            match transport_policy(config.profile, config.allow_insecure_transport) {
                TransportPolicy::WarnDevelopment => tracing::warn!(
                    reason = detail,
                    "insecure Neo4j URI for a non-local host; use bolt+s:// for remote connections"
                ),
                TransportPolicy::WarnOverride => tracing::warn!(
                    reason = detail,
                    "insecure Neo4j URI permitted by NEO4J_ALLOW_INSECURE_TRANSPORT; only safe on a \
                     trusted private network (e.g. an isolated Docker network)"
                ),
                TransportPolicy::Reject => {
                    tracing::error!(
                        reason = detail,
                        "refusing to start: insecure NEO4J_URI for a non-local host in production"
                    );
                    return Err(Error::bad_config("NEO4J_URI", InsecureUri(detail)));
                }
            }
        }
        let driver_cfg = ConfigBuilder::default()
            .uri(&config.neo4j_uri)
            .user(&config.neo4j_user)
            .password(config.neo4j_password.expose_secret())
            .db("neo4j")
            // One batch covers the largest result (row cap is the true upper bound); +1 detects a row-cap.
            .fetch_size(config.limits.max_result_rows as usize + 1)
            // Size the pool to the HTTP concurrency cap; otherwise neo4rs's default (16)
            // would make admitted requests beyond 16 stall on connection acquire.
            .max_connections(config.neo4j_max_connections)
            .build()
            // Connect failures are operational, not syntax, so map straight to Internal.
            .map_err(Error::internal)?;
        // Bound the connect like every DB call so an unreachable host fails the budget, not hangs.
        let graph = match timeout(config.query_timeout, Graph::connect(driver_cfg)).await {
            Ok(result) => result.map_err(Error::internal)?,
            Err(_elapsed) => return Err(Error::internal("Neo4j connection timed out")),
        };
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
        let (budget, budget_source) = row_budget(&self.inner.limits, requested_limit, query.result_limit());
        let mut neo_query = Query::new(query.cypher().to_owned());
        for (key, value) in params {
            neo_query = neo_query.param(key, json_to_bolt(value)?);
        }

        let byte_cap = self.inner.limits.max_result_bytes;
        let timeout_budget = self.inner.query_timeout;
        let started = std::time::Instant::now();

        // One deadline across every DB phase (pool acquire + start, execute, drain),
        // so the whole interaction respects the liveness budget — not just the drain.
        // txn/stream are owned out here so their borrows end before commit/rollback.
        let deadline = started + timeout_budget;
        let remaining = || deadline.saturating_duration_since(std::time::Instant::now());

        let mut txn = match timeout(remaining(), self.inner.graph.start_txn()).await {
            Ok(r) => r.map_err(Error::from_neo4rs)?,
            Err(_elapsed) => return Err(Error::timeout(duration_ms(timeout_budget))),
        };
        let mut stream = match timeout(remaining(), txn.execute(neo_query)).await {
            Ok(r) => r.map_err(Error::from_neo4rs)?,
            Err(_elapsed) => {
                rollback_quietly(txn).await;
                return Err(Error::timeout(duration_ms(timeout_budget)));
            }
        };

        let read = read_rows(&mut stream, &mut txn, budget, byte_cap);
        let result = match timeout(remaining(), read).await {
            Ok(Ok((mut response, truncation))) => {
                // A read txn has nothing to commit; this releases it. A failure here is cleanup, logged not surfaced.
                if let Err(e) = txn.commit().await {
                    tracing::warn!(error = %e, "read transaction commit/release failed; connection returned to pool on drop");
                }
                // Name the cap that actually fired. Blaming the row budget for a
                // byte-cap cut sends the caller paging for rows that will never come.
                let count = response.count;
                response.notes.extend(result_note(
                    truncation,
                    budget_source,
                    budget,
                    count,
                    &self.inner.limits,
                ));
                Ok(response)
            }
            Ok(Err(e)) => {
                rollback_quietly(txn).await;
                Err(e)
            }
            Err(_elapsed) => {
                rollback_quietly(txn).await;
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

    /// Returns the names of any [`REQUIRED_BOUNDS`] settings that are unset or
    /// unbounded (empty = all configured). Runs `SHOW SETTINGS` as a direct,
    /// gateway-originated driver call that deliberately bypasses the sanitizer
    /// (which denies `SHOW`).
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the check itself cannot run (e.g. the connecting user
    /// lacks permission to read settings); the caller treats that as "degraded",
    /// not "unbounded".
    pub(crate) async fn verify_server_bounds(&self) -> Result<Vec<&'static str>, Error> {
        // Bound the check like every DB call: `Graph::execute` is wrapped in the
        // driver's retry loop, so a flaky Neo4j could otherwise stall the probe for
        // the backoff window.
        let check = async {
            let query = neo4rs::query("SHOW SETTINGS YIELD name, value WHERE name IN $names RETURN name, value")
                .param("names", REQUIRED_BOUNDS.to_vec());
            let mut stream = self.inner.graph.execute(query).await.map_err(Error::from_neo4rs)?;
            let mut bounded: std::collections::HashSet<&'static str> = std::collections::HashSet::new();
            // Surface a row-shape mismatch as a verification error, not "unset", so production fails closed on it.
            let shape_err = |e| Error::internal(format!("SHOW SETTINGS returned an unexpected row shape: {e}"));
            while let Some(row) = stream.next().await.map_err(Error::from_neo4rs)? {
                let name = row.get::<String>("name").map_err(shape_err)?;
                let value = row.get::<String>("value").map_err(shape_err)?;
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
                .collect::<Vec<_>>())
        };
        match timeout(self.inner.query_timeout, check).await {
            Ok(result) => result,
            Err(_elapsed) => Err(Error::internal("server-side cost-bound verification timed out")),
        }
    }
}

/// Drains rows from the stream into a shaper, converting each immediately.
async fn read_rows(
    stream: &mut neo4rs::RowStream,
    txn: &mut neo4rs::Txn,
    budget: usize,
    byte_cap: usize,
) -> Result<(QueryResponse, Option<Truncation>), Error> {
    let mut shaper = RowShaper::new(budget, byte_cap);
    while let Some(row) = stream.next(txn.handle()).await.map_err(Error::from_neo4rs)? {
        let json = row_to_json(&row);
        if !shaper.push(json) {
            break;
        }
    }
    let (rows, truncated) = shaper.finish();
    Ok((QueryResponse::new(rows, truncated.is_some()), truncated))
}

async fn rollback_quietly(txn: neo4rs::Txn) {
    // Bounded by a short fixed cap, not the query budget (see ROLLBACK_TIMEOUT): the
    // connection may still be busy, and on failure dropping it lets the pool reset it.
    match timeout(ROLLBACK_TIMEOUT, txn.rollback()).await {
        Ok(Ok(())) => {}
        Ok(Err(e)) => tracing::warn!(error = %e, "transaction rollback failed; connection will be recycled"),
        Err(_) => tracing::warn!("transaction rollback timed out; connection will be recycled"),
    }
}

/// How the row budget was arrived at. Carried alongside the budget so a truncated
/// result can say *why* it stopped: a caller who never sees the rule it hit reports
/// the capped count as the real total.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BudgetSource {
    /// The query's own `LIMIT n` set the budget, so *Neo4j* ends the stream. The
    /// gateway therefore never sees an n+1'th row and cannot tell a complete answer
    /// from a page, which is why a full page under this source needs saying.
    QueryLimit,
    /// The request's `limit` field set the budget, so the *gateway* does the cutting
    /// and a further row would have been seen. A full page here really is complete.
    Requested,
    /// Nothing asked for a limit, so the default applied. The gateway does the
    /// cutting, so the same completeness reasoning as [`Self::Requested`] holds.
    Default,
    /// The limit asked for exceeded `max_result_rows` and was cut to it.
    Capped(u32),
    /// `LIMIT $n`, whose value is bound at execution and unknown here. The read runs
    /// to the ceiling; if the ceiling stops it, the gateway did the cutting.
    Ceiling,
}

impl BudgetSource {
    /// Whether a result that exactly fills the budget is ambiguous. True only when
    /// a server-side `LIMIT` ended the stream, since then "exactly the limit" and
    /// "everything there is" are indistinguishable from here.
    const fn full_page_is_ambiguous(self) -> bool {
        matches!(self, Self::QueryLimit | Self::Ceiling)
    }
}

/// The number of rows to read, capped at `max_result_rows`, and why. An explicit
/// request limit (`--limit` / HTTP `limit`) always wins; otherwise the query's own
/// top-level `LIMIT` is honored, so an agent's natural `... LIMIT 50` returns 50
/// without needing a separate flag. A parameterized `LIMIT $n` reads up to the
/// ceiling and lets the server-side `LIMIT` do the cut; no limit falls back to the
/// default.
fn row_budget(limits: &Limits, requested: Option<u32>, query_limit: ResultLimit) -> (usize, BudgetSource) {
    let (asked, source) = match requested {
        Some(r) => (r, BudgetSource::Requested),
        None => match query_limit {
            ResultLimit::Fixed(n) => (n, BudgetSource::QueryLimit),
            ResultLimit::Dynamic => (limits.max_result_rows, BudgetSource::Ceiling),
            ResultLimit::None => (limits.default_limit, BudgetSource::Default),
        },
    };
    // An over-large ask from the caller is a cap worth naming. A `default_limit`
    // misconfigured above the ceiling is not the caller's doing, so that case keeps
    // reporting "no LIMIT" rather than quoting a limit they never wrote.
    let source =
        if asked > limits.max_result_rows && matches!(source, BudgetSource::Requested | BudgetSource::QueryLimit) {
            BudgetSource::Capped(asked)
        } else {
            source
        };
    (asked.min(limits.max_result_rows) as usize, source)
}

/// The disclosure note for a shaped result, or `None` when there is nothing the
/// caller could misread. Numbers come from `limits`, which is env-configurable, so
/// they are never hardcoded.
fn result_note(
    truncation: Option<Truncation>,
    source: BudgetSource,
    budget: usize,
    returned: usize,
    limits: &Limits,
) -> Option<String> {
    let Some(truncation) = truncation else {
        // Nothing was cut. But when a server-side LIMIT ended the stream, a page that
        // exactly fills the budget is indistinguishable on the wire from a complete
        // answer: `truncated` stays false because the gateway never saw a further row.
        // That shape reads as "this is all of them" and is the single easiest way to
        // report a confidently wrong total, so it gets said out loud.
        return (source.full_page_is_ambiguous() && returned > 0 && returned == budget).then(|| {
            format!(
                "returned exactly the {returned}-row limit, so there may be more; \
                 re-run with SKIP {returned}, or compare against a count() to get the true total"
            )
        });
    };
    match truncation {
        // Paging is the wrong advice here: the rows themselves are too big, so the
        // next page is cut the same way. Narrowing the RETURN is the fix.
        Truncation::ByteCap => Some(format!(
            "result payload reached the {} KiB cap, so the remaining rows were dropped; \
             return fewer or smaller columns rather than paging",
            limits.max_result_bytes / 1024
        )),
        Truncation::RowBudget => match source {
            BudgetSource::QueryLimit | BudgetSource::Requested => None,
            BudgetSource::Default => Some(format!(
                "no LIMIT in the query, so the default of {} rows applied (maximum {}); \
                 add a LIMIT, or page with SKIP, to read further",
                limits.default_limit, limits.max_result_rows
            )),
            BudgetSource::Capped(asked) => Some(format!(
                "the requested limit of {asked} was capped to the maximum of {} rows; \
                 page with SKIP to read further",
                limits.max_result_rows
            )),
            BudgetSource::Ceiling => Some(format!(
                "the parameterized LIMIT exceeded the maximum of {} rows and was capped; \
                 page with SKIP to read further",
                limits.max_result_rows
            )),
        },
    }
}

fn duration_ms(d: Duration) -> u64 {
    u64::try_from(d.as_millis()).unwrap_or(u64::MAX)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn row_budget_reports_the_rule_it_applied() {
        let l = Limits::default(); // default_limit 25, max_result_rows 100
        let budget = |requested, query_limit| row_budget(&l, requested, query_limit);

        // No limit anywhere: the default applies and says so.
        assert_eq!(budget(None, ResultLimit::None), (25, BudgetSource::Default));
        // The query's own LIMIT and the request's `limit` field are tracked apart:
        // only the former lets Neo4j end the stream, which changes what a full page
        // can be trusted to mean.
        assert_eq!(budget(None, ResultLimit::Fixed(50)), (50, BudgetSource::QueryLimit));
        assert_eq!(budget(Some(10), ResultLimit::None), (10, BudgetSource::Requested));
        // Exactly at the ceiling is honored, not "capped".
        assert_eq!(budget(None, ResultLimit::Fixed(100)), (100, BudgetSource::QueryLimit));
        // Over the ceiling: cut, and the original ask is kept for the note.
        assert_eq!(budget(None, ResultLimit::Fixed(500)), (100, BudgetSource::Capped(500)));
        assert_eq!(
            budget(Some(500), ResultLimit::Fixed(5)),
            (100, BudgetSource::Capped(500))
        );
        // `LIMIT $n` reads to the ceiling. Its value is unknown here, so the source
        // records that the ceiling is what bounds the read.
        assert_eq!(budget(None, ResultLimit::Dynamic), (100, BudgetSource::Ceiling));

        // A default misconfigured above the ceiling is not the caller's doing: it
        // still reports "no LIMIT" rather than quoting a limit they never wrote.
        let misconfigured = Limits {
            default_limit: 500,
            max_result_rows: 100,
            ..Limits::default()
        };
        assert_eq!(
            row_budget(&misconfigured, None, ResultLimit::None),
            (100, BudgetSource::Default)
        );
    }

    #[test]
    fn a_full_page_says_so_only_when_it_could_be_hiding_more() {
        let l = Limits::default();
        // Nothing cut, and the page exactly fills the budget.
        let full = |s, budget| result_note(None, s, budget, budget, &l);

        // Neo4j's LIMIT ended the stream, so "exactly 50" and "all of them" look the
        // same from here. This is the case that turned 134 friends into 100.
        let q = full(BudgetSource::QueryLimit, 50).expect("ambiguous, must be disclosed");
        assert!(q.contains("50") && q.contains("SKIP") && q.contains("count()"), "{q}");
        assert!(full(BudgetSource::Ceiling, 100).is_some());

        // The gateway did the cutting, so it would have seen a further row. A full
        // page here really is complete and must stay quiet, or every ordinary query
        // gets a scary note.
        assert!(full(BudgetSource::Default, 25).is_none());
        assert!(full(BudgetSource::Requested, 10).is_none());

        // Under budget is complete under any source; zero rows is not a "full page".
        assert!(result_note(None, BudgetSource::QueryLimit, 50, 12, &l).is_none());
        assert!(result_note(None, BudgetSource::QueryLimit, 50, 0, &l).is_none());
    }

    #[test]
    fn truncation_note_names_the_cap_that_actually_fired() {
        let l = Limits::default();
        let note = |t, s| result_note(t, s, 100, 100, &l);

        // Row budget, but the caller got exactly the limit it asked for.
        assert!(note(Some(Truncation::RowBudget), BudgetSource::Requested).is_none());

        let default = note(Some(Truncation::RowBudget), BudgetSource::Default).expect("worth disclosing");
        assert!(default.contains("25") && default.contains("100"), "{default}");
        assert!(default.contains("SKIP"), "must name the way past the cap: {default}");

        let capped = note(Some(Truncation::RowBudget), BudgetSource::Capped(500)).expect("worth disclosing");
        assert!(capped.contains("500") && capped.contains("100"), "{capped}");

        let ceiling = note(Some(Truncation::RowBudget), BudgetSource::Ceiling).expect("worth disclosing");
        assert!(ceiling.contains("100"), "{ceiling}");

        // The byte cap must never be reported as a row cap: paging is the wrong fix,
        // and the caller would page forever getting the same dropped rows.
        let bytes = note(Some(Truncation::ByteCap), BudgetSource::Default).expect("worth disclosing");
        assert!(bytes.contains("1024 KiB"), "should name the byte cap: {bytes}");
        assert!(
            !bytes.contains("SKIP") && !bytes.contains("no LIMIT"),
            "byte-cap advice must not send the caller paging: {bytes}"
        );
    }

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
    fn a_127_prefixed_hostname_is_not_loopback() {
        // Regression: `127.evil.com` is not 127.0.0.0/8 and must be flagged.
        assert!(insecure_remote_reason("bolt://127.evil.com:7687").is_some());
        assert!(insecure_remote_reason("bolt://127.0.0.1.attacker.example:7687").is_some());
    }

    #[test]
    fn unknown_or_missing_scheme_is_flagged_not_assumed_safe() {
        assert!(insecure_remote_reason("bolt+foo://evil.com:7687").is_some());
        assert!(insecure_remote_reason("evil.com:7687").is_some());
    }

    #[test]
    fn host_parsing_handles_userinfo_and_bracketed_ipv6() {
        // `user@` is stripped; the host past it drives the decision.
        assert!(insecure_remote_reason("bolt://user:pass@evil.com:7687").is_some());
        assert!(insecure_remote_reason("bolt://[2001:db8::1]:7687").is_some());
        assert_eq!(insecure_remote_reason("bolt://[::1]:7687"), None);
        assert_eq!(insecure_remote_reason("neo4j+s://user@db.example.com:7687"), None);
    }

    #[test]
    fn transport_policy_for_an_insecure_uri_depends_on_profile_and_override() {
        // Production refuses an insecure URI by default, unless the override is set
        // (trusted private network): proceed warning loudly. Development always warns.
        assert_eq!(transport_policy(Profile::Production, false), TransportPolicy::Reject);
        assert_eq!(
            transport_policy(Profile::Production, true),
            TransportPolicy::WarnOverride
        );
        assert_eq!(
            transport_policy(Profile::Development, false),
            TransportPolicy::WarnDevelopment
        );
        assert_eq!(
            transport_policy(Profile::Development, true),
            TransportPolicy::WarnDevelopment
        );
    }

    #[test]
    fn cost_bound_names_match_the_deployment_configs() {
        // Each name must appear (in Neo4j's NEO4J_<underscored> env form) in the
        // compose files and CI, so a rename cannot silently desync the gate from the
        // deployments that set the bounds.
        let configs = [
            include_str!("../../../../docker/docker-compose.yml"),
            include_str!("../../../../docker/docker-compose.prod.yml"),
            include_str!("../../../../.github/workflows/test.yml"),
        ];
        for bound in REQUIRED_BOUNDS {
            let env = format!("NEO4J_{}", bound.replace('.', "_"));
            for config in configs {
                assert!(config.contains(&env), "deployment config missing {env}");
            }
        }
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
