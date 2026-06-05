> This project is mostly vibecoded.

# nexus-scout

A read-only Cypher query gateway between AI agents and the Pubky social graph (Neo4j).

An agent sends a Cypher query; nexus-scout validates it is read-only, executes it against Neo4j
under tight guardrails, and returns structured JSON. It runs as a hosted, public **HTTP service** -
agents POST Cypher to its API and get JSON back, never touching the Neo4j connection string - with a
thin `scout` CLI client and a Model Context Protocol server (stdio) over the same core.

## What it is / is not

- **Is**: a tiny, stateless sanitizer + executor. The calling agent writes its own Cypher (it has
  its own LLM); nexus-scout just validates and runs it.
- **Is not**: a write path (no mutations, ever), an auth gateway, or a replacement for the Nexus
  REST API.

## Workspace

| Crate | Role |
|-------|------|
| [`cypher-guard`](crates/cypher-guard) | Pure, reusable, read-only-Cypher sanitizer (no I/O, no driver). The security core. |
| [`nexus-scout-types`](crates/nexus-scout-types) | The shared wire contract: request/response DTOs + the code→status / code→exit maps. |
| [`nexus-scout`](crates/nexus-scout) | The gateway server: sanitizer + executor + schema + the HTTP and MCP transports. |
| [`nexus-scout-cli`](crates/nexus-scout-cli) | The `scout` HTTP client (holds no Neo4j credentials). |

## Running the gateway

```sh
# Serve the public HTTP API (default). Binds 127.0.0.1:8080; put a TLS-terminating
# reverse proxy in front for public traffic (see docker/docker-compose.prod.yml).
nexus-scout serve --transport http

# Or serve MCP over stdio for MCP-native agents.
nexus-scout serve --transport stdio
```

## HTTP API

| Method + path | Purpose |
|---------------|---------|
| `POST /v1/query` | Run a read-only query: body `{ "cypher": ..., "params"?: {…}, "limit"?: n }`. |
| `GET /v1/schema` | The curated graph schema. |
| `GET /health` | Liveness (process up). |
| `GET /ready` | Readiness: Neo4j reachable **and** the server-side cost bounds are set, else `503`. |
| `GET /metrics` | In-flight gauge + request/shed counters (plain text). |

```sh
curl -s localhost:8080/v1/schema | jq .nodes
curl -s -XPOST localhost:8080/v1/query -H 'content-type: application/json' \
  -d '{"cypher":"MATCH (u:User) RETURN u.name AS name LIMIT 5"}' | jq
```

Success and error share one envelope shape: `{ results, count, truncated }` or
`{ error, message, hint }`. Codes map to status: `QUERY_REJECTED`/`QUERY_SYNTAX_ERROR` → 400,
`QUERY_TIMEOUT` → 504, `RATE_LIMITED` → 429, `INTERNAL_ERROR` → 500; an oversized body → 413.

**Result row shape.** Each row in `results` is a JSON object keyed by your `RETURN` columns. A few
keys are reserved: a returned **node** expands to its properties plus `_id` and `_labels`; a
**relationship** adds `_id`, `_type`, `_start`, `_end`; a **single-column** row wraps its bare value
under `value`; an uncommon temporal/spatial value becomes `{ "_unconvertible": "<tag>" }`; and a row
that fails conversion becomes `{ "_row_error": "..." }`. Column order is not preserved (keys are
sorted). These keys are pinned by tests in `crates/nexus-scout/src/convert.rs`.

## `scout` CLI client

```sh
scout schema | jq .nodes
scout query "MATCH (u:User)<-[f:FOLLOWS]-() RETURN u.name, count(f) AS followers ORDER BY followers DESC"

# Typed parameters via --params-json; string params via --param.
scout query --params-json '{"since": 1709251200000}' \
  "MATCH (u:User)-[t:TAGGED]->(p) WHERE t.indexed_at > \$since RETURN t.label, count(p) AS c"
```

`scout` is a thin client of the HTTP API (target it via `--server-url` or `NEXUS_SCOUT_URL`, default
`http://localhost:8080`); it holds no Neo4j credentials. JSON is always on **stdout** (so `| jq`
works); exit codes: `0` ok, `1` internal/transient, `2` rejected, `3` timeout.

## Guardrails

