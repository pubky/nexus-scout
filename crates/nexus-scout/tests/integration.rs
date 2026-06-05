//! End-to-end tests against a live Neo4j, gated behind the `integration` feature.
//! Connection details come from the environment (`NEO4J_URI`/`NEO4J_USER`/
//! `NEO4J_PASSWORD` for the gateway; `NEO4J_ADMIN_*` for seed/verify).
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

/// Fetches a single scalar column from the first row of `cypher`.
async fn scalar<T: serde::de::DeserializeOwned>(g: &Graph, cypher: &str, key: &str) -> T {
    let mut rows = g.execute(query(cypher)).await.unwrap();
    let row = rows.next().await.unwrap().unwrap();
    row.get::<T>(key).unwrap()
}

async fn user_count(g: &Graph) -> i64 {
    scalar(g, "MATCH (u:User) RETURN count(u) AS c", "c").await
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
    // A param that looks like Cypher must come back a literal string, proving it was bound, not interpolated.
    let mut params = Map::new();
    params.insert("x".into(), serde_json::json!("1) DETACH DELETE (n) //"));
    let resp = scout
        .query("RETURN $x AS echoed", params, None)
        .await
        .expect("query succeeds");
    assert_eq!(resp.results[0]["echoed"], serde_json::json!("1) DETACH DELETE (n) //"));
}

async fn edition(g: &Graph) -> String {
    scalar(g, "CALL dbms.components() YIELD edition RETURN edition", "edition").await
}

/// Proves defense layer 2 (the read-only DB user) independent of the sanitizer, by
/// sending raw mutations through a reader connection. Layer 2 needs Enterprise
/// RBAC; on Community there is none, so the sanitizer is the sole write guard.
/// Asserts the correct reality for the running edition.
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
        // Community: the DB does not block writes, so layer 2 is unavailable here.
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

/// Proves defense layer 1 (the sanitizer) end to end through `Scout::query`: each
/// mutation must be rejected and leave the graph unchanged. Edition-independent —
/// the sanitizer is the only write guard on Community.
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

/// The curated `get_schema` must cover every node label and relationship type
/// present in the seeded fixture (via live `db.labels()` / `db.relationshipTypes()`);
/// an omission would make an agent write Cypher against unadvertised structure. This
/// guards the curated schema against the *fixture* topology, not the full production
/// graph — broaden `seed` to widen the coverage.
#[tokio::test]
async fn curated_schema_covers_seeded_fixture_topology() {
    let admin = admin_graph().await;
    seed(&admin).await;

    let schema = nexus_scout::schema();
    let known_labels: Vec<&str> = schema.nodes.iter().map(|n| n.label.as_str()).collect();
    let known_rels: Vec<&str> = schema.relationships.iter().map(|r| r.rel_type.as_str()).collect();

    for label in string_column(&admin, "CALL db.labels() YIELD label RETURN label", "label").await {
        assert!(
            known_labels.contains(&label.as_str()),
            "live node label {label:?} is missing from the curated schema"
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
            "live relationship type {rel:?} is missing from the curated schema"
        );
    }
}
