# Sanitizer security matrix

This is the auditable, version-pinned policy of the `cypher-guard` sanitizer: for
every relevant Cypher / GQL construct, what the sanitizer does and where that is
proven. It exists because on the decided deployment (Neo4j **Community**, no
role-based access control) the sanitizer is the **sole write guard**
([ADR-0002](adr/0002-defense-in-depth.md)), so "we think we cover everything"
is not good enough: the coverage has to be enumerated and tested.

## Audited against

| Thing | Version |
|-------|---------|
| Cypher language | Cypher 5 (Neo4j 5.x) |
| Neo4j server | `neo4j:5.26-community` (CI and local compose) |
| Driver | `neo4rs =0.8.0` |
| Policy source of truth | `crates/cypher-guard/src/rules.rs` (`DENY_MUTATION`, `DENY_ADMIN`, `READ_ENTRY`) |

Re-audit this table on every Neo4j minor-version bump (see the checklist at the
end). New write-capable or administration clauses are the principal forward-compat
risk for a deny-list guard. The audited image tag above is enforced by
`crates/cypher-guard/tests/version_pinning.rs`: it fails the build if the tag drifts
in the compose files or CI without this matrix being updated in lockstep, so a bump
cannot skip the re-audit.

## How the sanitizer decides (pipeline order)

`Sanitizer::sanitize` runs a fixed pipeline (`crates/cypher-guard/src/lib.rs`):

1. **Unicode normalize + validate** (`unicode.rs`): NFC, then reject any non-ASCII
   or disallowed control char *outside* string/backtick regions.
2. **Length** check against `max_query_length`.
3. **Lex** into tokens (`tokenizer.rs`); strings/backticks/comments are opaque.
4. **Classify** (`rules.rs`), in this order: comments/semicolons, then denied
   (mutation/admin) keywords, then namespaced calls, then the read-entry rule.
5. **Path-bound transform** (`transforms.rs`): rewrite, never reject.

A construct is **Allowed**, **Denied** (rejected), **Rewritten** (bounded), or
**N/A** (cannot occur as written). "Enforced by" names the rule; "Proven by"
names the test.

## Read constructs (allowed)

| Construct | Policy | Notes / proven by |
|-----------|--------|-------------------|
| `MATCH`, `OPTIONAL MATCH` | Allow | Read-entry. `accept_spec_queries.rs`, `adversarial.rs` ACCEPT |
| `WHERE`, `RETURN`, `WITH`, `UNWIND` | Allow | `WITH`/`UNWIND`/`RETURN` are also valid read entries |
| `ORDER BY`, `SKIP`, `LIMIT`, `DISTINCT` | Allow | Not in any deny table |
| `UNION` / `UNION ALL` | Allow | `adversarial.rs` ACCEPT (`... UNION MATCH ...`) |
| Operators (`AND OR NOT XOR IN IS STARTS ENDS CONTAINS` ...) | Allow | Bare words, not denied |
| Bare function calls (`count()`, `collect()`, `nodes()`, `shortestPath()`, `datetime()`, `toLower()`) | Allow | A bare `ident(` is never a namespaced call; `adversarial.rs` ACCEPT |
| Map projection `n{.name}`, `n{.*}` | Allow | `{.*}` is not a path range; `adversarial.rs` ACCEPT |
| Nested property access `n.a.b` | Allow | Dotted with no trailing `(` is property access, not a call |
| `EXISTS { ... }`, `COUNT { ... }`, `COLLECT { ... }` subqueries | Allow | Read-only; the leading word is not denied |
| `FINISH` | Allow if not leading | Read-only GQL clause; rejected as a *leading* clause (not a read entry) |

## Write constructs (denied)

All rejected wherever they appear, not only at statement start. Source:
`DENY_MUTATION` in `rules.rs`. Reason code: `Mutation`.

| Clause | Example | Proven by |
|--------|---------|-----------|
| `CREATE` | `CREATE (n) ...` | `adversarial.rs`, `every_denied_keyword_is_enforced`, integration `gateway_rejects_writes_*` |
| `MERGE` | `MERGE (n) ...` | same |
| `SET` | `MATCH (n) SET n.x = 1` | same |
| `DELETE`, `DETACH DELETE` | `MATCH (n) DETACH DELETE n` | same |
| `NODETACH DELETE` (GQL) | `MATCH (n) NODETACH DELETE n` | `adversarial.rs` |
| `REMOVE` | `MATCH (n) REMOVE n.x` | `adversarial.rs` |
| `DROP` | `DROP INDEX foo` | `adversarial.rs` |
| `INSERT` (GQL) | `INSERT (m:Evil) ...` | `adversarial.rs` |
| `FOREACH` | `FOREACH (x IN [1] \| SET x.y = 1)` | `adversarial.rs` |
| `LOAD` (`LOAD CSV`) | `LOAD CSV FROM 'file:///x' AS r ...` | `adversarial.rs` |
| `USING` (`USING PERIODIC COMMIT`, query hints) | `USING PERIODIC COMMIT 500 LOAD CSV ...` | `adversarial.rs`. NOTE: query hints `USING INDEX/SCAN/JOIN` are also rejected (safe over-rejection) |

