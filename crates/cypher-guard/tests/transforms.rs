//! Path-bounding transform assertions: accepted queries with unbounded or
//! over-deep variable-length paths must come back rewritten to be bounded, and
//! `*` outside relationship brackets must never be treated as a path.

use cypher_guard::{Limits, Sanitizer};

fn out(cypher: &str) -> String {
    Sanitizer::new(Limits::default())
        .sanitize(cypher)
        .expect("should accept")
        .cypher()
        .to_owned()
}

#[test]
fn unbounded_star_is_bounded() {
    assert_eq!(
        out("MATCH (a)-[:R*]->(b) RETURN a, b"),
        "MATCH (a)-[:R*1..5]->(b) RETURN a, b"
    );
}

#[test]
fn open_upper_bound_is_capped() {
    assert_eq!(
        out("MATCH (a)-[:R*2..]->(b) RETURN a, b"),
        "MATCH (a)-[:R*2..5]->(b) RETURN a, b"
    );
}

#[test]
fn over_deep_upper_bound_is_capped() {
    assert_eq!(
        out("MATCH (a)-[:R*1..50]->(b) RETURN a, b"),
        "MATCH (a)-[:R*1..5]->(b) RETURN a, b"
    );
}

#[test]
fn within_bounds_is_unchanged() {
    let q = "MATCH (a)-[:R*1..3]->(b) RETURN a, b";
    assert_eq!(out(q), q);
}

#[test]
fn open_lower_bound_is_capped() {
    assert_eq!(
        out("MATCH (a)-[:R*..9]->(b) RETURN a, b"),
        "MATCH (a)-[:R*1..5]->(b) RETURN a, b"
    );
}

#[test]
fn star_outside_brackets_is_not_a_path() {
    // count(*) and map-projection wildcard must survive untouched.
    let q = "MATCH (n) RETURN count(*) AS c";
    assert_eq!(out(q), q);
    let p = "MATCH (n:User) RETURN n{.*} LIMIT 5";
    assert_eq!(out(p), p);
}

#[test]
fn multiplication_inside_list_brackets_is_not_a_path() {
    // `*` inside a list/comprehension `[ ... ]` is multiplication, not a
    // variable-length path, and must not be rewritten.
    let q = "MATCH (n) RETURN [x IN range(1,3) | x * 2] AS doubled LIMIT 5";
    assert_eq!(out(q), q);
    let p = "MATCH (n) WHERE n.v IN [a * 2, b * 3] RETURN n LIMIT 5";
    assert_eq!(out(p), p);
}

#[test]
fn relationship_path_bounded_while_list_multiply_preserved() {
    // A real variable-length relationship path and a list-literal multiplication
    // in the same query: bound the former, leave the latter alone.
    assert_eq!(
        out("MATCH (a)-[:R*]->(b) RETURN [x IN range(1,3) | x * 2] AS d"),
        "MATCH (a)-[:R*1..5]->(b) RETURN [x IN range(1,3) | x * 2] AS d"
    );
}

#[test]
fn multiple_paths_all_bounded() {
    assert_eq!(
        out("MATCH (a)-[:R*]->(b)-[:S*3..]->(c) RETURN a, b, c"),
        "MATCH (a)-[:R*1..5]->(b)-[:S*3..5]->(c) RETURN a, b, c"
    );
}

#[test]
fn left_facing_and_chained_relationship_paths_are_bounded() {
    // `<-[` (incoming) and a mid-chain `)-[` both tag as relationship brackets.
    assert_eq!(
        out("MATCH (a)<-[:R*]-(b) RETURN a, b"),
        "MATCH (a)<-[:R*1..5]-(b) RETURN a, b"
    );
    assert_eq!(
        out("MATCH (a)-[:R]->(b)-[:S*]->(c) RETURN a"),
        "MATCH (a)-[:R]->(b)-[:S*1..5]->(c) RETURN a"
    );
}

#[test]
fn arithmetic_minus_before_list_is_not_a_relationship_bracket() {
    // `<expr> - [ ... * ... ]` is subtraction then a list literal; the `*` is
    // multiplication and must not be rewritten as a variable-length path.
    assert_eq!(
        out("RETURN 3 - [x IN range(1,3) | x * 99] AS y"),
        "RETURN 3 - [x IN range(1,3) | x * 99] AS y"
    );
    assert_eq!(
        out("WITH 1 AS a RETURN a-[2*99] AS y"),
        "WITH 1 AS a RETURN a-[2*99] AS y"
    );
}
