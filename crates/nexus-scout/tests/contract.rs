//! Wire-contract tests: the JSON nexus-scout emits must match the documented
//! shapes. Comparisons are structural since row column order is sorted for
//! determinism.

use cypher_guard::{Limits, Sanitizer};
use nexus_scout::{schema, Error, ErrorCode, QueryResponse};
use serde_json::{json, Map, Value};

#[test]
fn query_response_shape() {
    let mut row = Map::new();
    row.insert("u.id".into(), json!("pk:abc"));
    row.insert("u.name".into(), json!("Alice"));
    row.insert("followers".into(), json!(142));
    let resp = QueryResponse::new(vec![row], false);

    let v = serde_json::to_value(&resp).unwrap();
    assert_eq!(
        v,
        json!({
            "results": [{"u.id": "pk:abc", "u.name": "Alice", "followers": 142}],
            "count": 1,
            "truncated": false
        })
    );
}

#[test]
fn count_always_equals_results_len() {
    let rows: Vec<Map<String, Value>> = (0..3).map(|_| Map::new()).collect();
    let resp = QueryResponse::new(rows, true);
    let v = serde_json::to_value(&resp).unwrap();
    assert_eq!(v["count"], json!(3));
    assert_eq!(v["results"].as_array().unwrap().len(), 3);
    assert_eq!(v["truncated"], json!(true));
}

#[test]
fn error_response_shape_and_codes() {
    // A real rejection flows from the sanitizer through the gateway error.
    let err = Sanitizer::new(Limits::default())
        .sanitize("MATCH (n) DETACH DELETE n")
        .map_err(Error::from)
        .unwrap_err();
    let resp = err.to_response();
    let v = serde_json::to_value(&resp).unwrap();
    assert_eq!(v["error"], json!("QUERY_REJECTED"));
    assert!(v["message"].is_string());
    assert!(v["hint"].as_str().unwrap().contains("read-only"));
}

#[test]
fn schema_matches_golden_and_spec_shape() {
    let v = serde_json::to_value(schema()).unwrap();

    // Node properties are objects; relationship properties are bare type strings (deliberate asymmetry).
    let user = v["nodes"]
        .as_array()
        .unwrap()
        .iter()
        .find(|n| n["label"] == json!("User"))
        .unwrap();
    assert_eq!(user["properties"]["id"]["type"], json!("string"));
    assert_eq!(user["properties"]["id"]["unique"], json!(true));

    let follows = v["relationships"]
        .as_array()
        .unwrap()
        .iter()
        .find(|r| r["type"] == json!("FOLLOWS"))
        .unwrap();
    assert_eq!(follows["from"], json!("User"));
    assert_eq!(follows["properties"]["indexed_at"], json!("integer"));

    assert!(!v["examples"].as_array().unwrap().is_empty());
}

#[test]
fn schema_examples_pass_the_sanitizer() {
    // We must never ship example queries our own gateway would reject.
    let s = Sanitizer::new(Limits::default());
    for ex in &schema().examples {
        assert!(s.sanitize(ex).is_ok(), "schema example rejected by sanitizer: {ex:?}");
    }
}

/// Every Cypher recipe in the usage guide must survive the sanitizer. The guide is
/// served verbatim at `/llms.txt`, so a recipe our own gateway rejects is a
/// published instruction that cannot work, and the caller has no way to tell that
/// the fault is ours.
#[test]
fn served_guide_recipes_pass_the_sanitizer() {
    const GUIDE: &str = include_str!("../../../SKILL.md");
    let s = Sanitizer::new(Limits::default());

    let mut checked = 0;
    for block in GUIDE.split("```cypher").skip(1) {
        let query = block.split("```").next().unwrap_or(block).trim();
        if let Err(e) = s.sanitize(query) {
            panic!("guide recipe rejected as {:?}: {query}", e.reason());
        }
        checked += 1;
    }
    // Guards against the split silently finding nothing if the fences are renamed.
    assert!(checked >= 8, "expected the guide's Cypher recipes, found {checked}");
}

#[test]
fn error_codes_serialize_screaming_snake() {
    let cases = [
        (ErrorCode::QueryRejected, "QUERY_REJECTED"),
        (ErrorCode::QueryTimeout, "QUERY_TIMEOUT"),
        (ErrorCode::QuerySyntaxError, "QUERY_SYNTAX_ERROR"),
        (ErrorCode::RateLimited, "RATE_LIMITED"),
        (ErrorCode::InternalError, "INTERNAL_ERROR"),
    ];
    for (code, expected) in cases {
        assert_eq!(serde_json::to_value(code).unwrap(), json!(expected));
        // Serialize and Display share one `as_str` source; assert they agree.
        assert_eq!(code.to_string(), expected);
    }
}
