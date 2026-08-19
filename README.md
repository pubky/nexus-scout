> This project is mostly vibecoded. Verify before you trust; the tests and
> [`docs/SECURITY_MATRIX.md`](docs/SECURITY_MATRIX.md) are the ground truth.

# nexus-scout

A read-only Cypher gateway between AI agents and the Pubky social graph (Neo4j).

An agent sends Cypher; nexus-scout validates it is read-only, runs it under tight guardrails, and
returns structured JSON. The calling agent writes its own queries (it has its own LLM); nexus-scout
is just a sanitizer plus executor. It is not a write path, not an auth gateway, and not a
replacement for the Nexus REST API.

## Try it

The public instance is `https://nexus-scout.pubky.app`. No account, no API key.

```sh
curl -s https://nexus-scout.pubky.app/v1/schema | jq .nodes
curl -s -XPOST https://nexus-scout.pubky.app/v1/query -H 'content-type: application/json' \
  -d '{"cypher":"MATCH (u:User) RETURN u.name AS name LIMIT 5"}' | jq
```

Agents should read `https://nexus-scout.pubky.app/llms.txt`: the full usage guide (endpoints,
result shapes, error codes, query patterns). [`examples.md`](examples.md) walks through real
question-to-Cypher sessions.

## HTTP API

| Method + path | Purpose |
|---------------|---------|
| `GET /` | Service descriptor: endpoints, an example request, the live limits. |
| `GET /llms.txt` | Usage guide for agents. |
| `POST /v1/query` | Run a read-only query: `{ "cypher": ..., "params"?: {…}, "limit"?: n }`. |
| `GET /v1/query` | Same operation via query string, for callers that cannot send a body. |
| `GET /v1/schema` | The curated graph schema, with example queries. |

The operational endpoints (`/health`, `/ready`, `/metrics`) are served on a separate listener
(`METRICS_ADDR`, default `127.0.0.1:9090`) so probes and metrics stay off the public surface; never
front that port with the reverse proxy.

Success and error share one envelope: `{ results, count, truncated, notes? }` or
`{ error, message, hint }`. `QUERY_REJECTED`/`QUERY_SYNTAX_ERROR` map to 400, `QUERY_TIMEOUT` to
504, `RATE_LIMITED` to 429, `INTERNAL_ERROR` to 500. Returned nodes and relationships are
properties-only JSON objects; `/llms.txt` documents the exact row shapes.

## `scout` CLI

```sh
cargo install --path crates/nexus-scout-cli

scout schema | jq .nodes
scout query "MATCH (u:User)<-[f:FOLLOWS]-() RETURN u.name, count(f) AS followers ORDER BY followers DESC"
```

A thin client of the HTTP API (default host is the public instance; override with `--server-url` or
`NEXUS_SCOUT_URL`). It holds no Neo4j credentials. JSON on stdout; exit codes `0` ok, `1`
internal/transient, `2` rejected, `3` timeout.

## Guardrails

| Guardrail | Default |
|-----------|---------|
| Read-only validation | always: every mutation/admin clause and namespaced procedure call is rejected |
| Result rows | 25 default, 100 hard ceiling |
| Result payload | 1 MiB |
| Variable-length paths | rewritten to `*1..5`, reported in `notes` |
| Query timeout | 10 s (client liveness; server cost is bounded by Neo4j's own transaction timeout/memory limits) |
| Params | 32 count / 8 KiB / depth 8 |
| HTTP | 64 KiB body, 64 in-flight, 50 rps, 30 s whole-request timeout |

All configurable via environment variables ([`.env.example`](.env.example)).

## Security model

The sanitizer is a tokenizer plus allow/deny classifier: it rejects comments, semicolons, every
mutation and administration clause, and any namespaced procedure/function call (`apoc.*`, `db.*`,
`gds.*`), with Unicode hardening. Its `SanitizedQuery` output is a sealed proof token the executor
requires, so unvalidated Cypher cannot reach Neo4j at compile time. Parameters are bound natively,
never interpolated.

The hosted deployment runs against Neo4j **Community** (no RBAC, so no `DENY WRITE` role behind
the sanitizer) on an isolated read replica re-cloned nightly. Writes can never reach the primary;
on the replica the sanitizer is the sole write guard, which is why it is treated as
security-critical: an exhaustive deny list, an adversarial corpus, property tests, and continuous
fuzzing with a guardrail oracle. Per-construct policy, residual risks, and the re-audit checklist
for Neo4j bumps live in [`docs/SECURITY_MATRIX.md`](docs/SECURITY_MATRIX.md). On Enterprise, a
`DENY WRITE` reader role adds a second layer
([`scripts/`](scripts/), [ADR-0002](docs/adr/0002-defense-in-depth.md)).

Honest scope: the endpoint is public and unauthenticated. It bounds writes and per-request cost and
sheds excess load, but the graph is public-read by design, so it does not prevent bulk reads, and a
distributed flood can still degrade availability. Authentication is a documented follow-up.

## Development

```sh
cargo nextest run --workspace          # DB-free: sanitizer corpus, properties, contract, CLI
cargo clippy --workspace --all-targets --all-features -- -D warnings

# Integration tests need a live Neo4j (any --features integration run does):
docker compose -f docker/docker-compose.yml up -d neo4j
cargo nextest run -p nexus-scout --features integration,http
```

Design and ADR index: [`docs/architecture.md`](docs/architecture.md). Running the hosted service:
[`docs/deployment.md`](docs/deployment.md). Open infrastructure items:
[`docs/ops-handoff.md`](docs/ops-handoff.md).

## License

MIT
