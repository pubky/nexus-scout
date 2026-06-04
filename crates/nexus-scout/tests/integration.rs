//! End-to-end tests against a live Neo4j. Gated behind the `integration` feature
//! so the default test run stays database-free.
//!
//! Run with a database up (see `docker/docker-compose.yml`) and the reader user
//! provisioned (`scripts/neo4j_reader_setup_community.cypher`, or the
//! `_enterprise` variant on Enterprise):
//!
//! ```text
//! cargo nextest run -p nexus-scout --features integration
//! ```
//!
//! Connection details come from the environment (`NEO4J_URI` / `NEO4J_USER` /
//! `NEO4J_PASSWORD` for the gateway; `NEO4J_ADMIN_USER` / `NEO4J_ADMIN_PASSWORD`
//! for the privileged seed/verify steps).
#![cfg(feature = "integration")]

use neo4rs::{query, Graph};
use nexus_scout::{Config, Scout};
use serde_json::Map;

fn env_or(key: &str, default: &str) -> String {
    std::env::var(key).unwrap_or_else(|_| default.to_owned())
}

fn uri() -> String {
    env_or("NEO4J_URI", "bolt://localhost:7687")
}

async fn admin_graph() -> Graph {
    Graph::new(
        uri(),
        env_or("NEO4J_ADMIN_USER", "neo4j"),
        env_or("NEO4J_ADMIN_PASSWORD", "testtest12"),
    )
    .await
    .expect("admin connection")
}

async fn reader_graph() -> Graph {
    Graph::new(
        uri(),
        env_or("NEO4J_USER", "nexus_scout_reader"),
        env_or("NEO4J_PASSWORD", "change-me-in-production"),
    )
    .await
    .expect("reader connection")
}

async fn gateway() -> Scout {
    let config = Config::builder()
        .neo4j_uri(uri())
        .neo4j_user(env_or("NEO4J_USER", "nexus_scout_reader"))
        .neo4j_password(env_or("NEO4J_PASSWORD", "change-me-in-production"))
        .build();
    Scout::connect(config).await.expect("gateway connects")
}

async fn seed(admin: &Graph) {
    // Idempotent fixture: a handful of users and follows.
    admin
        .run(query(
            "MERGE (a:User {id:'pk:alice', name:'Alice'})
             MERGE (b:User {id:'pk:bob', name:'Bob'})
             MERGE (c:User {id:'pk:carol', name:'Carol'})
             MERGE (b)-[:FOLLOWS]->(a)
             MERGE (c)-[:FOLLOWS]->(a)",
        ))
        .await
        .expect("seed fixture");
}

async fn string_column(g: &Graph, cypher: &str, key: &str) -> Vec<String> {
    let mut rows = g.execute(query(cypher)).await.unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<String>(key).unwrap());
    }
    out
}

async fn user_count(g: &Graph) -> i64 {
    let mut rows = g.execute(query("MATCH (u:User) RETURN count(u) AS c")).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get::<i64>("c").unwrap()
}

#[tokio::test]
async fn happy_path_returns_rows() {
    let admin = admin_graph().await;
    seed(&admin).await;

    let scout = gateway().await;
    let resp = scout
        .query(
            "MATCH (u:User)<-[f:FOLLOWS]-() RETURN u.name AS name, count(f) AS followers ORDER BY followers DESC",
            Map::new(),
            None,
        )
        .await
        .expect("query succeeds");

    assert!(resp.count >= 1);
    assert_eq!(resp.count, resp.results.len());
    assert!(resp.results.iter().any(|r| r["name"] == "Alice"));
}

#[tokio::test]
async fn row_cap_truncates() {
    let admin = admin_graph().await;
    seed(&admin).await;

    let scout = gateway().await;
    let resp = scout
        .query("MATCH (u:User) RETURN u.id AS id", Map::new(), Some(1))
        .await
        .expect("query succeeds");

    assert_eq!(resp.results.len(), 1);
    assert!(resp.truncated, "result should be flagged truncated at the row cap");
}

#[tokio::test]
async fn params_bind_natively_and_are_inert() {
    let scout = gateway().await;
    // A parameter value that looks like Cypher must come back as a literal
    // string, proving it was bound, not interpolated.
    let mut params = Map::new();
    params.insert("x".into(), serde_json::json!("1) DETACH DELETE (n) //"));
    let resp = scout
        .query("RETURN $x AS echoed", params, None)
        .await
        .expect("query succeeds");
    assert_eq!(resp.results[0]["echoed"], serde_json::json!("1) DETACH DELETE (n) //"));
}

async fn edition(g: &Graph) -> String {
    let mut rows = g
        .execute(query("CALL dbms.components() YIELD edition RETURN edition"))
        .await
        .unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get::<String>("edition").unwrap()
}

