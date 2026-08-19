# ADR-0005: Explicit read transaction; client-liveness vs server-resource timeout

**Status:** Accepted (2026-06-01)

## Context

The obvious shape is a driver-level per-query timeout (`transaction.run_with_timeout()`). No such
API exists in `neo4rs` 0.8. Worse, `Graph::execute`/`run` wrap the query in a transient-error retry loop
with exponential backoff up to ~60 s, which would blow any liveness budget. Dropping a row stream
does not reliably stop server-side work.

## Decision

Execute each query inside an **explicit transaction** (`Graph::start_txn` + `Txn::execute`), which is
single-shot (not wrapped in the retry loop). Wrap the whole read loop in a single
`tokio::time::timeout`; the transaction and stream are owned outside the timeout future so their
borrows end when it resolves, leaving the transaction free to move into commit/rollback. On timeout,
roll back (itself time-bounded; dropped on failure). Set the driver `fetch_size` to `row_budget + 1`
so the first `PULL` does not over-fetch.

Treat the client timeout as a **liveness** bound only. The authoritative bound on server resources is
the Neo4j `db.transaction.timeout` and transaction memory limit configured server-side.

## Consequences

- ✅ The caller never waits past the budget, and a hostile query does not get silently retried for a
  minute.
- ✅ The borrow/timeout/rollback shape is expressible in safe async Rust and compiles.
- ⚠️ Server-side resource protection depends on the operator setting `db.transaction.timeout` and
  memory limits (documented in the reader-setup script and docker-compose). A single-row
  `collect()` over the whole graph is bounded by the *server* memory limit, not the client byte cap
  (which can only act between whole rows).
- ⚠️ The sanitizer bounds *classic* variable-length paths (`*`, `*2..`) to `*1..5`, but does **not**
  bound Neo4j 5 quantified path patterns (`(...)+`, `(...){1,n}`), which a lexer-level guard cannot
  recognize without a parser. These are a traversal-cost (availability) concern, not a write hole, and
  the server-side transaction timeout is their backstop. Bounding QPP in the sanitizer is possible
  future work if cost control needs to move client-side.

## Alternatives considered

- **`Graph::execute` with a `tokio` timeout**: the internal 60 s retry loop makes the budget
  meaningless for server work. Rejected.
- **Patch `neo4rs` to expose a Bolt `tx_timeout`**: out of scope for the MVP; the server-side config
  is the supported lever today.
