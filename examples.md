# nexus-scout query examples

End-to-end examples of answering natural-language questions about the Pubky social graph through the
`scout` CLI (a client of the nexus-scout HTTP gateway). Each one follows the same agent loop:

1. **Decide what graph facts answer the question** (which labels / relationships).
2. **Check the schema** (`scout schema`) when unsure how something is modeled.
3. **Write read-only Cypher and run it** through `scout`.
4. **Read the JSON back as a plain answer** (the Cypher is shown so it can be verified).

Everything is read-only: the sanitizer rejects any write before it reaches Neo4j.

> The client targets `https://nexus-scout.pubky.org` by default; override with `--server-url` or
> `NEXUS_SCOUT_URL`. Results below are from the staging graph.

## Example 1: What are the most-used tags?

**Step 1-2 - what answers this, and confirm the model.** Tags could be nodes or edge properties, so
check the schema rather than guess:

```sh
scout schema | jq '.relationships[] | select(.type=="TAGGED")'
```
```json
{
  "type": "TAGGED",
  "from": "User",
  "to": "Post",
  "properties": { "id": "string", "label": "string", "indexed_at": "integer" }
}
```

`TAGGED` is a `User → Post` edge and the tag text is the edge's `label` property (not a separate
node), so "most-used tags" means counting `TAGGED` edges grouped by `label`.

**Step 3 - write read-only Cypher and run it:**

```sh
scout query "MATCH (:User)-[t:TAGGED]->(:Post) RETURN t.label AS tag, count(*) AS uses ORDER BY uses DESC LIMIT 5"
```
```json
{
  "results": [
    { "tag": "pubky",   "uses": 1653 },
    { "tag": "welcome", "uses": 1043 },
    { "tag": "bitcoin", "uses": 975 },
    { "tag": "music",   "uses": 742 },
    { "tag": "ai",      "uses": 601 }
  ],
  "count": 5,
  "truncated": false
}
```

**Step 4 - the answer.** The 5 most-used tags on staging are **pubky** (1,653 uses), **welcome**
(1,043), **bitcoin** (975), **music** (742), and **ai** (601).

Cypher run:
```cypher
MATCH (:User)-[t:TAGGED]->(:Post)
RETURN t.label AS tag, count(*) AS uses
ORDER BY uses DESC LIMIT 5
```

## Example 2: What's the follow distance between the two users most far apart?

This one is interesting because it bumps into the gateway's safety rails, and the honest answer is a
bounded one.

**Step 1 - what is this, in graph terms?** "Most far apart" over follow distance is the **diameter**
of the `FOLLOWS` graph: the largest shortest-path between any pair of users.

**Step 2 - reality check against the guardrails.** Diameter collides with three rails:
- Variable-length paths are **capped at `*1..5`** by the sanitizer, so distances are only measurable
  up to 5 hops.
- **`CALL` is blocked**, so the graph-algorithm procedures that compute diameter properly (GDS /
  APOC) are off-limits.
- A brute-force all-pairs shortest path over ~1.7k users would blow the 10 s timeout.

So I can't get the exact diameter, but I can establish a **lower bound** and learn the graph's shape.

**Step 3 - probe the structure.** First, eccentricity from the most-connected user (a good source):

```sh
scout query --params-json '{"src":"q9x5sfjbpajdebk45b9jashgb86iem7rnwpmu16px3ens63xzwro"}' \
  "MATCH (a:User {id:\$src}) MATCH (b:User) WHERE b <> a
   MATCH p = shortestPath((a)-[:FOLLOWS*1..5]->(b))
   RETURN max(length(p)) AS max_hops, count(*) AS reachable_within_5"
```
```json
{ "results": [ { "max_hops": 3, "reachable_within_5": 336 } ], "count": 1, "truncated": false }
```

The top hub reaches its *entire* reachable set in 3 hops, and only 336 of 1,682 users. So the
**directed** graph isn't strongly connected (most ordered pairs have no path), and the diameter is set
by the periphery, not the hub. "Far apart in the graph" usually means **undirected** separation, so
switch to undirected (`-[:FOLLOWS*1..5]-`, no arrow) and run a **2-sweep** (find the hub's farthest
node, then measure *its* eccentricity, the standard diameter lower-bound):

