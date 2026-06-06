# nexus-scout real-testing audit

Findings from driving the live gateway against the **staging** Pubky graph (via an SSH-tunneled Bolt
connection) with five non-trivial analytics queries plus a set of deliberate edge-case probes. The
gateway behaved correctly on the large majority of cases; the items below are the things worth fixing,
documenting, or watching.

**Resolution status:** F1, F2, F4, and F5 are **resolved** (and live-verified against staging). F3 and
F6 are **neo4rs-0.8-limited** — both need an upstream API the pinned driver does not expose; F6 is
already mitigated by the production startup gate + `/ready`. See each finding for detail.

## What works well (verified live)

- **Read-only enforcement** at the HTTP boundary: every write → `QUERY_REJECTED` (exit 2), nothing
  reaches the DB.
- **Error classification & exit codes:** syntax/semantic errors → `QUERY_SYNTAX_ERROR` (exit 2);
  timeouts → `QUERY_TIMEOUT` (exit 3). Hints are actionable.
- **Timeout guardrail:** a `FOLLOWS*1..10` path-explosion was cut at 10 s with a clean
  `QUERY_TIMEOUT` envelope.
- **Path-bounding rewrite:** `*1..10` is correctly rewritten to `*1..5` (a `shortestPath *1..10`
  capped at depth 3/5 as expected).
- **Row cap / truncation:** default budget 25 with `truncated:true`; `--limit 100` raises it to the
  100 ceiling.
- **Slow-query logging:** a 2.5 s query logged `WARN slow query query_fingerprint=… elapsed_ms=2572
  row_count=1` — fingerprint only, no raw query/params (M-LOG respected).
- **`/ready` gate:** correctly reports staging is missing `db.transaction.timeout` and
  `db.memory.transaction.max` (503), i.e. it caught a real missing server-side bound.
- **Cypher surface:** `COUNT {}` subqueries (incl. **nested** `count{ … count{} … }`), inline
  relationship property maps (`[:TAGGED {label:'x'}]`), ordered `collect()[0]`, multi-stage `WITH`
  aggregations, and `id()`/`elementId()` pair-keys all executed correctly.

## Findings

### F1 — Returned nodes/relationships leak internal Neo4j fields  (Medium) — RESOLVED

`RETURN n` (whole node) yields, e.g.:
```json
{ "name":"plp", "id":"1gyx…", "bio":"", "indexed_at":1761664032824, "_id":1363, "_labels":["User"] }
```
and `RETURN r` (whole relationship) yields keys `["_end","_id","_start","_type","indexed_at"]`.

So whole-node/relationship results expose **internal Neo4j ids** (`_id`, `_start`, `_end`) and metadata
(`_labels`, `_type`). These are implementation internals — distinct from the public `id` (pubkey) — and
are noise for an agent. The conversion (`convert::bolt_to_json` Node/Relationship arms) tagged them in.

**Resolved:** whole-node/relationship returns are now **properties-only** — the synthetic
`_id`/`_labels`/`_type`/`_start`/`_end` keys are no longer inserted. This also fixes a silent collision
where a real property literally named `_id` was clobbered. Agents wanting type info use
`labels(n)` / `type(r)`. (`convert.rs`; live-verified `RETURN n` returns only real properties.)

### F2 — In-query Cypher `LIMIT` is overridden by the request row budget (default 25)  (Medium, UX) — RESOLVED

`MATCH (u:User) RETURN u.id LIMIT 9999` returns **25** rows (`truncated:true`), not 100. The returned
count was governed by the *request* budget (`min(--limit ?? 25, 100)`), so the Cypher `LIMIT 9999` was
effectively ignored; `--limit 100` was required to get 100.

This was the read-cap working as designed, but it was **counterintuitive for agents**, who naturally
express row count via Cypher `LIMIT`. An agent writing `… LIMIT 50` silently got 25.

**Resolved:** a top-level `LIMIT` in the query now drives the row budget, so the natural way agents
write `LIMIT` just works (no `--limit` needed). The sanitizer extracts the last bracket-depth-0
`LIMIT` (`ResultLimit::{None,Fixed,Dynamic}`) and the executor's `row_budget` honors it: an explicit
`--limit`/HTTP `limit` still wins; otherwise a literal `LIMIT n` sets the budget, `LIMIT $p` allows up
to the max, and no `LIMIT` keeps the default 25 — all capped at the 100 ceiling. Subquery `LIMIT`s
(inside `{}`/`()`) are ignored; for `UNION` the last branch's `LIMIT` wins. (`cypher-guard` +
`executor`; live-verified `… LIMIT 50` → 50, `--limit 10` overrides to 10.)

