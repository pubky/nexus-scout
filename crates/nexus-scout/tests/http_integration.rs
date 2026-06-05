//! HTTP gateway tests against a live Neo4j, driven with `oneshot` (no socket).
//! Gated behind `integration` + `http`. The key case is write-rejection parity:
//! writes must be refused at the HTTP boundary exactly as on the CLI/MCP paths.
#![cfg(all(feature = "integration", feature = "http"))]

use axum::body::Body;
use axum::http::{Request, StatusCode};
use axum::Router;
use neo4rs::{query, Graph};
use nexus_scout::{Config, HttpLimits, Scout};
use serde_json::Value;
use tower::ServiceExt;

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

async fn gateway_router() -> Router {
    let config = Config::builder()
        .neo4j_uri(uri())
        .neo4j_user(env_or("NEO4J_USER", "nexus_scout_reader"))
        .neo4j_password(env_or("NEO4J_PASSWORD", "change-me-in-production"))
        .build();
    let scout = Scout::connect(config).await.expect("gateway connects");
    nexus_scout::http_router(scout, HttpLimits::default())
}

async fn seed(admin: &Graph) {
    admin
        .run(query(
            "MERGE (a:User {id:'pk:alice', name:'Alice'})
             MERGE (b:User {id:'pk:bob', name:'Bob'})
             MERGE (b)-[:FOLLOWS]->(a)",
        ))
        .await
        .expect("seed fixture");
}

async fn user_count(admin: &Graph) -> i64 {
    let mut rows = admin
        .execute(query("MATCH (u:User) RETURN count(u) AS c"))
        .await
        .unwrap();
    rows.next().await.unwrap().unwrap().get::<i64>("c").unwrap()
}

async fn label_ids(admin: &Graph, label: &str) -> Vec<String> {
    let mut rows = admin
        .execute(query(&format!("MATCH (n:{label}) RETURN n.id AS id")))
        .await
        .unwrap();
    let mut out = Vec::new();
    while let Some(row) = rows.next().await.unwrap() {
        out.push(row.get::<String>("id").unwrap_or_default());
    }
    out
}

/// Sends a request through the router and returns `(status, parsed JSON body)`.
async fn send(router: &Router, method: &str, path: &str, body: Option<Request<Body>>) -> (StatusCode, Value) {
    let request = body.unwrap_or_else(|| Request::builder().method(method).uri(path).body(Body::empty()).unwrap());
    let response = router.clone().oneshot(request).await.unwrap();
    let status = response.status();
    let bytes = axum::body::to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, serde_json::from_slice::<Value>(&bytes).unwrap_or(Value::Null))
}

fn query_request(cypher: &str) -> Request<Body> {
    let body = serde_json::json!({ "cypher": cypher }).to_string();
    Request::builder()
        .method("POST")
        .uri("/v1/query")
        .header("content-type", "application/json")
        .body(Body::from(body))
        .unwrap()
}

#[tokio::test]
async fn happy_path_query_returns_results() {
    let admin = admin_graph().await;
    seed(&admin).await;
    let router = gateway_router().await;

    let (status, body) = send(
        &router,
        "POST",
        "/v1/query",
        Some(query_request("MATCH (u:User) RETURN u.name AS name ORDER BY name")),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(body["results"].is_array());
    assert!(body["count"].as_u64().unwrap() >= 1);
    assert_eq!(body["truncated"], false);
}

#[tokio::test]
async fn schema_health_and_ready_endpoints() {
    let router = gateway_router().await;

    let (s_status, schema) = send(&router, "GET", "/v1/schema", None).await;
    assert_eq!(s_status, StatusCode::OK);
    assert!(schema["nodes"].is_array());

    let (h_status, _) = send(&router, "GET", "/health", None).await;
    assert_eq!(h_status, StatusCode::OK);

    // Readiness reflects whether the server-side bounds are set: 200 if all are
    // configured, 503 otherwise. Both are valid; assert it answers cleanly.
    let (r_status, _) = send(&router, "GET", "/ready", None).await;
    assert!(
        r_status == StatusCode::OK || r_status == StatusCode::SERVICE_UNAVAILABLE,
        "unexpected /ready status: {r_status}"
    );
}

#[tokio::test]
async fn oversized_body_is_rejected_413() {
    let router = gateway_router().await;
    let body = format!(r#"{{"cypher":"{}"}}"#, "x".repeat(100 * 1024));
    let request = Request::builder()
        .method("POST")
        .uri("/v1/query")
        .header("content-type", "application/json")
        // An explicit content-length over the cap trips the body-limit layer.
        .header("content-length", body.len().to_string())
        .body(Body::from(body))
        .unwrap();
    let (status, _) = send(&router, "POST", "/v1/query", Some(request)).await;
    assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
}

#[tokio::test]
async fn malformed_json_body_is_a_400_envelope() {
    let router = gateway_router().await;
    let request = Request::builder()
        .method("POST")
        .uri("/v1/query")
        .header("content-type", "application/json")
        .body(Body::from("{not json"))
        .unwrap();
    let (status, body) = send(&router, "POST", "/v1/query", Some(request)).await;
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert_eq!(body["error"], "QUERY_REJECTED");
}

/// The parity test: every write rejected at the HTTP boundary, graph unchanged.
#[tokio::test]
async fn writes_are_rejected_through_http_and_graph_is_unchanged() {
    let admin = admin_graph().await;
    seed(&admin).await;
    let before = user_count(&admin).await;
    let router = gateway_router().await;

    let mutations = [
        "CREATE (n:ScoutHttpProbe {id:'x'})",
        "MATCH (u:User) SET u.hacked = true",
        "MATCH (u:User) REMOVE u.name",
        "MERGE (n:ScoutHttpProbe {id:'y'})",
        "MATCH (u:User {id:'pk:alice'}) DETACH DELETE u",
        "CREATE INDEX scout_http_probe IF NOT EXISTS FOR (n:User) ON (n.id)",
        "CALL apoc.create.node(['X'], {}) YIELD node RETURN node",
    ];
    for m in mutations {
        let (status, body) = send(&router, "POST", "/v1/query", Some(query_request(m))).await;
        assert_eq!(status, StatusCode::BAD_REQUEST, "expected 400 for {m:?}");
        assert_eq!(body["error"], "QUERY_REJECTED", "expected QUERY_REJECTED for {m:?}");
    }

    assert_eq!(
        before,
        user_count(&admin).await,
        "a rejected write must not change the graph"
    );
    assert!(
        label_ids(&admin, "ScoutHttpProbe").await.is_empty(),
        "no probe node should have been created"
    );
}