## Procedure-call clause (denied)

Source: `DENY_CALL` in `rules.rs`. Reason code: `CallClause`.

`CALL` is denied on its own terms rather than as a write. A read-only
`CALL db.labels()` mutates nothing, so reporting it as a mutating clause sent
callers hunting for a write clause that was not there.

| Clause | Example | Proven by |
|--------|---------|-----------|
| `CALL` (procedure call *and* `CALL { }` subquery, incl. `IN TRANSACTIONS`) | `CALL db.labels() ...`, `MATCH (n) CALL { CREATE (m) }` | `adversarial.rs`. NOTE: read-only `CALL { }` subqueries are also rejected (safe over-rejection). A `CALL { }` wrapping a write reports `CallClause`, not `Mutation`: removing the `CALL` is the fix either way, so scan order does not pick the message |

## Administration / selector clauses (denied)

Source: `DENY_ADMIN` in `rules.rs`. Reason code: `AdminClause`. Every entry is
proven enforced by `every_denied_keyword_is_enforced`; individually-corpus'd
cases are noted.

| Clause | Covers | Corpus case |
|--------|--------|-------------|
| `USE` | graph/database selector (`USE system ...`) | `adversarial.rs` |
| `SHOW` | `SHOW USERS/DATABASES/INDEXES/CONSTRAINTS/PRIVILEGES/...` | `adversarial.rs` |
| `TERMINATE` | `TERMINATE TRANSACTION` | `adversarial.rs` |
| `START` | legacy/transaction start | `adversarial.rs` |
| `GRANT`, `DENY`, `REVOKE` | privilege administration | matrix test only |
| `ALTER`, `RENAME` | user/database/role administration | matrix test only |
| `ENABLE`, `DISABLE` | server administration | matrix test only |
| `PROFILE`, `EXPLAIN` | query-plan prefixes | `adversarial.rs` |

DDL such as `CREATE INDEX/CONSTRAINT/USER/ROLE/DATABASE` and
`DROP INDEX/CONSTRAINT/...` is caught by `CREATE` / `DROP` in `DENY_MUTATION`
(reason `Mutation`), not by `DENY_ADMIN`.

## Procedure / function namespaces (denied)

| Construct | Policy | Enforced by | Proven by |
|-----------|--------|-------------|-----------|
| Namespaced call `ns.fn(...)`, `ns.sub.fn(...)` (`apoc.*`, `db.*`, `dbms.*`, `gds.*`) | Deny (`NamespacedCall`) | `find_namespaced_call`: `Word ('.' Word)+ '('`, whitespace-insensitive across dots | `adversarial.rs` (incl. `apoc . convert . toJson`, newline-between-dots) |
| Backtick-quoted namespace segment (Neo4j resolves a backtick segment, so `apoc` + backtick-`cypher` + `.runFirstColumn(...)` is still a namespaced call) | Deny (`NamespacedCall`) | backtick idents count as name segments (`is_name_segment`) | `adversarial.rs` (the original backtick bypass this closes) |
| `CALL ns.proc(...)` | Deny (`CallClause`, via `CALL`) | `DENY_CALL` | `adversarial.rs` |

A *bare* `ident(` call (e.g. `count(`) is allowed: no bare Cypher function
mutates; mutation needs a clause or a namespaced procedure, both denied. An
unknown bare function fails closed at Neo4j as a syntax error.

## Structural / injection vectors

