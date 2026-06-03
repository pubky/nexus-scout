//! The critical adversarial corpus: every input here MUST be rejected (with the
//! expected reason) or accepted. This file is the executable form of the spec's
//! "block 100% of mutation attempts" success criterion.

use cypher_guard::{Limits, RejectReason, Sanitizer};

fn sanitizer() -> Sanitizer {
    Sanitizer::new(Limits::default())
}

/// Each case: (cypher, expected reason).
const REJECT: &[(&str, RejectReason)] = &[
    // --- Mutations (plain) ---
    ("CREATE (n:User {id:'x'}) RETURN n", RejectReason::Mutation),
    ("MATCH (n:User) DELETE n", RejectReason::Mutation),
    ("MATCH (n:User) DETACH DELETE n", RejectReason::Mutation),
    ("MATCH (n) SET n.name = 'x' RETURN n", RejectReason::Mutation),
    ("MERGE (n:User {id:'x'}) RETURN n", RejectReason::Mutation),
    ("MATCH (n) REMOVE n.name RETURN n", RejectReason::Mutation),
    ("DROP INDEX foo", RejectReason::Mutation),
    (
        "MATCH (n) WITH n FOREACH (x IN [1] | SET x.y = 1)",
        RejectReason::Mutation,
    ),
    (
        "LOAD CSV FROM 'file:///etc/passwd' AS row RETURN row",
        RejectReason::Mutation,
    ),
    // --- GQL-style and mid-query write clauses (must be caught anywhere, not
    //     just at statement start) ---
    ("MATCH (n) INSERT (m:Evil {x:1}) RETURN m", RejectReason::Mutation),
    ("INSERT (m:Evil) RETURN m", RejectReason::Mutation),
    ("MATCH (n) WITH n CREATE (m) RETURN m", RejectReason::Mutation),
    ("MATCH (n) NODETACH DELETE n", RejectReason::Mutation),
    ("MATCH (n) CALL { CREATE (m) } RETURN n", RejectReason::Mutation),
    // --- Case / spacing obfuscation ---
    ("cReAtE (n) RETURN n", RejectReason::Mutation),
    ("match (n) delete n", RejectReason::Mutation),
    ("MATCH (n)\nDETACH    DELETE n", RejectReason::Mutation),
    // --- Comment injection ---
    (
        "MATCH (n) RETURN n // harmless\nCREATE (x)",
        RejectReason::CommentInjection,
    ),
    ("MATCH (n) RETURN n /* c */ DELETE n", RejectReason::CommentInjection),
    ("MATCH (n) /* CREATE */ RETURN n", RejectReason::CommentInjection),
    ("MATCH (n) RETURN n /* unterminated CREATE", RejectReason::Unterminated),
    // --- Semicolon / multi-statement ---
    ("MATCH (n) RETURN n; CREATE (x)", RejectReason::Semicolon),
    ("MATCH (n) RETURN n ;", RejectReason::Semicolon),
    (";MATCH (n) RETURN n", RejectReason::Semicolon),
    // --- String-boundary tricks ---
    (
        "MATCH (n) WHERE n.x = \"unterminated RETURN n",
        RejectReason::Unterminated,
    ),
    // --- Stored procedures / apoc / gds / namespaced functions ---
    ("CALL db.labels() YIELD label RETURN label", RejectReason::Mutation),
    (
        "CALL apoc.periodic.iterate('MATCH (n)','DELETE n',{}) YIELD batches RETURN batches",
        RejectReason::Mutation,
    ),
    ("MATCH (n) RETURN apoc.convert.toJson(n)", RejectReason::NamespacedCall),
    ("MATCH (n) RETURN gds.util.asNode(0)", RejectReason::NamespacedCall),
    (
        "MATCH (n) RETURN apoc . convert . toJson(n)",
        RejectReason::NamespacedCall,
    ),
    ("MATCH (n) RETURN db . labels()", RejectReason::NamespacedCall),
    (
        "MATCH (n) RETURN dbms.security.listUsers()",
        RejectReason::NamespacedCall,
    ),
    (
        "MATCH (n) RETURN gds.graph.project('g','User','FOLLOWS')",
        RejectReason::NamespacedCall,
    ),
    // namespaced call with newline between dots (whitespace-insensitive)
    (
        "MATCH (n) RETURN apoc\n.\nconvert\n.\ntoJson(n)",
        RejectReason::NamespacedCall,
    ),
    // namespaced call where a segment is backtick-quoted (Neo4j resolves
    // apoc.`cypher`.doIt to apoc.cypher.doIt; must NOT bypass the rule)
    (
        "MATCH (n) RETURN apoc.`cypher`.runFirstColumn('CREATE (x)', {}) AS r",
        RejectReason::NamespacedCall,
    ),
    (
        "MATCH (n) RETURN apoc.convert.`toJson`(n)",
        RejectReason::NamespacedCall,
    ),
    (
        "MATCH (n) RETURN `apoc`.`cypher`.`doIt`('CREATE (x)', {}) AS r",
        RejectReason::NamespacedCall,
    ),
    // --- Admin / selector clauses ---
    ("USE system MATCH (n) RETURN n", RejectReason::AdminClause),
    ("SHOW USERS", RejectReason::AdminClause),
    ("SHOW DATABASES", RejectReason::AdminClause),
    ("PROFILE MATCH (n) RETURN n", RejectReason::AdminClause),
    ("EXPLAIN MATCH (n) RETURN n", RejectReason::AdminClause),
    ("TERMINATE TRANSACTION 'x'", RejectReason::AdminClause),
    (
        "USING PERIODIC COMMIT 500 LOAD CSV FROM 'x' AS r RETURN r",
        RejectReason::Mutation,
    ),
    // --- Unicode tricks ---
    ("\u{0421}REATE (n) RETURN n", RejectReason::NonAsciiKeyword), // Cyrillic C
    (
        "\u{FF23}\u{FF32}\u{FF25}\u{FF21}\u{FF34}\u{FF25} (n) RETURN n",
        RejectReason::NonAsciiKeyword,
    ), // fullwidth
    ("MATCH (n) RETURN n\u{2028}DELETE n", RejectReason::NonAsciiKeyword), // line separator
    ("MATCH (n) DEL\u{200D}ETE n", RejectReason::NonAsciiKeyword), // ZWJ split
    ("MATCH (n) RETURN n\u{FEFF}", RejectReason::NonAsciiKeyword), // BOM / zero-width no-break space
    ("MATCH (n) RETURN n\u{202E}", RejectReason::NonAsciiKeyword), // right-to-left override (bidi)
    // --- Comment-splice that must not reassemble a keyword across the boundary ---
    ("MAT/**/CH (n) RETURN n", RejectReason::CommentInjection),
    ("MATCH (n) RETURN n/*c*/", RejectReason::CommentInjection),
    // --- Non-read entry ---
    ("ORDER BY x", RejectReason::NonReadEntry),
    ("WHERE n.x = 1 RETURN n", RejectReason::NonReadEntry),
    // --- Bare variable spelling a denied keyword (documented safe over-rejection) ---
    ("MATCH (create:User) RETURN create", RejectReason::Mutation),
    // --- Empty ---
    ("", RejectReason::Empty),
    ("   \n  ", RejectReason::Empty),
];

