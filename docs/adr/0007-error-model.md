# ADR-0007: Error-as-struct with a private kind; serialize-only `ErrorCode` DTO

**Status:** Accepted (2026-06-01)

## Context

The Microsoft Rust guidelines (M-ERRORS-CANONICAL-STRUCTS) prefer a situation-specific error struct
with helper methods over a public kind enum, so callers are not coupled to every failure mode. The
spec, separately, mandates a fixed set of machine-readable error codes on the wire.

## Decision

Two distinct types:

- `Error` - a public struct with a **private** `Kind`, an optional source, and a captured
  `Backtrace`. Callers classify via `code()`, `is_rejected()`, `is_timeout()`, and read `hint()`;
  they never match a public variant.
- `ErrorCode` - a small `#[non_exhaustive]`, `SCREAMING_SNAKE_CASE` serialize-only enum. It is a wire
  data-transfer type fixed by the spec, not the error type.

`from_neo4rs` classifies driver errors via the typed `Neo4jErrorKind`, with one documented string
match (`code().starts_with("Neo.ClientError.Statement.Syntax")`) because the driver models no syntax
category. Hints have a single home (derived from the kind). `neo4rs` types never appear in a public
signature.

## Consequences

- ✅ Adding a new internal failure mode does not break callers.
- ✅ The wire contract stays exactly as the spec specifies; agents match on stable code strings.
- ⚠️ `ErrorCode::RateLimited` is reserved but never produced in the MVP (rate limiting is deferred);
  keeping the variant avoids a future wire-contract change.

## Alternatives considered

- **A single public `thiserror` enum**: exposes every variant to callers, the anti-pattern the
  guideline warns against. Rejected.
- **Only the wire enum, no struct**: loses the backtrace and source chain useful for diagnosing
  `INTERNAL_ERROR`. Rejected.
