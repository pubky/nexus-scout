# nexus-scout architecture

nexus-scout is a read-only Cypher query gateway. It is a four-crate workspace:

- **`cypher-guard`** - a pure, dependency-light sanitizer. Given untrusted Cypher it returns a
  `SanitizedQuery` proof token or a `SanitizeError`. No I/O, no database driver, no async runtime, so
  it is unit-, property-, and fuzz-testable in isolation and reusable by any read-only Cypher
  consumer.
- **`nexus-scout-types`** - the shared wire contract: the request/response DTOs and the single
  `ErrorCode → HTTP status` / `ErrorCode → exit code` maps, depended on by both the server and the
  client so the contract is compiler-enforced rather than duplicated.
- **`nexus-scout`** - the gateway server. It owns configuration, the Neo4j executor, the curated
  schema, and the HTTP and MCP transports. The `Scout` facade is the single shared core every
  transport calls, so they cannot diverge in validation behavior.
- **`nexus-scout-cli`** - the `scout` client: a thin HTTP client of the gateway holding no Neo4j
  credentials (it depends only on the types crate, clap, serde_json, and ureq).

## Request flow

The `scout` CLI and HTTP agents reach the gateway over the public HTTP API; MCP-native agents over
stdio. All three terminate at the same `Scout` core.

```
agent ──HTTP / MCP──▶ Scout ──▶ cypher_guard::Sanitizer::sanitize ──▶ SanitizedQuery
                       │                                                  │
                       ├──▶ params::check_params (DoS bounds)             │
                       └──▶ Executor::execute(&SanitizedQuery, params) ◀──┘
                                  │  start_txn ▸ bind params natively ▸ stream rows
                                  │  ▸ tokio timeout ▸ shape (row/byte cap) ▸ bolt_to_json
                                  ▼
                            QueryResponse { results, count, truncated }
```

The executor accepts only a `&SanitizedQuery`, whose only constructor is crate-private to
`cypher-guard`. Unvalidated Cypher therefore cannot reach Neo4j - a compile-time guarantee, not a
convention.

## Module map (`nexus-scout`)

| Module | Responsibility |
|--------|----------------|
| `lib.rs` (`Scout`) | The shared facade: sanitize → bound params → execute; schema; server-bounds check. |
| `config.rs` | `Config` built through one populator (`ConfigBuilder`); `HttpLimits`, `Profile`, `Secret`. |
| `error.rs` | Canonical `Error` struct; `ErrorCode` re-exported from `nexus-scout-types`; `from_neo4rs`. |
| `response.rs` | Re-exports the wire types from `nexus-scout-types`. |
| `params.rs` | Parameter count/byte/depth bounds. |
| `convert.rs` | `bolt_to_json` (total over all Bolt variants) and `json_to_bolt`. |
| `executor/mod.rs` | Read transaction, dual timeout, native binding; server-bounds gate; slow-query log. |
| `executor/shape.rs` | Pure row/byte-cap shaping (DB-free unit-test target). |
| `schema.rs` | The curated `get_schema` graph schema, sourced from `schema.golden.json`. |
| `http.rs` | The public Axum HTTP server: routes, admission/DoS layers, readiness gate (feature `http`). |
| `server.rs` | MCP server (feature `mcp`). |
| `main.rs` | The serve-only binary: `serve --transport http\|stdio` dispatch. |

## Decisions (ADRs)

| # | Decision |
|---|----------|
| [0001](adr/0001-tokenizer-sanitizer.md) | Sanitizer is a tokenizer + allow/deny classifier, not a scanner or a full parser. |
| [0002](adr/0002-defense-in-depth.md) | Defense in depth: sanitizer + read-only DB user (Enterprise-only); honest scope. |
| [0003](adr/0003-two-crate-workspace.md) | A pure `cypher-guard` crate, separate from the gateway. |
| [0004](adr/0004-sanitized-query-typestate.md) | `SanitizedQuery` sealed proof token. |
| [0005](adr/0005-executor-transaction-timeout.md) | Explicit read transaction; client-liveness vs server-resource timeout split. |
| [0006](adr/0006-curated-schema.md) | `get_schema` is a curated golden artifact, not introspection or type-derivation. |
| [0007](adr/0007-error-model.md) | Error-as-struct with a private kind; serialize-only `ErrorCode` DTO. |
| [0008](adr/0008-defer-nl-layer.md) | Defer the natural-language → Cypher layer. |
| [0009](adr/0009-http-service-transport.md) | Public HTTP service transport; shared types crate; `scout` client; honest DoS scope. |

## Security

[`SECURITY_MATRIX.md`](SECURITY_MATRIX.md) is the version-pinned per-construct sanitizer policy
(allow / deny / rewrite for every Cypher 5 clause), the accepted residual risks, and the re-audit
checklist for Neo4j version bumps. Because the decided deployment is Neo4j Community (no RBAC), the
sanitizer is the sole write guard, so its coverage is enumerated and tested rather than assumed.
