# nexus-scout behavior audit (live staging)

Empirical test of the gateway against the **staging Pubky graph** (1,682 users), driven through
the `scout` CLI → HTTP gateway → neo4rs → Neo4j (via SSH tunnel). Goal: observe expected vs.
unexpected behavior on diverse natural-language → Cypher queries, then fix what's broken.

Setup notes captured up front (themselves findings):
- `/ready` = **503**: staging has **no server-side cost bounds** (`db.transaction.timeout`,
  `db.memory.transaction.max` unset). The gateway runs the **development** profile, so it logs an
  error and starts anyway. Production profile would refuse to start. → **B7**.
- Schema is richer than a follow-only graph: `User, Post, Tag, File` nodes; `FOLLOWS, AUTHORED,
  TAGGED(label), REPLIED, REPOSTED, BOOKMARKED, MENTIONED, MUTED` edges. The Twitter-style
  structural questions (bridges, reply-vs-repost, reach, mutuality) map onto these.

## Query matrix

| # | NL question (adapted to Pubky) | Result | Notes |
|---|---|---|---|
| Q01 | most-used tags | OK, 3.1s | `pubky` 1653, `welcome` 1043, `bitcoin` 975. Values are proper JSON ints. |
| Q02 | most-followed users | OK, 2.5s | Big Bad John 241, Pav **149** | grouping by `u.name` (see B9) |
| Q03 | reach via `FOLLOWS*` | OK, 2.5s | `*`→`*1..5` silently; reach=337 = "within 5 hops" (B8) |
| Q04 | follow distance (both bound) `shortestPath` | OK, 2.5s | 2 rows for 1 pair → duplicate-named nodes (B9) |
| Q05 | follow distance to **unbound** end `shortestPath` | **500 INTERNAL** | B3 — driver/DB edge |
| Q06 | most-reposted → authors | OK, 2.5s | works |
| Q07 | reply-heavy authors (conflict proxy) | OK, 2.3s | Big Bad John 968 replies; emoji names fine |
| Q08 | mutual follows | OK, 2.7s | `a.id < b.id` dedup works |
| Q09 | count all 1–5-hop follow paths (cost probe) | **TIMEOUT**, was 20s | B2 |
| Q10 | write attempt (`SET`) | REJECTED 0.01s | sanitizer blocks, never hits DB ✓ |
| Q11 | `CALL db.labels()` | REJECTED 0.01s | reason "mutating clause" — misleading (B6) |
| Q12 | quantified path `(){1,3}` (residual probe) | OK, 6.6s, 268k paths | B5 — accepted, unbounded cost |
| Q13 | author's posts by kind ($id param) | OK, 2.3s | param binding works ✓ |
| Q14 | posts `LIMIT 200` | OK, 25 rows, `truncated=true` | B4 — default budget overrides query LIMIT |
| Q15 | trailing `// comment` | REJECTED 0.01s | comment blocked ✓ |

Probes for the error classifier (raw Neo4j codes confirmed via the python driver):
- `1/0` → `Neo.ClientError.Statement.ArithmeticError` (a **client** statement error)
- `'a'+1+{}` → `Neo.ClientError.Statement.SyntaxError`; undefined var → same
- unbound `shortestPath`+ORDER BY → `Neo.DatabaseError.Statement.ExecutionFailed`
  (*"shortest path … start and end nodes are the same … re-write the query …"*)

## Findings

### B1 — Runtime statement errors were misclassified as 500 — **FIXED**
A query that compiles but fails **at runtime** (e.g. `1/0`) returned `INTERNAL_ERROR` (500,
"retry") instead of a 400. Root cause: **neo4rs 0.8 `RowStream::next` does not match a mid-stream
`BoltResponse::Failure`** — it falls into `Err(unexpected(msg, "PULL"))`, surfacing as
`Error::UnexpectedMessage` (not `Error::Neo4j`), so `from_neo4rs` never saw the embedded
`Neo.ClientError.Statement.*` code. Compile-time errors (caught at `execute()`) classified fine;
runtime ones slipped through. Fix: also recover the statement code from the `UnexpectedMessage`
text (stable under the pinned neo4rs). **Verified live: `1/0` now → `QUERY_SYNTAX_ERROR` (400).**

### B2 — Timed-out queries took ~20s, not ~10s — **FIXED**
On a query timeout the post-timeout **rollback was given the full 10s query budget** on top of the
10s query, so the client waited ~20s for a "10s timeout" (log: `transaction rollback timed out`).
Rollback is cleanup, not the query. Fix: cap it at a short fixed `ROLLBACK_TIMEOUT` (2s); if it
can't finish, drop the connection (the pool resets it, and closing cancels the txn).
**Verified live: the expensive query now times out at 12s (10s + 2s) instead of 20s.**