### F3 — One intermittent Bolt desync mis-surfaced as INTERNAL_ERROR  (Low, watch) — neo4rs-limited

Observed **once** (not reproducible afterward): a query returned `INTERNAL_ERROR` with the server log
showing `transaction rollback failed; relying on server-side timeout error=unknown message b"\xb0~"` —
a Bolt protocol desync. The pool recovered on the next query (good). Likely amplified by running over
an SSH tunnel (a partial/corrupted Bolt read), or a neo4rs 0.8 edge case on a specific error path. Two
notes: (a) the original cause is lost behind the generic INTERNAL_ERROR; (b) on a true desync the
connection should be dropped rather than attempting (and failing) a rollback on a corrupted stream.

**Status (neo4rs-limited):** neo4rs 0.8 returns a connection to the pool on `Txn` drop with **no
health-check / validate / explicit-drop hook**, so a desynced connection can't be force-evicted from
our side. The current handling stays graceful (bounded rollback, WARN, recycle on drop) and the desync
path is observable in logs. Revisit on a neo4rs bump that exposes a per-checkout health check or an
explicit connection drop. No robust code fix is possible at 0.8.

### F4 — Path-bounding rewrite is silent  (Low, by-design) — RESOLVED

`*1..10` → `*1..5` happened with **no signal** in the response (no flag, `truncated` stays the row/byte
meaning). An agent asking for 10 hops silently measured 5.

**Resolved:** the response now carries an additive `notes: []` array describing transforms the gateway
applied. A bounded path yields e.g. `notes:["variable-length path '*1..10' bounded to '*1..5'"]`.
The field is omitted from the wire when empty, so existing callers are unaffected. (`bound_paths` →
`SanitizedQuery.notes` → `QueryResponse.notes`; live-verified.)

### F5 — Generic syntax-error message  (Low, by-design tradeoff) — RESOLVED

Syntax/semantic errors returned `message:"the query could not be executed"` with no specific parser
detail (line/column/expected token). The hint was good, but an agent couldn't see *what* was wrong to
self-correct.

**Resolved:** a statement error now surfaces the Neo4j detail in `message` (e.g. `Invalid input
'RETURN': expected … (line 1, column 15)`), so an agent can self-correct. This is safe: it only echoes
the caller's own submitted query back to that same caller, and the raw query is still never *logged*
(`Scout::query` logs the `code` only). The raw neo4rs `UnexpectedMessage` wrapper (an internal debug
string) is not echoed; it falls back to the generic detail. (`error.rs` `Kind::Syntax(String)`;
live-verified the full parser detail is returned.)

### F6 — Client-side timeout vs. server-side runaway  (Low, operational) — neo4rs-limited; already gated

The `*1..10` explosion timed out at the gateway (10 s, exit 3) but ran **~12 s wall**, and staging's
Neo4j keeps computing afterward because staging lacks `db.transaction.timeout`.

**Status (neo4rs-limited; already mitigated):** neo4rs 0.8 has **no public API** to set a
per-transaction `tx_timeout` (Bolt `BEGIN` supports it, but no builder/`start_txn` variant exposes it),
so the gateway cannot inject a server-side query timeout from its side — the real bound stays Neo4j's
`db.transaction.timeout`. This is **already enforced for production**: the production-profile startup
gate refuses to boot, and `/ready` returns 503, when those bounds are unset (the audit saw the runaway
only because staging runs in *dev* profile, where the gate downgrades to an `ERROR` log naming the unset
bounds). Future fix: inject `tx_timeout` once neo4rs exposes it (or contribute it upstream).

## Not gateway issues, but relevant to interpreting results

- **Power-user dominance:** a handful of accounts (Big Bad John, Pav, Severin Alex B, SHAcollision)
  top nearly every ranking (engagement, tag diversity, pioneering). On a small graph a few hyperactive
  users drive most signals — ranking queries need normalization (e.g. per-follower) to be interesting.
- **Synthetic / test data:** `Pagination Owner <timestamp>`, padded names (`Miguel Medeirosmmmm…`,
  `BobiWWWW…`), `-stg` suffixes, and a `test` tag pollute results and must be filtered to get real
  signal. (Surfaced in the topic-bridge and trend-pioneer queries.)
