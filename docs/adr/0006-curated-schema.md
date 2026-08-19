# ADR-0006: `get_schema` is a curated golden artifact

**Status:** Accepted (2026-06-01)

## Context

Agents call `get_schema` once to learn the graph, then write Cypher. The schema must be stable,
example-rich, and keep its deliberately asymmetric wire shape: node properties are objects
(`{type, description?, unique?}`) while relationship properties are bare type strings.

## Decision

Ship the schema as a checked-in golden JSON file (`docs/schema.golden.json`) that the `schema()`
function parses and serves; a contract test pins the served output to the golden and to that wire
shape, and asserts every example query passes the sanitizer.

## Consequences

- ✅ The wire shape (including the asymmetry and human descriptions/examples) is exactly
  controllable - none of which runtime introspection or type-derivation can express.
- ✅ Examples can never drift into something the gateway would reject (tested).
- ⚠️ The golden is hand-maintained against the on-graph model (`nexus-common`'s
  `models/{user,post,tag,file}`). An integration test that diffs labels/relationship types against a
  live `CALL db.labels()` / `db.relationshipTypes()` is the intended drift guard.

## Alternatives considered

- **Runtime introspection** (`CALL db.schema.*`): needs `CALL`, which the sanitizer denies, and
  cannot carry curated descriptions/examples. Rejected.
- **Derive from `nexus-common` model structs**: their serialization shape deliberately differs from
  the on-graph shape (e.g. `links` is a JSON-encoded string), so derivation would misrepresent the
  graph. Rejected.