```sh
scout query --params-json '{"src":"q9x5sfjbpajdebk45b9jashgb86iem7rnwpmu16px3ens63xzwro"}' \
  "MATCH (a:User {id:\$src}) MATCH (b:User) WHERE b <> a
   MATCH p = shortestPath((a)-[:FOLLOWS*1..5]-(b))
   WITH b ORDER BY length(p) DESC LIMIT 1
   MATCH (c:User) WHERE c <> b
   MATCH p2 = shortestPath((b)-[:FOLLOWS*1..5]-(c))
   RETURN b.name AS far_node, max(length(p2)) AS eccentricity, count(*) AS reachable"
```
```json
{ "results": [ { "far_node": "Wawa", "eccentricity": 5, "reachable": 449 } ], "count": 1, "truncated": false }
```

**Step 4 - the answer.** There is a connected pair at undirected follow-distance **5**, which is
exactly the `*1..5` cap, so the answer is **at least 5, and the exact maximum is not computable through
nexus-scout**:

- We're hitting the 5-hop measurement ceiling, so anything farther is invisible.
- The directed graph isn't strongly connected and even undirected it's fragmented into clusters (the
  hub reaches only ~478 of 1,682 within 5 hops), so the literal maximum is partly undefined
  (cross-cluster pairs are infinitely far apart).
- Computing the true diameter needs GDS/APOC or a direct Bolt session, which the read-only gateway
  deliberately does not expose.

