# nexus-scout architecture

nexus-scout is a read-only Cypher query gateway. It is a two-crate workspace:

- **`cypher-guard`** - a pure, dependency-light sanitizer. Given untrusted Cypher it returns a
  `SanitizedQuery` proof token or a `SanitizeError`. No I/O, no database driver, no async runtime, so
  it is unit-, property-, and fuzz-testable in isolation and reusable by any read-only Cypher
  consumer.
- **`nexus-scout`** - the gateway. It owns configuration, the Neo4j executor, the curated schema, the
  wire types, the CLI, and the MCP server. The `Scout` facade is the single shared core that both the
  CLI and the MCP server call, so the two front-ends cannot diverge in validation behavior.

## Request flow

```
agent ──CLI / MCP──▶ Scout ──▶ cypher_guard::Sanitizer::sanitize ──▶ SanitizedQuery
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
| `lib.rs` (`Scout`) | The shared facade: sanitize → bound params → execute; expose schema. |
| `config.rs` | `Config` built through one populator (`ConfigBuilder`); `Secret` redaction. |
| `error.rs` | Canonical `Error` struct + serialize-only `ErrorCode` wire DTO; `from_neo4rs`. |
| `response.rs` | `QueryResponse` / `ErrorResponse` / `Response` wire types. |
| `params.rs` | Parameter count/byte/depth bounds. |
| `convert.rs` | `bolt_to_json` (total over all Bolt variants) and `json_to_bolt`. |
| `executor/mod.rs` | Read transaction, dual timeout, native binding. |
| `executor/shape.rs` | Pure row/byte-cap shaping (DB-free unit-test target). |
| `schema.rs` | The curated `get_schema` graph schema, sourced from `schema.golden.json`. |
| `cli.rs` / `main.rs` | clap CLI and thin dispatch. |
| `server.rs` | MCP server (feature `mcp`). |

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
