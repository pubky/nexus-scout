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

/// F2: a query's own `LIMIT` drives the row budget without a `--limit` flag. The
/// default budget is 25, so a `LIMIT 30` proves the in-query limit is honored
/// (and `UNWIND range` needs no seed data).
#[tokio::test]
async fn query_limit_is_honored_above_the_default() {
    let scout = gateway().await;
    let resp = scout
        .query("UNWIND range(1, 100) AS i RETURN i LIMIT 30", Map::new(), None)
        .await
        .expect("query succeeds");
    assert_eq!(
        resp.results.len(),
        30,
        "the query's own LIMIT should set the budget, not the default 25"
    );
}

/// F2: an explicit requested limit still overrides the in-query `LIMIT`.
#[tokio::test]
async fn requested_limit_overrides_the_query_limit() {
    let scout = gateway().await;
    let resp = scout
        .query("UNWIND range(1, 100) AS i RETURN i LIMIT 30", Map::new(), Some(10))
        .await
        .expect("query succeeds");
    assert_eq!(
        resp.results.len(),
        10,
        "the requested limit must override the in-query LIMIT"
    );
}

/// F4: an over-deep variable-length path is bounded to `*1..5` and the rewrite is
/// surfaced to the caller via `notes` (not silent).
#[tokio::test]
async fn bounded_path_is_surfaced_in_notes() {
    let admin = admin_graph().await;
    seed(&admin).await;

    let scout = gateway().await;
    let resp = scout
        .query(
            "MATCH p = (a:User)-[:FOLLOWS*1..10]->(b:User) RETURN length(p) AS len",
            Map::new(),
            None,
        )
        .await
        .expect("query succeeds");
    assert!(
        resp.notes.iter().any(|n| n.contains("bounded to '*1..5'")),
        "a *1..10 path should be bounded and surfaced via notes, got {:?}",
        resp.notes
    );
}

/// A sanitizer note and an executor note must both survive onto one response. They
/// are produced in different crates and merged in `Scout::run`, which used to
/// *assign* the sanitizer's notes over whatever the executor had recorded, silently
/// dropping the row-limit disclosure whenever a query also triggered a rewrite.
#[tokio::test]
async fn sanitizer_and_row_limit_notes_coexist() {
    let admin = admin_graph().await;
    seed(&admin).await;

    let scout = gateway().await;
    // Bounded path (sanitizer note) and no LIMIT over more rows than the default
    // (executor note), in one query.
    let resp = scout
        .query(
            "MATCH (a:User)-[:FOLLOWS*1..10]->(b:User) UNWIND range(1,100) AS i RETURN b.id AS id, i",
            Map::new(),
            None,
        )
        .await
        .expect("query succeeds");

    assert!(resp.truncated, "expected the default row budget to bite: {resp:?}");
    assert!(
        resp.notes.iter().any(|n| n.contains("bounded to '*1..5'")),
        "sanitizer note missing: {:?}",
        resp.notes
    );
    assert!(
        resp.notes.iter().any(|n| n.contains("no LIMIT in the query")),
        "row-limit note missing: {:?}",
        resp.notes
    );
}

/// F1: a whole-node return is properties-only, with no synthetic `_id`/`_labels`
/// leaked into the row.
#[tokio::test]
async fn whole_node_return_is_properties_only() {
    let admin = admin_graph().await;
    seed(&admin).await;

    let scout = gateway().await;
    let resp = scout
        .query("MATCH (u:User {id:'pk:alice'}) RETURN u LIMIT 1", Map::new(), None)
        .await
        .expect("query succeeds");
    let node = resp.results[0]["u"].as_object().expect("node is a JSON object");
    assert!(node.contains_key("id"), "real properties are present: {node:?}");
    for synthetic in ["_id", "_labels", "_type", "_start", "_end"] {
        assert!(
            !node.contains_key(synthetic),
            "no synthetic {synthetic:?} key: {node:?}"
        );
    }
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