/// The refusal matrix proves defense layer 2 (the read-only database user)
/// independent of the sanitizer, by sending raw mutations through a
/// reader-credential connection that bypasses the gateway entirely.
///
/// Layer 2 relies on Neo4j role-based access control (`GRANT ROLE reader` +
/// `DENY WRITE`), which exists only in the **Enterprise** edition. On
/// **Community** there is no RBAC, so the configured user can write and the
/// sanitizer is the sole write guard. The test asserts the correct reality for
/// the running edition rather than a guarantee the edition cannot provide.
#[tokio::test]
async fn reader_role_write_policy_matches_edition() {
    let admin = admin_graph().await;
    seed(&admin).await;
    let before = user_count(&admin).await;
    let edition = edition(&admin).await;

    let reader = reader_graph().await;
    let mutations = [
        "CREATE (n:ScoutRefusalProbe {id:'x'})",
        "MATCH (u:User) SET u.hacked = true",
        "MATCH (u:User) REMOVE u.name",
        "MERGE (n:ScoutRefusalProbe {id:'y'})",
        "MATCH (u:User {id:'pk:alice'}) DETACH DELETE u",
        "CREATE INDEX scout_probe_index IF NOT EXISTS FOR (n:User) ON (n.id)",
    ];

    if edition.eq_ignore_ascii_case("enterprise") {
        for m in mutations {
            assert!(reader.run(query(m)).await.is_err(), "reader role should refuse: {m:?}");
        }
        let after = user_count(&admin).await;
        assert_eq!(before, after, "no write should have taken effect under Enterprise RBAC");
    } else {
        // Community: document that the DB does NOT block writes, so the team is
        // never lulled into believing layer 2 is active here.
        let allowed = reader.run(query("CREATE (n:ScoutRefusalProbe {id:'community'})")).await;
        assert!(
            allowed.is_ok(),
            "on Community the reader user can write (no RBAC); layer 2 is unavailable"
        );
        // Clean up the probe node we just created.
        admin
            .run(query("MATCH (n:ScoutRefusalProbe) DETACH DELETE n"))
            .await
            .unwrap();
    }
}

/// Proves defense layer 1 (the sanitizer) end to end through the public
/// `Scout::query` path, and that a rejected write touches nothing. Unlike the
/// reader-role matrix above, this is edition-independent: the sanitizer is the
/// only write guard present on Community, so this is the test that actually
/// covers the decided deployment. Each mutation must be rejected *and* leave the
/// graph unchanged.
#[tokio::test]
async fn gateway_rejects_writes_and_leaves_graph_unchanged() {
    let admin = admin_graph().await;
    seed(&admin).await;
    let before = user_count(&admin).await;

    let scout = gateway().await;
    let mutations = [
        "CREATE (n:ScoutGatewayProbe {id:'x'})",
        "MATCH (u:User) SET u.hacked = true",
        "MATCH (u:User) REMOVE u.name",
        "MERGE (n:ScoutGatewayProbe {id:'y'})",
        "MATCH (u:User {id:'pk:alice'}) DETACH DELETE u",
        "CREATE INDEX scout_gateway_probe IF NOT EXISTS FOR (n:User) ON (n.id)",
        "CALL apoc.create.node(['X'], {}) YIELD node RETURN node",
        "MATCH (u:User) CALL { WITH u DELETE u } RETURN count(*)",
    ];
    for m in mutations {
        let err = scout
            .query(m, Map::new(), None)
            .await
            .expect_err("the gateway must reject a write");
        assert!(
            err.is_rejected(),
            "expected the sanitizer to reject {m:?}, got code {:?}",
            err.code()
        );
    }

    let after = user_count(&admin).await;
    assert_eq!(before, after, "a rejected write must not change the graph");
    let probes = string_column(&admin, "MATCH (n:ScoutGatewayProbe) RETURN n.id AS id", "id").await;
    assert!(probes.is_empty(), "no probe node should have been created: {probes:?}");
}

/// The curated `get_schema` must not omit any node label or relationship type
/// that actually exists in the graph; a silent omission would make an agent
/// write Cypher against structure the schema never advertised. This is the drift
/// guard ADR-0006 refers to: it compares the hand-authored schema against the
/// live `db.labels()` / `db.relationshipTypes()`.
#[tokio::test]
async fn curated_schema_covers_live_graph_topology() {
    let admin = admin_graph().await;
    seed(&admin).await;

    let schema = nexus_scout::schema();
    let known_labels: Vec<&str> = schema.nodes.iter().map(|n| n.label.as_str()).collect();
    let known_rels: Vec<&str> = schema.relationships.iter().map(|r| r.rel_type.as_str()).collect();

    for label in string_column(&admin, "CALL db.labels() YIELD label RETURN label", "label").await {
        assert!(
            known_labels.contains(&label.as_str()),
            "live node label {label:?} is missing from the curated schema (docs/schema.golden.json)"
        );
    }
    for rel in string_column(
        &admin,
        "CALL db.relationshipTypes() YIELD relationshipType RETURN relationshipType",
        "relationshipType",
    )
    .await
    {
        assert!(
            known_rels.contains(&rel.as_str()),
            "live relationship type {rel:?} is missing from the curated schema (docs/schema.golden.json)"
        );
    }
}
