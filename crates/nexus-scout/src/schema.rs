//! The curated graph schema returned by `get_schema` (spec §5.2).
//!
//! The schema is hand-authored rather than introspected at runtime (introspection
//! needs `CALL db.schema.*`, which the sanitizer denies) or derived from the
//! `nexus-common` model structs (whose serialization shape intentionally differs
//! from the on-graph shape, e.g. `links` is stored as a JSON-encoded string).
//!
//! The wire shape is asymmetric on purpose, matching the spec: node properties
//! are objects (`{type, description?, unique?}`) while relationship properties
//! are bare type strings. A contract test pins the serialized output to
//! `docs/schema.golden.json`. When the on-graph model changes, the integration
//! `schema_matches_live_graph` test (which diffs against `CALL db.labels()` /
//! `db.relationshipTypes()`) is what catches drift; keep this in sync with the
//! `nexus-common` models under `models/{user,post,tag,file}/details.rs`.

use serde::Serialize;

/// The full graph schema: node types, relationship types, and example queries.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct GraphSchema {
    pub nodes: Vec<NodeSchema>,
    pub relationships: Vec<RelationshipSchema>,
    pub examples: Vec<String>,
}

/// A node label and its properties. Each property maps to a descriptor object
/// (`{type, description?, unique?}`).
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct NodeSchema {
    pub label: String,
    pub properties: serde_json::Map<String, serde_json::Value>,
}

/// A relationship type, its direction (`from`/`to` labels), and its properties.
/// Unlike node properties, each maps to a bare type string (the spec's
/// deliberate asymmetry). Serializes `rel_type` as `type`.
#[derive(Debug, Clone, Serialize)]
#[non_exhaustive]
pub struct RelationshipSchema {
    #[serde(rename = "type")]
    pub rel_type: String,
    pub from: String,
    pub to: String,
    pub properties: serde_json::Map<String, serde_json::Value>,
}

/// Returns the curated graph schema.
///
/// # Panics
///
/// Panics if the embedded `schema.golden.json` is malformed. The file is
/// compiled into the binary and covered by a contract test, so this cannot
/// happen at runtime in a built artifact.
#[must_use]
pub fn schema() -> GraphSchema {
    let raw = include_str!("../../../docs/schema.golden.json");
    // The golden file is the single source of truth for the wire shape; parsing
    // it here keeps the Rust types and the published contract from drifting.
    serde_json::from_str::<GoldenSchema>(raw)
        .expect("schema.golden.json is valid and matches the schema types")
        .into()
}

#[derive(serde::Deserialize)]
struct GoldenSchema {
    nodes: Vec<GoldenNode>,
    relationships: Vec<GoldenRel>,
    examples: Vec<String>,
}

#[derive(serde::Deserialize)]
struct GoldenNode {
    label: String,
    properties: serde_json::Map<String, serde_json::Value>,
}

#[derive(serde::Deserialize)]
struct GoldenRel {
    #[serde(rename = "type")]
    rel_type: String,
    from: String,
    to: String,
    properties: serde_json::Map<String, serde_json::Value>,
}

impl From<GoldenSchema> for GraphSchema {
    fn from(g: GoldenSchema) -> Self {
        Self {
            nodes: g
                .nodes
                .into_iter()
                .map(|n| NodeSchema {
                    label: n.label,
                    properties: n.properties,
                })
                .collect(),
            relationships: g
                .relationships
                .into_iter()
                .map(|r| RelationshipSchema {
                    rel_type: r.rel_type,
                    from: r.from,
                    to: r.to,
                    properties: r.properties,
                })
                .collect(),
            examples: g.examples,
        }
    }
}
