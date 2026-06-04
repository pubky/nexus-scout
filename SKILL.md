---
name: nexus-scout
description: Use when you need facts from the Pubky social graph - who follows whom, trending tags, thread/reply history, author reputation, trust distance between users. Query the Neo4j graph with read-only Cypher via the `nexus-scout` CLI.
---

# Querying the Pubky social graph (nexus-scout)

`nexus-scout` is a read-only gateway to the Pubky social graph. You write Cypher; it validates the
query is read-only, runs it against Neo4j, and returns JSON.

## When to use

Reach for this when answering a question needs graph facts, for example:
- "Who are X's followers / who does X follow?"
- "What tags are trending this week?"
- "Reconstruct this reply thread."
- "Is this author trusted - how is X tagged, and what's the follow distance from Y to X?"

## Workflow

1. **Learn the schema first** (no database round-trip; cache the result):
   ```sh
   nexus-scout schema
   ```
   This returns the node types (`User`, `Post`, `Tag`, `File`) with their properties, the
   relationship types (`FOLLOWS`, `AUTHORED`, `TAGGED`, `REPLIED`, `REPOSTED`, `BOOKMARKED`,
   `MENTIONED`, `MUTED`) with direction, and example queries.

2. **Write read-only Cypher and run it**:
   ```sh
   nexus-scout query "MATCH (u:User)<-[f:FOLLOWS]-() RETURN u.name, count(f) AS followers ORDER BY followers DESC LIMIT 10"
   ```

3. **Pass parameters** - typed values (numbers, arrays) via `--params-json`, simple strings via
   `--param key=value`:
   ```sh
   nexus-scout query --params-json '{"since": 1709251200000}' \
     "MATCH (u:User)-[t:TAGGED]->(p:Post) WHERE t.indexed_at > \$since RETURN t.label, count(p) AS c ORDER BY c DESC"
   ```

## Capabilities & limits

`get_schema` is the source of truth; the examples below are starting points. Everything is read-only
over four node types (`User`, `Post`, `Tag`, `File`) and eight relationships (`FOLLOWS`, `AUTHORED`,
`TAGGED`, `REPLIED`, `REPOSTED`, `BOOKMARKED`, `MENTIONED`, `MUTED`).

**Answerable** — map the question to a relationship:
- Followers / following and counts → `FOLLOWS` (User→User)
- A user's posts → `AUTHORED` (User→Post)
- Reply threads → `REPLIED` (Post→Post); reposts → `REPOSTED` (Post→Post)
- Tags on posts / trending tags → `TAGGED` (User→Post; the tag text is `t.label`)
- Bookmarks → `BOOKMARKED`; mentions → `MENTIONED` (Post→User); mutes → `MUTED` (User→User)
- Follow / trust distance between two users → variable-length `FOLLOWS` path (capped `*1..5`)

```sh
# Trending tags since a timestamp
nexus-scout query --params-json '{"since": 1709251200000}' \
  "MATCH (u:User)-[t:TAGGED]->(p:Post) WHERE t.indexed_at > \$since
   RETURN t.label, count(p) AS uses ORDER BY uses DESC LIMIT 20"

# Reconstruct a reply thread under a post
nexus-scout query --params-json '{"post_id": "pk:..."}' \
  "MATCH (reply:Post)-[:REPLIED*0..5]->(root:Post {id: \$post_id})
   MATCH (a:User)-[:AUTHORED]->(reply)
   RETURN reply.content, a.name, reply.indexed_at ORDER BY reply.indexed_at LIMIT 50"

# Follow distance from one user to another
nexus-scout query --params-json '{"from": "pk:a", "to": "pk:b"}' \
  "MATCH path = shortestPath((me:User {id: \$from})-[:FOLLOWS*..5]->(them:User {id: \$to}))
   RETURN length(path) AS distance, [n IN nodes(path) | n.name] AS chain"
```

**Not modeled** — `get_schema` shows nothing for these, so don't try:
- No likes/reactions/upvotes or view/engagement counts — popularity is only inferable by counting
  `FOLLOWS` / `TAGGED` / `REPOSTED` edges (`BOOKMARKED` is the only "save").
- No direct messages, message bodies, or per-item privacy/visibility flags.
- No edit or version history; the only time signal is `indexed_at` (Unix ms) — there is no separate
  created / updated / deleted timeline.
- No full-text/relevance search — match text with property predicates (`CONTAINS`, `STARTS WITH`,
  `=`), which is exact/substring, not ranked.
- `Tag` and `File` have no connecting relationship: tag text is the `label` property on `TAGGED`, and
  files are found by scanning a property (`MATCH (f:File) WHERE f.owner_id = \$id ...`), not by traversal.

## Rules and guardrails

- **Read-only only.** Any `CREATE`, `MERGE`, `SET`, `DELETE`, `REMOVE`, `DROP`, `FOREACH`, `LOAD`,
  `CALL`, or admin clause (`USE`, `SHOW`, `PROFILE`, ...) is rejected. Namespaced calls like
  `apoc.*` / `db.*` / `gds.*` are rejected too. Bare functions (`count()`, `collect()`,
  `shortestPath()`, `datetime()`, ...) are fine.
- **Limits**: if you omit `LIMIT`, you get 25 rows; the hard ceiling is 100. A large `LIMIT` is
  capped. Variable-length paths are capped at `*1..5` (write `*` and it becomes `*1..5`).
- **Timeout**: queries are bounded at ~10 s. If you hit `QUERY_TIMEOUT`, add a `LIMIT`, narrow the
  `MATCH`, or reduce path depth.
- **Address result columns by name** - column order in the JSON is not guaranteed.
- A variable that happens to spell a reserved keyword (e.g. naming a node `create`) is rejected;
  just rename the variable.

## Reading the output

Success:
```json
{ "results": [ { "u.name": "Alice", "followers": 142 } ], "count": 1, "truncated": false }
```
`truncated: true` means a guardrail capped the result - add a tighter `LIMIT` or narrow `RETURN`.

Error:
```json
{ "error": "QUERY_REJECTED", "message": "...", "hint": "..." }
```
Read `hint` and adjust. Error codes: `QUERY_REJECTED` (fix the query), `QUERY_TIMEOUT` (narrow it),
`QUERY_SYNTAX_ERROR` (fix Cypher syntax), `INTERNAL_ERROR` (retry). Exit codes mirror these
(0 ok, 2 rejected, 3 timeout, 1 internal), and the JSON envelope is always on stdout for `jq`.
