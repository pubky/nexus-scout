# ADR-0002: Defense in depth, and the Neo4j edition caveat

**Status:** Accepted (2026-06-01)

## Context

The success criterion is "zero data modification possible through any code path." A sanitizer is
software and can have bugs, so a single layer is not enough to make that claim credibly.

## Decision

Run two independent layers where the platform allows, and a physical-isolation layer where it does
not:

1. **Layer 1 - the sanitizer** ([ADR-0001](0001-tokenizer-sanitizer.md)): blocks every mutation and
   administration clause and every namespaced procedure/function call before the query is executed.
2. **Layer 2 - a read-only database user**: connect as a `reader`-role user with `DENY WRITE`, so
   even if layer 1 had a bug the connection cannot modify data. Provisioned by
   `scripts/neo4j_reader_setup_enterprise.cypher`; `scripts/neo4j_reader_setup_community.cypher`
   creates only the user (no RBAC to grant). **Enterprise only** (see Consequences).
3. **Deployment isolation**: nexus-scout runs against a **read replica that is re-cloned nightly**,
   not the primary. Writes therefore cannot reach the primary at all, and any corruption of the
   replica is wiped within a day.

Additionally, parameters are bound natively (never interpolated), and we never call `nexus-common`'s
`bolt_to_cypher_literal` / `populate_cypher`, which interpolate values into the query string and
carry a documented injection hazard.

## Consequences

- ✅ On Neo4j **Enterprise**, layer 2 is a parser-independent guarantee. The integration
  `reader_role_write_policy_matches_edition` test sends raw mutations through the reader connection
  and asserts each is refused with the node count unchanged.
- ❌ **Layer 2 requires Enterprise.** `GRANT ROLE` / `DENY` are Enterprise-only; Neo4j **Community**
  (which the current Nexus stack runs) has no role-based access control, so the configured user can
  write and **the sanitizer is the sole write guard**. This was confirmed empirically against
  `neo4j:5.26-community`. On Community, the only server-side protection available is the transaction
  timeout / memory limit (a resource bound, not a write bound).
- ⚠️ **Deployment reality (decided):** nexus-scout runs against **Neo4j Community** on a
  **nightly-cloned read replica**. So layer 2 is unavailable and the **sanitizer is the sole write
  guard**; the replica provides physical isolation (no path to the primary; intra-day corruption is
  wiped nightly) but does not by itself prevent writes to the replica. Consequently the sanitizer is
  treated as security-critical: hand-audited deny list (including GQL `INSERT`/`NODETACH`),
  adversarial corpus, property tests, and continuous fuzzing with a guardrail oracle.
- ⚠️ **Honest scope:** "zero modification of the primary" holds by isolation. "Zero modification of
  the replica" rests entirely on the sanitizer's correctness. "Zero exfiltration" is not achievable
  for a public-read graph and is reframed as rate/row/byte/timeout bounding.

## Alternatives considered

- **Sanitizer only**: insufficient for the "any code path" claim where Enterprise is available; still
  the de-facto situation on Community, which is why the sanitizer is treated as security-critical
  (fuzzed, property-tested, adversarial corpus) rather than best-effort.
- **DB-only (no sanitizer)**: cannot enforce resource guardrails (path depth, row caps) and is
  impossible on Community; also gives agents opaque permission errors instead of actionable hints.
