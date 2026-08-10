# ADR-0009: Public HTTP service transport

**Status:** Accepted (2026-06-04)

## Context

The deployment goal is a hosted gateway that holds the Neo4j connection and executes agents' Cypher
over the network, so third parties get an API endpoint and **never** the database connection string.
The MVP shipped only a CLI and a stdio MCP server, both of which connect directly to Neo4j. A network
transport was needed, and the team decided it should be **public and unauthenticated** to start: the
graph is public-read, so read exposure is acceptable. The open questions were write safety and
availability under anonymous load.

## Decision

Add an Axum HTTP server as an additive `http` feature on `nexus-scout`, exposing `POST /v1/query`,
`GET /v1/schema`, `GET /`, `GET /llms.txt`, `GET /health`, `GET /ready`, and `GET /metrics` over the
shared `Scout` core, so
the sanitizer enforces read-only at the HTTP boundary exactly as on the CLI/MCP paths. The CLI becomes
a separate, credential-free `scout` client crate, and the wire contract (DTOs + the `code → HTTP
status` / `code → exit code` maps) moves to a shared `nexus-scout-types` crate so client and server
cannot drift.

For a public, unauthenticated v1:

- **Write safety** rests on the sanitizer (the sole write guard on Community), proven end-to-end by an
  integration test that POSTs every mutation and asserts `400` plus an unchanged graph.
- **Aggregate load** is bounded by a request body cap (`413`), a global in-flight concurrency cap and
  a QPS token bucket that **shed** excess as `429` (rather than queueing), a whole-request timeout,
  and a per-IP rate limit at the Caddy reverse proxy.
- **Per-query cost** is bounded by the **server-side Neo4j transaction timeout/memory**, which the
  gateway verifies at readiness (`/ready` → `503` until set) and, under
  `NEXUS_SCOUT_PROFILE=production`, **refuses to start** without.
- **Topology**: TLS terminates at Caddy; the gateway binds plain HTTP (default loopback) and is never
  publicly published; Neo4j is private. Credentials live only in the service.

## Consequences

- ✅ One sanitizer-enforced core across CLI, MCP, and HTTP; the write guarantee is tested at the new
  boundary.
- ✅ Neo4j credentials never leave the box; the `scout` client links neither neo4rs nor axum.
- ✅ The reserved `RATE_LIMITED` / `429` code is now produced (by the load-shed path), not dead.
- ❌ A public, unauthenticated endpoint is exposed. Mitigated by read-only enforcement, the bounds
  above, and the nightly-cloned replica deployment ([ADR-0002](0002-defense-in-depth.md)).
- ⚠️ **Honest scope:** v1 bounds writes and per-request cost and rate-limits aggregate load, but does
  **not prevent** bulk reads of a public-read graph, and a sufficiently distributed flood can still
  degrade availability. The single global concurrency limit means one source can, absent the Caddy
  per-IP limit, crowd others. Authentication is a deferred follow-up (a top-of-stack middleware).
- ⚠️ The server-side cost bounds are external configuration; the production-profile startup gate makes
  them a machine-checked precondition rather than a runbook note.
- ⚠️ Removing the `query`/`schema` subcommands from the `nexus-scout` binary (now serve-only) and the
  `cli` module from the library is a pre-1.0 breaking change to that surface.

## Alternatives considered

- **Authenticated v1** (API keys / bearer): deferred. There is no identity system yet, and the graph
  is public-read, so read auth buys little for v1; write safety does not depend on it.
- **Per-IP limiting inside the gateway**: needs trusted `X-Forwarded-For` (a spoofing footgun behind a
  proxy). Doing it at Caddy, which sees the real client IP, is simpler and safer.
- **A standalone HTTP-server crate**: the transport shares `Scout`, `Config`, and the wire types, so a
  feature-gated module (mirroring the `mcp` one) is less duplication than a separate crate.
- **`reqwest` for the client**: heavier (pulls a hyper/tower client stack); a one-shot CLI needs only a
  blocking `ureq` over the rustls stack already vendored.

## Notes

Cross-references: [ADR-0005](0005-executor-transaction-timeout.md) (client-liveness vs server-resource
timeout) and [ADR-0007](0007-error-model.md) (the reserved `RateLimited` code). Operations:
[`docs/deployment.md`](../deployment.md).
