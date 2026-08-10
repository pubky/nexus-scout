---
name: nexus-scout
description: Use when you need facts from the Pubky social graph - who follows whom, trending tags, thread/reply history, author reputation, trust distance between users. Query the Neo4j graph with read-only Cypher over HTTP.
---

# Querying the Pubky social graph (nexus-scout)

nexus-scout is a public, read-only gateway to the Pubky social graph. You write Cypher, it validates
the query is read-only, runs it against Neo4j, and returns JSON. No account, no API key, no install.

**Base URL: `https://nexus-scout.pubky.org`**

## Quickstart

One POST is the whole API:

```sh
curl -s https://nexus-scout.pubky.org/v1/query \
  -H 'content-type: application/json' \
  -d '{"cypher":"MATCH (u:User)<-[f:FOLLOWS]-() RETURN u.name AS name, count(f) AS followers ORDER BY followers DESC LIMIT 10"}'
```

```json
{"results":[{"name":"John Carvalho","followers":283}],"count":1,"truncated":false}
```

Body fields: `cypher` (required), `params` (object, optional), `limit` (number, optional, forces the
row cap). Single-quote the shell argument so `$param` references reach the server intact.

## Learn the schema first

```sh
curl -s https://nexus-scout.pubky.org/v1/schema
```

Returns the node labels and their properties, the relationship types with direction, and example
queries. It is the source of truth; the recipes below are starting points.

## Recipes

Copy these. They cover most of what gets asked.

**Find a user by name.** Names are display names and are not unique, so check before you commit to
one. `id` is the pubky public key and is the stable handle.

```cypher
MATCH (u:User) WHERE toLower(u.name) CONTAINS toLower($name)
OPTIONAL MATCH (u)<-[f:FOLLOWS]-()
RETURN u.id AS id, u.name AS name, u.bio AS bio, count(f) AS followers
ORDER BY followers DESC LIMIT 10
```

Pick by follower count and bio when several match. Searching "Carvalho" returns four users, one of
whom has 283 followers and another 2.

**Friends.** There is no friendship edge. The usual reading is a mutual follow:

```cypher
MATCH (u:User {id: $id})-[:FOLLOWS]->(f:User)-[:FOLLOWS]->(u)
RETURN count(DISTINCT f) AS friends
```

Swap `count(DISTINCT f)` for `f.name AS name` plus `ORDER BY`/`SKIP`/`LIMIT` to list them. Run the
count first: the list is capped (see Limits) and the count tells you whether you got all of them.
Say which definition you used, since one-way follows give different numbers.

**Someone's most-used tag.** `TAGGED` points at both posts and users, and the answer can change
depending on which you count, so split it when it matters:

```cypher
MATCH (u:User {id: $id})-[t:TAGGED]->(x)
RETURN t.label AS tag, count(*) AS uses, labels(x)[0] AS target
ORDER BY uses DESC LIMIT 20
```

Drop `labels(x)[0]` for a combined ranking. Add `WHERE x:Post` to count only post tags.

**Trending tags.**

```cypher
MATCH (:User)-[t:TAGGED]->(p:Post) WHERE t.indexed_at > $since
RETURN t.label AS tag, count(p) AS uses ORDER BY uses DESC LIMIT 20
```

**A user's reputation**, meaning the tags other people put on them:

```cypher
MATCH (:User)-[t:TAGGED]->(u:User {id: $id})
RETURN t.label AS label, count(*) AS n ORDER BY n DESC LIMIT 20
```

**Reply thread under a post.**

```cypher
MATCH (reply:Post)-[:REPLIED*0..5]->(root:Post {id: $post_id})
MATCH (a:User)-[:AUTHORED]->(reply)
RETURN reply.content AS content, a.name AS author, reply.indexed_at AS at
ORDER BY at LIMIT 50
```

**Follow distance between two users.**

```cypher
MATCH path = shortestPath((a:User {id: $from})-[:FOLLOWS*..5]->(b:User {id: $to}))
RETURN length(path) AS distance, [n IN nodes(path) | n.name] AS chain
```

Parameters go in `params`:

```sh
curl -s https://nexus-scout.pubky.org/v1/query \
  -H 'content-type: application/json' \
  -d '{"cypher":"MATCH (u:User {id: $id})-[:FOLLOWS]->(f)-[:FOLLOWS]->(u) RETURN count(DISTINCT f) AS friends",
       "params":{"id":"gujx6qd8ksydh1makdphd3bxu351d9b8waqka8hfg6q7hnqkxexo"}}'
```

## Limits and paging

**Rows are capped, and the cap is easy to miss.** No `LIMIT` gives you 25. The hard ceiling is 100,
so `LIMIT 500` silently returns 100. `truncated: true` and the `notes` array tell you when a cap bit.

Page past the ceiling with `SKIP`, and reconcile against a `count()` so you know when you have
everything:

