# ADR-0003: A pure `cypher-guard` crate, separate from the gateway

**Status:** Accepted (2026-06-01)

> **Update:** the workspace later split out `nexus-scout-types` (the shared wire
> contract) and `nexus-scout-cli` (the client), so it is now **four** crates. The
> core decision - a pure, dependency-light `cypher-guard` separate from the gateway -
> stands; read "two crates" below as the original split.

## Context

The sanitizer is the security-critical heart of the project and carries the heaviest test burden
(adversarial corpus, property tests, fuzzing). The executor, by contrast, needs `neo4rs`, `tokio`,
and a live database.

## Decision

Split the project into two crates: `cypher-guard` (sanitizer; depends only on `std` and
`unicode-normalization`) and `nexus-scout` (everything else). `nexus-common` is a documentation
reference for the curated schema, not a compile dependency.

## Consequences

- ✅ The sanitizer compiles, fuzzes, and property-tests with a tiny dependency tree and no database.
- ✅ It is structurally impossible for the sanitizer to reach for I/O or a driver.
- ✅ `cypher-guard` is reusable by any other read-only Cypher consumer in the ecosystem.
- ✅ Dropping the `nexus-common` compile dependency keeps both crates buildable out of the box
  (no path/git dependency on a heavy, private workspace).
- ⚠️ The two crates are released in lockstep on their shared public dependencies (`serde_json`); a
  major bump of those is a breaking release of both.

## Alternatives considered

- **Single crate (lib + bin)**: simpler, but drags `neo4rs`/`tokio` into the sanitizer's fuzz/test
  builds and forfeits reuse. Rejected.
