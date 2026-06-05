//! The curated graph schema returned by `get_schema`. Hand-authored rather than
//! introspected (introspection needs `CALL db.schema.*`, which the sanitizer
//! denies). The wire shape is asymmetric on purpose: node properties are objects
//! (`{type, description?, unique?}`) while relationship properties are bare type
//! strings.

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

/// A node label and its `{type, description?, unique?}` property descriptors.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[non_exhaustive]
pub struct NodeSchema {
    /// The node label.
    pub label: String,
    /// The node's properties, each a `{type, description?, unique?}` descriptor.
    pub properties: serde_json::Map<String, serde_json::Value>,
}

/// A relationship type, its `from`/`to` labels, and its bare-string properties.
/// Serializes `rel_type` as `type`.
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

/// The curated schema, parsed once from the embedded golden file (the single
/// source of truth for the wire shape).
static SCHEMA: LazyLock<GraphSchema> = LazyLock::new(|| {
    let raw = include_str!("../../../docs/schema.golden.json");
    serde_json::from_str(raw).expect("schema.golden.json is valid and matches the schema types")
});

/// Returns the curated graph schema.
///
/// # Panics
///
/// Panics if the embedded golden file is malformed; it is compiled in and
/// contract-tested, so this cannot happen in a built artifact.
#[must_use]
pub fn schema() -> &'static GraphSchema {
    &SCHEMA
}