### B3 — unbound `shortestPath` can 500 — **DOCUMENTED (driver/DB edge, not our logic)**
`shortestPath((a)-[*..5]->(b:User))` with an unbound `b` (and `ORDER BY length DESC`) raised
`Neo.DatabaseError.Statement.ExecutionFailed` ("start and end nodes are the same"). It's a Neo4j
**DatabaseError** (server-side), so the gateway correctly returns 500. Separately, neo4rs surfaces
it as `UnexpectedMessage` and may leave the connection failed; the pool self-heals (deadpool
`recycle`→`reset` discards it on next checkout — confirmed earlier). The python driver handles the
same query, so the gateway 500 is a neo4rs/Neo4j interaction, not gateway logic. Agent guidance:
bind both endpoints, or rewrite per Neo4j's message. *(Worth a one-line note in `examples.md`.)*

### B4 — default row budget (25) overrides a larger in-query `LIMIT` — **DOCUMENTED (by design)**
`… LIMIT 200` with no `--limit` returned 25 rows + `truncated=true`, because the row budget is
`requested.unwrap_or(DEFAULT_LIMIT=25).min(MAX_RESULT_ROWS=100)` and ignores the query's own LIMIT.
Intended (the gateway budget is the authority) and documented, but a real authoring gotcha: an
agent must pass `--limit` (≤100) to raise it. `truncated=true` signals it.

### B5 — quantified path patterns accepted, cost-unbounded — **CONFIRMED RESIDUAL**
`((x)-[:FOLLOWS]->(y)){1,3}` ran (268k paths, 6.6s). The path transform only bounds legacy `*`
ranges; QPP depth is unbounded by the sanitizer. This is the documented accepted-residual risk —
now empirically confirmed reachable, and compounded by **B7** (no server-side bound on staging),
leaving only the 12s client timeout as the backstop.

### B6 — `CALL` rejected as "mutating clause" — **RESOLVED**
`CALL db.labels()` was rejected with "query contains a mutating clause" and a hint to remove
CREATE/MERGE/SET, because CALL sat in `DENY_MUTATION`. Originally deferred as cosmetic. That
assessment was wrong: the hint is not merely imprecise, it is actionable in the wrong direction. Six
clean-context agent runs showed the failure mode, an agent re-reading its own query for a write
clause that does not exist and looping. Fixed with a `RejectReason::CallClause` variant and a
`DENY_CALL` table, still chained into `denied_keywords()` so the property test and fuzz oracle keep
covering it.

### B7 — staging has no server-side cost bounds — **OPS**
`/ready`=503 and a startup error: `db.transaction.timeout` / `db.memory.transaction.max` unset.
Dev profile starts anyway. So on staging the only cost bound is the client timeout. Production
profile would fail closed. Either set the bounds on the staging Neo4j or accept client-only bounds.

### B8 — path-bounding is silent — **OBSERVATION**
`FOLLOWS*` → `*1..5` rewrite is invisible in the response; `reach=337` means "within 5 hops," which
an agent could read as total reach. Considered surfacing a "query was bounded" hint; low priority.

### B9 — `name` is not unique — **OBSERVATION (authoring gotcha)**
Grouping by `u.name` merges distinct users (Pav showed 149 vs 146 when grouped by node); a
name-keyed `shortestPath` returns one row per duplicate-named pair. The schema marks `id` unique,
`name` not. Agents should key on `id`. *(Could note in `examples.md`.)*

## Positive confirmations (guardrails behave)
Writes (`SET`) and admin/procedure (`CALL`) and comments rejected in 0.01s (never reach the DB);
parameters bind natively ($id worked); `truncated` flagged; legacy variable-length paths bounded;
UTF-8/emoji round-trip in results; integer fidelity preserved (counts are JSON ints, not strings);
aggregations, multi-hop traversals, reciprocal patterns, and bound `shortestPath` all correct.

## This round's code changes
- **B1** `crates/nexus-scout/src/error.rs` — `from_neo4rs` recovers the statement code from a
  wrapped `UnexpectedMessage`; unit test `mid_stream_runtime_failure_classifies_as_query_error_not_internal`.
- **B2** `crates/nexus-scout/src/executor/mod.rs` — `ROLLBACK_TIMEOUT` (2s); `rollback_quietly`
  no longer takes the query budget.

Deferred/documented: B3, B4, B5, B6, B7, B8, B9.
