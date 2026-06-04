> This project is mostly vibecoded.

# nexus-scout

A read-only Cypher query gateway between AI agents and the Pubky social graph (Neo4j).

An agent sends a Cypher query; nexus-scout validates it is read-only, executes it against Neo4j
under tight guardrails, and returns structured JSON. It is exposed both as a CLI (for shell agents
like Claude Code) and as a Model Context Protocol server (for MCP-native agents like Jeb).

## What it is / is not

- **Is**: a tiny, stateless sanitizer + executor. The calling agent writes its own Cypher (it has
  its own LLM); nexus-scout just validates and runs it.
- **Is not**: a write path (no mutations, ever), an auth gateway, or a replacement for the Nexus
  REST API.

## Workspace

| Crate | Role |
|-------|------|
| [`cypher-guard`](crates/cypher-guard) | Pure, reusable, read-only-Cypher sanitizer (no I/O, no driver). The security core. |
| [`nexus-scout`](crates/nexus-scout) | The gateway: executor, schema, config, CLI, and MCP server. |

## CLI

```sh
# Learn the graph structure first (no database needed).
nexus-scout schema | jq .nodes

# Run a read-only query.
nexus-scout query "MATCH (u:User)<-[f:FOLLOWS]-() RETURN u.name, count(f) AS followers ORDER BY followers DESC"

# Typed parameters (numbers/arrays/objects) via --params-json; string params via --param.
nexus-scout query --params-json '{"since": 1709251200000}' \
  "MATCH (u:User)-[t:TAGGED]->(p) WHERE t.indexed_at > \$since RETURN t.label, count(p) AS c"

# Run as an MCP server over stdio.
nexus-scout serve --transport stdio
```

Output is JSON on **stdout** (both success and error envelopes, so `| jq` always works); logs go to
**stderr**. Exit codes: `0` ok, `1` internal, `2` rejected, `3` timeout.

## Guardrails

| Guardrail | Default | Purpose |
|-----------|---------|---------|
| Read-only validation | always | Blocks every mutation/admin clause and namespaced procedure call. |
| Default row limit | 25 | Applied when the caller requests none. |
| Max result rows | 100 | Caps rows *returned* (not server-side work), regardless of any query `LIMIT`. |
| Max result bytes | 1 MiB | Hard ceiling on serialized response size; a row that would exceed it is dropped (flagged `truncated`). |
| Variable-length paths | `*1..5` | Unbounded/over-deep paths are rewritten, not rejected. |
| Query timeout | 10 s | Client liveness bound. |
| Param count/bytes/depth | 32 / 8 KiB / 8 | Denial-of-service bounds on parameters. |

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

**Honest scope**: "zero modification of the primary" holds by isolation; "zero modification of the
replica" rests on sanitizer correctness. "Zero data **exfiltration**" is *not* achievable for a
public-read graph; it is bounded (rows, bytes, timeout), not prevented.

## Development

```sh
cargo nextest run --workspace          # DB-free: sanitizer corpus, properties, contract, CLI
cargo clippy --workspace --all-targets --all-features -- -D warnings
cargo fmt --check

# Integration tests need a live Neo4j (see docker/docker-compose.yml):
docker compose -f docker/docker-compose.yml up -d neo4j
cargo nextest run -p nexus-scout --features integration
```

See [`docs/architecture.md`](docs/architecture.md) for the design and the ADR index.

## License

MIT