/// Valid read-only queries that MUST be accepted (after guardrail transforms).
const ACCEPT: &[&str] = &[
    "MATCH (u:User)<-[f:FOLLOWS]-() RETURN u.name, count(f) AS followers ORDER BY followers DESC LIMIT 5",
    "MATCH (u:User) WHERE u.name CONTAINS 'create' RETURN u.name LIMIT 10", // keyword as STRING data
    "MATCH (u:User {id:$id})-[:AUTHORED]->(p:Post) RETURN p.content, p.indexed_at LIMIT 10",
    "MATCH (a:User)-[:FOLLOWS*1..3]->(b:User) RETURN a.name, b.name LIMIT 25",
    "MATCH (u:User) WHERE u.bio IS NOT NULL AND u.name STARTS WITH 'a' RETURN u LIMIT 20",
    "MATCH (u:User) RETURN u.name UNION MATCH (p:Post) RETURN p.id AS name",
    "UNWIND [1,2,3] AS x RETURN x",
    "MATCH (u:User) WITH u WHERE u.indexed_at > $since RETURN u.name LIMIT 50",
    "OPTIONAL MATCH (u:User)-[t:TAGGED]->(p) RETURN u.id, collect(t.label) AS tags LIMIT 25",
    "MATCH (n:User) WHERE n.uri STARTS WITH 'http://x//y' RETURN n LIMIT 5", // // inside string
    "RETURN datetime(), timestamp(), toLower('X')",                          // bare builtin functions
    "MATCH (n:User) RETURN n{.name, .bio} LIMIT 5",                          // map projection
    "MATCH (n:User) RETURN n{.*} LIMIT 5",                                   // map projection wildcard
    "MATCH (n:User) RETURN n.a.b LIMIT 5",                                   // nested property access
];

#[test]
fn rejects_with_expected_reason() {
    let s = sanitizer();
    for (cypher, expected) in REJECT {
        match s.sanitize(cypher) {
            Ok(_) => panic!("expected rejection ({expected:?}) but accepted: {cypher:?}"),
            Err(e) => assert_eq!(
                e.reason(),
                *expected,
                "wrong reason for {cypher:?}: got {:?}, want {expected:?}",
                e.reason()
            ),
        }
    }
}

#[test]
fn accepts_valid_read_queries() {
    let s = sanitizer();
    for cypher in ACCEPT {
        assert!(
            s.sanitize(cypher).is_ok(),
            "expected acceptance but rejected: {cypher:?} ({:?})",
            s.sanitize(cypher).err()
        );
    }
}