| Guardrail | Default | Purpose |
|-----------|---------|---------|
| Read-only validation | always | Blocks every mutation/admin clause and namespaced procedure call. |
| Default row limit | 25 | Applied when the caller requests none. |
| Max result rows | 100 | Caps rows *returned* (not server-side work), regardless of any query `LIMIT`. |
| Max result bytes | 1 MiB | Caps the summed returned-row payload bytes; a row that would exceed it is dropped (flagged `truncated`). The response envelope adds a small fixed overhead on top. |
| Variable-length paths | `*1..5` | Unbounded/over-deep paths are rewritten, not rejected. |
| Query timeout | 10 s | Client liveness bound. |
| Param count/bytes/depth | 32 / 8 KiB / 8 | Denial-of-service bounds on parameters. |
| Request body size (HTTP) | 64 KiB | Oversized request bodies rejected with `413`. |
| In-flight concurrency (HTTP) | 64 | Excess `/v1/query` requests shed with `429` (not queued). |
| Request rate (HTTP) | 50 rps | Sustained `/v1/query` over the cap shed with `429`. |
| Whole-request timeout (HTTP) | 30 s | Coarse backstop above the 10 s query timeout. |

All are configurable via environment variables (see [`.env.example`](.env.example)).

The row and byte caps bound what is *returned*; they do not limit the work Neo4j does to produce a
result (a broad scan, sort, or aggregation can be expensive before the first row). Server-side
execution cost is bounded by the Neo4j `db.transaction.timeout` and transaction memory limit
configured for the reader (see the setup scripts), not by this gateway; the 10 s client timeout only
bounds caller liveness.

Result columns are addressed **by name**; column order is not preserved (the driver yields an
unordered map, so nexus-scout sorts keys for deterministic output).

## Security model

Two independent layers plus a compile-time invariant:

1. **Sanitizer (layer 1)** - a tokenizer + allow/deny classifier rejects comments, semicolons, every
   mutation and administration clause, and any namespaced procedure/function call (`apoc.*`, `db.*`,
   `gds.*`), with Unicode hardening that forces every keyword-position byte to be ASCII. Unbounded
   paths are bounded. The `SanitizedQuery` it produces is a sealed proof token: the executor accepts
   nothing else, so unvalidated Cypher cannot reach Neo4j at compile time.
2. **Read-only database user (layer 2)** - a `DENY WRITE` reader role means even a sanitizer bug
   cannot modify data. **This requires Neo4j Enterprise**: Community edition has no role-based access
   control, so on Community the configured user can write and the sanitizer is the *sole* write
   guard. Provisioning is split by edition:
   [`neo4j_reader_setup_community.cypher`](scripts/neo4j_reader_setup_community.cypher) (creates the
   user only) and [`neo4j_reader_setup_enterprise.cypher`](scripts/neo4j_reader_setup_enterprise.cypher)
   (adds the `DENY WRITE` reader role). See [ADR-0002](docs/adr/0002-defense-in-depth.md).

**Deployment (decided):** this runs against **Neo4j Community** on a **read replica re-cloned
nightly**. Layer 2 is therefore unavailable; the replica gives physical isolation (writes never reach
the primary, intra-day corruption is wiped nightly) and the **sanitizer is the sole write guard**.
Because of that the sanitizer is treated as security-critical: an exhaustive deny list (classic
clauses plus GQL `INSERT`/`NODETACH`), an adversarial corpus, property tests, and continuous fuzzing
with a guardrail oracle. Its full per-construct policy, the accepted residual risks, and the
re-audit checklist for Neo4j version bumps are enumerated in
[`docs/SECURITY_MATRIX.md`](docs/SECURITY_MATRIX.md).

Parameters are bound natively (never interpolated), so parameter values are inert against injection.

**Public HTTP endpoint (v1).** The HTTP API is public and **unauthenticated**. It is read-only
(the sanitizer enforces this at the HTTP boundary exactly as on the CLI/MCP paths) and bounded both
per request and in aggregate: a body cap, an in-flight concurrency cap, a QPS shed, and an optional
(recommended) Caddy per-IP limit. Neo4j credentials live only in the service; clients never receive
them. The real bound
on a single expensive read is the **server-side Neo4j transaction timeout/memory**, which the gateway
verifies at readiness and refuses to start without under `NEXUS_SCOUT_PROFILE=production`. See
[ADR-0009](docs/adr/0009-http-service-transport.md) and [`docs/deployment.md`](docs/deployment.md).

**Honest scope**: "zero modification of the primary" holds by isolation; "zero modification of the
replica" rests on sanitizer correctness. "Zero data **exfiltration**" is *not* achievable for a
public-read graph; v1 bounds writes and per-request cost and rate-limits aggregate load, but does not
*prevent* bulk reads, and a sufficiently distributed flood can still degrade availability.
Authentication is a documented follow-up.

## Development

```sh
cargo nextest run --workspace          # DB-free: sanitizer corpus, properties, contract, CLI
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --check

# Integration tests need a live Neo4j (any --features integration / --all-features run
# requires it; the DB-free command above does not). See docker/docker-compose.yml:
docker compose -f docker/docker-compose.yml up -d neo4j
cargo nextest run -p nexus-scout --features integration,http
```

See [`docs/architecture.md`](docs/architecture.md) for the design and the ADR index, and
[`docs/deployment.md`](docs/deployment.md) for running the hosted service.

## License

MIT
