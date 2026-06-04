//! Every query the spec itself shows an agent issuing MUST pass the sanitizer.
//! A false-positive here means a real agent's query is silently rejected, the
//! single highest ship risk. These strings are copied verbatim from the spec's
//! §5.2 `get_schema` examples and §7 interaction flows.

use cypher_guard::{Limits, Sanitizer};

fn s() -> Sanitizer {
    Sanitizer::new(Limits::default())
}

/// §5.2 `get_schema` example queries.
const SCHEMA_EXAMPLES: &[&str] = &[
    "MATCH (u:User)<-[f:FOLLOWS]-() RETURN u.name, count(f) AS followers ORDER BY followers DESC LIMIT 10",
    "MATCH (a:User)-[:AUTHORED]->(p:Post)-[:REPLIED]->(parent:Post) WHERE a.id = $user_id RETURN p.content, parent.content LIMIT 25",
    "MATCH (u:User)-[t:TAGGED]->(p:Post) WHERE t.indexed_at > $since RETURN t.label, count(p) AS cnt ORDER BY cnt DESC LIMIT 20",
];

/// §7 agent interaction-flow queries.
const FLOW_QUERIES: &[&str] = &[
    // 7.1 thread summarization
    "MATCH (root:Post {id: $post_id})
     MATCH (reply:Post)-[:REPLIED*0..5]->(root)
     MATCH (author:User)-[:AUTHORED]->(reply)
     RETURN reply.id, reply.content, reply.indexed_at, author.name
     ORDER BY reply.indexed_at ASC
     LIMIT 50",
    // 7.2 fact-check social context
    "MATCH (author:User)-[:AUTHORED]->(p:Post {id: $post_id})
     OPTIONAL MATCH (tagger:User)-[t:TAGGED]->(author)
     RETURN author.id, author.name,
            collect({tagger: tagger.name, label: t.label}) AS tags
     LIMIT 25",
    // 7.3 network analysis
    "MATCH (u:User)-[t:TAGGED]->(p:Post)
     WHERE t.indexed_at > $since
     WITH t.label AS tag, count(p) AS postCount,
          count(DISTINCT u) AS authorCount
     RETURN tag, postCount, authorCount
     ORDER BY postCount DESC
     LIMIT 20",
    // 7.4 trust assessment (three queries)
    "MATCH (u:User {id: $target})<-[f:FOLLOWS]-()
     RETURN count(f) AS followers",
    "MATCH (tagger:User)-[t:TAGGED]->(u:User {id: $target})
     RETURN tagger.name, t.label, t.indexed_at
     ORDER BY t.indexed_at DESC LIMIT 25",
    "MATCH path = shortestPath(
        (me:User {id: $viewer})-[:FOLLOWS*..5]->(them:User {id: $target})
      )
      RETURN length(path) AS distance, [n IN nodes(path) | n.name] AS chain",
];

fn assert_all_accepted(corpus: &[&str], what: &str) {
    let s = s();
    for q in corpus {
        let result = s.sanitize(q);
        assert!(result.is_ok(), "{what} rejected: {q:?} ({:?})", result.err());
    }
}

#[test]
fn schema_examples_pass() {
    assert_all_accepted(SCHEMA_EXAMPLES, "schema example");
}

#[test]
fn flow_queries_pass() {
    assert_all_accepted(FLOW_QUERIES, "flow query");
}
