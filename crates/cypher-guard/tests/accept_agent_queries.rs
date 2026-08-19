//! Real agent queries that MUST pass the sanitizer; a false positive silently
//! rejects a legitimate read.

use cypher_guard::{Limits, Sanitizer};

fn s() -> Sanitizer {
    Sanitizer::new(Limits::default())
}

/// `get_schema` example queries.
const SCHEMA_EXAMPLES: &[&str] = &[
    "MATCH (u:User)<-[f:FOLLOWS]-() RETURN u.name, count(f) AS followers ORDER BY followers DESC LIMIT 10",
    "MATCH (a:User)-[:AUTHORED]->(p:Post)-[:REPLIED]->(parent:Post) WHERE a.id = $user_id RETURN p.content, parent.content LIMIT 25",
    "MATCH (u:User)-[t:TAGGED]->(p:Post) WHERE t.indexed_at > $since RETURN t.label, count(p) AS cnt ORDER BY cnt DESC LIMIT 20",
];

/// Agent interaction-flow queries.
const FLOW_QUERIES: &[&str] = &[
    // thread summarization
    "MATCH (root:Post {id: $post_id})
     MATCH (reply:Post)-[:REPLIED*0..5]->(root)
     MATCH (author:User)-[:AUTHORED]->(reply)
     RETURN reply.id, reply.content, reply.indexed_at, author.name
     ORDER BY reply.indexed_at ASC
     LIMIT 50",
    // fact-check social context
    "MATCH (author:User)-[:AUTHORED]->(p:Post {id: $post_id})
     OPTIONAL MATCH (tagger:User)-[t:TAGGED]->(author)
     RETURN author.id, author.name,
            collect({tagger: tagger.name, label: t.label}) AS tags
     LIMIT 25",
    // network analysis
    "MATCH (u:User)-[t:TAGGED]->(p:Post)
     WHERE t.indexed_at > $since
     WITH t.label AS tag, count(p) AS postCount,
          count(DISTINCT u) AS authorCount
     RETURN tag, postCount, authorCount
     ORDER BY postCount DESC
     LIMIT 20",
    // trust assessment (three queries)
    "MATCH (u:User {id: $target})<-[f:FOLLOWS]-()
     RETURN count(f) AS followers",
    "MATCH (tagger:User)-[t:TAGGED]->(u:User {id: $target})
     RETURN tagger.name, t.label, t.indexed_at
     ORDER BY t.indexed_at DESC LIMIT 25",
    "MATCH path = shortestPath(
        (me:User {id: $viewer})-[:FOLLOWS*..5]->(them:User {id: $target})
      )
      RETURN length(path) AS distance, [n IN nodes(path) | n.name] AS chain",
    "MATCH (u:User) RETURN u.id, count { (u)-[:FOLLOWS]->(:User) } AS following LIMIT 3",
    "MATCH (u:User) RETURN u.id, exists { MATCH (u)-[:AUTHORED]->(:Post) } AS has_posts LIMIT 3",
    "MATCH (u:User) RETURN u.id, collect { MATCH (u)-[:AUTHORED]->(p:Post) RETURN p.id LIMIT 3 } AS post_ids LIMIT 3",
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