| Construct | Policy | Reason | Proven by |
|-----------|--------|--------|-----------|
| Line comment `//`, block comment `/* */` | Deny | `CommentInjection` | `adversarial.rs` (incl. `MAT/**/CH`, comment splice) |
| Unterminated string or block comment | Deny | `Unterminated` | `adversarial.rs` |
| Statement separator `;` | Deny | `Semicolon` | `adversarial.rs` (leading, trailing, mid-query) |
| Non-ASCII / invisible char outside strings (homoglyph, ZWSP/ZWJ, fullwidth, BOM, bidi, `U+2028/2029/0085`) | Deny | `NonAsciiKeyword` | `adversarial.rs` (Cyrillic, fullwidth, line-sep, ZWJ, BOM, bidi) |
| Query exceeding `max_query_length` | Deny | `TooLong` | unit coverage |
| Empty / whitespace-only query | Deny | `Empty` | `adversarial.rs` |
| Query not beginning with `MATCH/OPTIONAL MATCH/WITH/UNWIND/RETURN` | Deny | `NonReadEntry` | `adversarial.rs` (`ORDER BY x`, `WHERE ...`) |

## Resource-bounding transforms (rewritten, not rejected)

| Construct | Policy | Proven by |
|-----------|--------|-----------|
| Legacy variable-length path `*`, `*n`, `*n..`, `*..m`, `*n..m` over `max_path_depth` | Rewrite to `*lower..5` (`transforms.rs`) | `transforms.rs` tests, `properties.rs` |

## Accepted residual risks

These are known and deliberate; they are documented here so they are never
mistaken for guarantees.

1. **Deny-list, not a parser.** A *new* write-capable or admin clause in a future
   Neo4j release passes until added to `DENY_MUTATION`/`DENY_ADMIN`. Mitigated by:
   the read-entry rule (a query must begin with a read clause), the
   `every_denied_keyword_is_enforced` table test, fuzzing, and the re-audit
   checklist below. This is the reason the matrix is version-pinned.
2. **Safe over-rejection.** A bare variable/label/alias/property that spells a
   denied keyword (e.g. a node named `create`, a `:Set` label) is rejected, and
   read-only `CALL { }` subqueries and `USING` query hints are rejected. This
   trades ergonomics for a sound sole guard ([ADR-0001](adr/0001-tokenizer-sanitizer.md));
   surfaced to agents in `SKILL.md`.
3. **Quantified path patterns are rejected outright.** Neo4j 5 `(...)+` /
   `(...){1,n}` patterns are read-only but are not cost-bounded by the path
   transform (which only rebinds legacy `*` ranges), so they are denied
   (`QuantifiedPath`) rather than accepted and left to the server timeout. Callers
   are told to use a bounded variable-length relationship instead. The residual risk
   is ergonomic, not a write or cost concern.
4. **Caps bound output, not server work.** The row and byte caps bound what is
   returned; a broad scan/sort/aggregation can still be expensive before the
   first row. Server execution cost is bounded server-side, not by this gateway.

## Re-audit checklist (run on each Neo4j minor-version bump)

- [ ] Read the target release's Cypher changelog for new clauses, especially
      write-capable (`CREATE`-like), administration, and import/load clauses.
- [ ] Add any new write/admin clause to `DENY_MUTATION` / `DENY_CALL` / `DENY_ADMIN`;
      the `every_denied_keyword_is_enforced` test then proves it is enforced. Every
      deny table must be chained into `denied_keywords()`, which is the oracle the
      property test and the fuzzer scan accepted output against.
- [ ] Add a hand-written `adversarial.rs` REJECT case for each new clause with its
      expected reason.
- [ ] Confirm no new read clause is wrongly rejected: add it to `READ_ENTRY` if it
      can legally lead a query, and add an `accept_spec_queries.rs` ACCEPT case.
- [ ] Re-run the fuzz oracle for an extended budget against the new keyword set.
- [ ] Bump the "Audited against" versions at the top of this file **and** the
      `AUDITED_NEO4J_IMAGE` constant in `crates/cypher-guard/tests/version_pinning.rs`
      (the test fails until the tag matches across compose, CI, and this file).

## Test provenance

| Layer | Where |
|-------|-------|
| Deny/accept corpus | `crates/cypher-guard/tests/adversarial.rs` |
| Spec query acceptance | `crates/cypher-guard/tests/accept_spec_queries.rs` |
| Deny-table enforcement | `crates/cypher-guard/tests/adversarial.rs::every_denied_keyword_is_enforced` |
| Path bounding | `crates/cypher-guard/tests/transforms.rs` |
| Invariants / no-hang | `crates/cypher-guard/tests/properties.rs` |
| Differential fuzz oracle | `crates/cypher-guard/fuzz/fuzz_targets/sanitize.rs` |
| Audited-version pin (tag drift fails the build) | `crates/cypher-guard/tests/version_pinning.rs` |
| End-to-end write rejection (live DB, graph unchanged) | `crates/nexus-scout/tests/integration.rs::gateway_rejects_writes_and_leaves_graph_unchanged` |
