# ADR-0004: `SanitizedQuery` is a sealed proof token

**Status:** Accepted (2026-06-01)

## Context

"No unvalidated Cypher reaches Neo4j" should be a property the compiler enforces, not a convention a
reviewer has to police.

## Decision

`cypher_guard::SanitizedQuery` wraps the validated query string with a private field and a
crate-private constructor (`pub(crate) fn new`). The only public way to obtain one is
`Sanitizer::sanitize`, which runs the full pipeline. The executor's `execute` accepts only a
`&SanitizedQuery`.

The type is `#[non_exhaustive]`, has no `Default`, no public mutator, and - because `cypher-guard`
has no `serde` dependency - cannot implement `Deserialize`. A `seal.rs` test asserts these.

## Consequences

- ✅ Forging an executable query without going through validation does not compile.
- ✅ The proof obligation is crisp: the token attests exactly "this string passed read-only
  validation" and nothing else (parameters live separately, in the gateway).
- ⚠️ Minting the token from a test requires going through `sanitize`; the integration refusal matrix
  deliberately bypasses the gateway by talking to `neo4rs` directly instead of forging a token, which
  is both safer and a more honest test of layer 2.

## Alternatives considered

- **A `test-util` `from_trusted` bypass constructor**: rejected. Cargo feature unification could leak
  it into a release build, and the refusal matrix does not need it.
- **Passing a plain `&str` to the executor**: loses the compile-time guarantee. Rejected.
