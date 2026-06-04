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

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

/// The full graph schema: node types, relationship types, and example queries.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct GraphSchema {
    /// The node types (labels and their properties).
    pub nodes: Vec<NodeSchema>,
    /// The relationship types (direction and properties).
    pub relationships: Vec<RelationshipSchema>,
    /// Example read-only queries an agent can adapt.
    pub examples: Vec<String>,
}

/// A node label and its properties. Each property maps to a descriptor object
/// (`{type, description?, unique?}`).
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NodeSchema {
    /// The node label.
    pub label: String,
    /// The node's properties, each a `{type, description?, unique?}` descriptor.
    pub properties: serde_json::Map<String, serde_json::Value>,
}

/// A relationship type, its direction (`from`/`to` labels), and its properties.
/// Unlike node properties, each maps to a bare type string (the spec's
/// deliberate asymmetry). Serializes `rel_type` as `type`.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct RelationshipSchema {
    /// The relationship type (serialized as `type`).
    #[serde(rename = "type")]
    pub rel_type: String,
    /// The source node label.
    pub from: String,
    /// The target node label.
    pub to: String,
    /// The relationship's properties, each a bare type string.
    pub properties: serde_json::Map<String, serde_json::Value>,
}

/// The curated schema, parsed once from the embedded golden file. The file is
/// the single source of truth for the wire shape, so the Rust types and the
/// published contract cannot drift.
static SCHEMA: LazyLock<GraphSchema> = LazyLock::new(|| {
    let raw = include_str!("../../../docs/schema.golden.json");
    serde_json::from_str(raw).expect("schema.golden.json is valid and matches the schema types")
});

/// Returns the curated graph schema.
///
/// # Panics
///
/// Panics if the embedded `schema.golden.json` is malformed. The file is
/// compiled into the binary and covered by a contract test, so this cannot
/// happen at runtime in a built artifact.
#[must_use]
pub fn schema() -> &'static GraphSchema {
    &SCHEMA
}