```cypher
MATCH (u:User {id: $id})-[:FOLLOWS]->(f:User)-[:FOLLOWS]->(u)
RETURN f.name AS name ORDER BY name SKIP 100 LIMIT 100
```

Getting this wrong is the most common way to report a confidently wrong number. If a count says 134
and your list has 100, you are missing 34.

Other bounds:

- Variable-length paths are capped at `*1..5`. Write `*` and it becomes `*1..5`; the response `notes`
  say so when it happens.
- Queries time out around 10 s. Narrow the `MATCH`, add a `LIMIT`, or reduce depth.
- Several `OPTIONAL MATCH` clauses in one query multiply into a cartesian product and time out. Run
  separate queries instead, or wrap the counts in `count(DISTINCT ...)`.
- Request bodies are capped at 64 KiB.
- Address result columns by name. Column order is not guaranteed.

## What is in the graph

Four node labels and eight relationship types.

| you want | use |
|---|---|
| followers / following | `FOLLOWS` (User→User) |
| a user's posts | `AUTHORED` (User→Post) |
| reply threads | `REPLIED` (Post→Post) |
| reposts | `REPOSTED` (Post→Post) |
| tags on posts or people | `TAGGED` (User→Post and User→User), tag text is `t.label` |
| bookmarks | `BOOKMARKED` (User→Post) |
| mentions | `MENTIONED` (Post→User) |
| mutes | `MUTED` (User→User) |

`User.id` and `Post.id` are the stable identifiers. `indexed_at` is Unix milliseconds and is the only
time signal.

**Tags are not nodes.** The tag text lives on the `TAGGED` relationship's `label` property. There is
no `(:Tag)` node to traverse to, and matching one returns nothing. Files are found by scanning a
property (`MATCH (f:File) WHERE f.owner_id = $id`), not by traversal.

**Not modeled**, so do not try: likes, reactions, upvotes, view or engagement counts, direct
messages, per-item privacy flags, edit history, and ranked full-text search. Match text with
`CONTAINS` / `STARTS WITH` / `=`, which is exact or substring, not relevance-ranked. Popularity is
inferable only by counting `FOLLOWS` / `TAGGED` / `REPOSTED` edges.

## Rules

Read-only is enforced by a sanitizer, not by convention:

- `CREATE`, `MERGE`, `SET`, `DELETE`, `REMOVE`, `DROP`, `FOREACH`, `LOAD`, and `INSERT` are rejected.
- `CALL` is rejected in every form, both stored procedures and `CALL {}` subqueries. Bare functions
  need no `CALL` and are fine: `count()`, `collect()`, `shortestPath()`, `datetime()`, `labels()`,
  `type()`.
- Namespaced calls (`apoc.*`, `db.*`, `dbms.*`, `gds.*`) are rejected.
- Admin and selector clauses (`USE`, `SHOW`, `PROFILE`, `EXPLAIN`) are rejected.
- Comments and `;` are rejected.
- Quantified path patterns (`((a)-[:R]->(b)){1,3}`) are rejected. Use a bounded variable-length
  relationship instead: `-[:FOLLOWS*1..5]->`.
- A variable that spells a reserved keyword is rejected. Rename it.

## Reading the output

```json
{"results":[{"name":"Alice","followers":142}],"count":1,"truncated":false}
```

`truncated: true` means a cap dropped rows. `notes` appears only when the gateway changed your query
and reports what it did, for example `"variable-length path '*1..10' bounded to '*1..5'"`. Read it
whenever a path or count answer looks smaller than you expected.

Errors are `{"error": CODE, "message": ..., "hint": ...}`. The `hint` names the fix.

| code | meaning | what to do |
|---|---|---|
| `QUERY_REJECTED` | A guardrail refused the query | Read the `hint`; it names the clause. Rewrite with plain `MATCH`/`WITH`/`RETURN`. |
| `QUERY_SYNTAX_ERROR` | Neo4j could not parse it | `message` carries the offending token, line, and column. Fix and resend. |
| `QUERY_TIMEOUT` | Too expensive | Add a `LIMIT`, narrow the `MATCH`, reduce path depth, or split one query into several. |
| `INTERNAL_ERROR` | Transient | Retry once. |

A 503 from `/ready` means the operators have not finished configuring server-side cost bounds. It
does not stop `/v1/query` from answering.

## The CLI (optional)

`scout` wraps the same HTTP API with exit codes for scripting. curl is enough; use this if you want
the ergonomics.

```sh
cargo install --git https://github.com/pubky/nexus-scout nexus-scout-cli
scout query "MATCH (u:User) RETURN u.name LIMIT 5"
scout schema
```

It defaults to `https://nexus-scout.pubky.org`; override with `--server-url` or `NEXUS_SCOUT_URL`.
Parameters take `--param key=value` for strings and `--params-json '{...}'` for typed values. Exit
codes: `0` ok, `1` internal or transient, `2` rejected, `3` timeout. The JSON envelope always goes to
stdout for `jq`.