This is the gateway working as designed: it answers what it safely can (a sound lower bound and the
graph's shape) and refuses the unbounded traversal that a true diameter would require.

> Note: `shortestPath` needs **both** endpoints bound. An unbound end node
> (`shortestPath((a)-[...]->(b:User))` with `b` not matched first) errors out and is returned as
> `INTERNAL_ERROR`; bind it with a prior `MATCH (b:User)`.

## Example 3: Which small accounts act as bridges between otherwise separate communities?

The most interesting one: the rigorous answer is blocked, but plain-Cypher proxies (iterated) still
surface real, concrete bridges.

**Step 1 - what is this, in graph terms?** "Bridges between communities" is **brokerage**: nodes with
high *betweenness* (many shortest paths cross them) or that span **structural holes** (their contacts
aren't connected to each other). "Small accounts" = low degree.

**Step 2 - guardrails check.** The proper tools, betweenness centrality and community detection
(Louvain), are **GDS procedures** (`CALL`-blocked) and need global all-pairs computation. So compute a
**plain-Cypher proxy** and iterate.

**Step 3 - iterate proxies.**

*(a) Pure structural holes* (a small account whose contacts are pairwise unconnected): returns
nothing, even small accounts have *some* interconnection among contacts.

*(b) Lowest local clustering coefficient* (fraction of a user's contact-pairs that are themselves
connected):

```sh
scout query "MATCH (u:User)-[:FOLLOWS]-(a:User)
  WITH u, count(DISTINCT a) AS deg, collect(DISTINCT a) AS nbrs
  WHERE deg >= 5 AND deg <= 12
  OPTIONAL MATCH (x:User)-[:FOLLOWS]-(y:User)
  WHERE x IN nbrs AND y IN nbrs AND id(x) < id(y)
  WITH u, deg, count(DISTINCT id(x)*100000 + id(y)) AS connected_pairs
  RETURN u.name AS name, deg AS contacts,
         round(100.0*connected_pairs/(deg*(deg-1)/2)) AS clustering_pct
  ORDER BY clustering_pct ASC, deg DESC LIMIT 8"
```

This surfaces candidates but with **noise**: the top results are synthetic test accounts
(`Pagination Owner <timestamp>`), and inspecting a real one (`V1n`) shows it just links one hub
(*Big Bad John*, degree 261) to a handful of degree-1 leaves, "hub + leaves," not a community broker.

*(c) Refine to the real signal* - a small account linking **two well-connected hubs that don't follow
each other** (uses a `COUNT {}` subquery, which the sanitizer allows):

```sh
scout query "MATCH (u:User)-[:FOLLOWS]-(a:User)
  WHERE NOT coalesce(u.name,'') STARTS WITH 'Pagination Owner'
  WITH u, count(DISTINCT a) AS du, collect(DISTINCT a) AS nbrs
  WHERE du >= 2 AND du <= 12
  UNWIND nbrs AS h1 UNWIND nbrs AS h2
  WITH u, du, h1, h2 WHERE id(h1) < id(h2)
    AND count{ (h1)-[:FOLLOWS]-(:User) } >= 15
    AND count{ (h2)-[:FOLLOWS]-(:User) } >= 15
    AND NOT (h1)-[:FOLLOWS]-(h2)
  RETURN u.name AS bridge, du AS contacts, h1.name AS hub_a, h2.name AS hub_b LIMIT 8"
```
```json
{ "results": [
  { "bridge": "stagy",          "contacts": 12, "hub_a": "Miguel Medeiros", "hub_b": "JohnDev" },
  { "bridge": "BraveBrave…",    "contacts": 8,  "hub_a": "s0nG0ku",         "hub_b": "Pav" },
  { "bridge": "Z32P-DXCK-9N2G", "contacts": 2,  "hub_a": "Miguel Medeiros", "hub_b": "moon rock" }
], "count": 8, "truncated": false }
```

**Step 4 - the answer.** Concrete small bridges:
- **`Z32P-DXCK-9N2G`** - only **2 contacts**, both hubs (*Miguel Medeiros*, *moon rock*) that don't
  follow each other: a textbook tiny broker.
- **stagy** (12 contacts) and **BraveBrave…** (8) - each links several otherwise-unconnected hubs.

Honest caveat: this is a **structural proxy**, not true betweenness or community detection. The hubs
stand in for "communities"; rigorous detection (Louvain + betweenness, or `gds.bridges`) needs GDS via
a direct Bolt session, which the read-only gateway deliberately does not expose. Real graph data also
carries noise (synthetic `Pagination Owner …` / padded-name accounts) that has to be filtered out.

## Example 4: What was the total activity and the hottest 3 topics in January 2026?

A time-windowed analytics question, runs clean (no guardrails hit).

**Step 1 - the time model.** The only time signal is `indexed_at` (Unix ms), so "January 2026" is a
window. Compute the bounds (UTC):

```sh
date -u -d '2026-01-01 00:00:00' +%s%3N   # 1767225600000
date -u -d '2026-02-01 00:00:00' +%s%3N   # 1769904000000
```

**Step 2 - define "activity".** Only some edges carry `indexed_at` (`FOLLOWS`, `TAGGED`, `BOOKMARKED`,
`MUTED`); `AUTHORED`/`REPLIED`/`REPOSTED`/`MENTIONED` do not (time-bound those via `Post.indexed_at`).
So count the events stamped in the window: posts created, tags applied, follows, bookmarks.

**Step 3 - run it.** Total activity, via `COUNT {}` subqueries with native parameters:

```sh
scout query --params-json '{"start":1767225600000,"end":1769904000000}' "RETURN
  count{ MATCH (p:Post)              WHERE p.indexed_at >= \$start AND p.indexed_at < \$end } AS posts,
  count{ MATCH ()-[t:TAGGED]->()     WHERE t.indexed_at >= \$start AND t.indexed_at < \$end } AS tags,
  count{ MATCH ()-[f:FOLLOWS]->()    WHERE f.indexed_at >= \$start AND f.indexed_at < \$end } AS follows,
  count{ MATCH ()-[b:BOOKMARKED]->() WHERE b.indexed_at >= \$start AND b.indexed_at < \$end } AS bookmarks"
```
```json
{ "results": [ { "posts": 1889, "tags": 2640, "follows": 139, "bookmarks": 29 } ], "count": 1, "truncated": false }
```

Hottest topics, tags applied in the window grouped by label:

```sh
scout query --params-json '{"start":1767225600000,"end":1769904000000}' \
  "MATCH (:User)-[t:TAGGED]->(:Post) WHERE t.indexed_at >= \$start AND t.indexed_at < \$end
   RETURN t.label AS topic, count(*) AS uses ORDER BY uses DESC LIMIT 3"
```
```json
{ "results": [ { "topic": "ai", "uses": 58 }, { "topic": "test", "uses": 41 }, { "topic": "nice", "uses": 34 } ], "count": 3, "truncated": false }
```

**Step 4 - the answer.** January 2026 saw **~4,697 events**: **1,889 posts**, **2,640 tags**, **139
follows**, **29 bookmarks**. The hottest topics were **`ai`** (58), `test` (41), and `nice` (34), with
`test` almost certainly test-data noise, so the genuine leading topic was **`ai`**. The tag
distribution is long-tailed (2,640 tag events across many labels), so even the top topic is only 58
uses for the month.

---

The lesson across these examples: the gateway answers what it can **safely and within bounds**, an
exact fact (Ex. 1 & 4), a sound lower bound (Ex. 2), or an iterated structural proxy (Ex. 3), and is
transparent about where a question needs the global graph algorithms it intentionally blocks.
