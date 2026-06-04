//! Wire response types.
//!
//! Defined in [`nexus_scout_types`] (shared with the `scout` client) and
//! re-exported here so the gateway and its clients use one definition and the
//! gateway's public API is unchanged.

pub use nexus_scout_types::{ErrorResponse, QueryResponse};
