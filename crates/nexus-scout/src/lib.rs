//! Read-only Cypher query gateway between AI agents and the Pubky social graph.
//!
//! `nexus-scout` accepts a Cypher query, validates it is read-only via
//! [`cypher_guard`], executes it against Neo4j, and returns structured JSON. The
//! gateway is served over a public HTTP API and (optionally) a Model Context
//! Protocol server; every transport shares the same [`Scout`] core so they
//! cannot diverge in validation behavior.
//!
//! # Examples
//!
//! ```no_run
//! # async fn run() -> Result<(), nexus_scout::Error> {
//! use nexus_scout::{Config, Scout};
//! use serde_json::Map;
//!
//! let scout = Scout::connect(Config::builder().apply_env()?.build()).await?;
//! let response = scout.query("MATCH (u:User) RETURN u.name", Map::new(), None).await?;
//! println!("{}", response.count);
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]

mod config;
mod convert;
mod error;
mod executor;
#[cfg(feature = "http")]
mod http;
mod params;
mod response;
mod schema;
#[cfg(feature = "mcp")]
mod server;

#[doc(inline)]
pub use config::{Config, ConfigBuilder, HttpLimits, Limits, Profile, Secret};
#[doc(inline)]
pub use error::{Error, ErrorCode};
#[cfg(feature = "http")]
#[doc(hidden)]
pub use http::router as http_router;
#[cfg(feature = "http")]
#[doc(inline)]
pub use http::serve_http;
#[doc(inline)]
pub use response::{ErrorResponse, QueryResponse};
#[doc(inline)]
pub use schema::{schema, GraphSchema, NodeSchema, RelationshipSchema};
#[cfg(feature = "mcp")]
#[doc(inline)]
pub use server::serve_stdio;

use cypher_guard::Sanitizer;
use executor::Executor;
use serde_json::{Map, Value};
use std::sync::Arc;

/// The shared gateway core: validate, then execute.
///
/// `Scout` is cheaply cloneable (it is `Arc`-backed) so it can be shared across
/// CLI invocations and concurrent MCP tool calls without reconnecting.
#[derive(Clone, Debug)]
pub struct Scout {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    sanitizer: Sanitizer,
    executor: Executor,
    limits: Limits,
}

impl Scout {
    /// Connects to Neo4j and builds the gateway from `config`.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the Neo4j connection cannot be established.
    pub async fn connect(config: Config) -> Result<Self, Error> {
        let limits = config.limits;
        let executor = Executor::connect(&config).await?;
        Ok(Self {
            inner: Arc::new(Inner {
                sanitizer: Sanitizer::new(limits.guard),
                executor,
                limits,
            }),
        })
    }

    /// Validates and executes a read-only Cypher query.
    ///
    /// `requested_limit` overrides the default row budget, still capped at the
    /// configured maximum.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the query is rejected by the sanitizer, the
    /// parameters breach a resource bound, or execution fails or times out.
    pub async fn query(
        &self,
        cypher: &str,
        params: Map<String, Value>,
        requested_limit: Option<u32>,
    ) -> Result<QueryResponse, Error> {
        // The raw query and parameters are never logged: they can carry user
        // content, and the codes/hints below are enough to operate on.
        let outcome = self.run(cypher, params, requested_limit).await;
        match &outcome {
            Ok(r) => tracing::info!(row_count = r.count, truncated = r.truncated, "query ok"),
            Err(e) => tracing::warn!(code = %e.code(), "query failed"),
        }
        outcome
    }

    async fn run(
        &self,
        cypher: &str,
        params: Map<String, Value>,
        requested_limit: Option<u32>,
    ) -> Result<QueryResponse, Error> {
        let sanitized = self.inner.sanitizer.sanitize(cypher)?;
        params::check_params(&params, &self.inner.limits)?;
        self.inner.executor.execute(&sanitized, &params, requested_limit).await
    }

    /// Returns the curated graph schema.
    #[must_use]
    pub fn schema(&self) -> &'static GraphSchema {
        schema::schema()
    }

    /// Returns the names of any server-side Neo4j cost bounds
    /// (`db.transaction.timeout`, transaction memory limits) that are unset or
    /// unbounded — empty means all are configured. Used by the HTTP readiness
    /// gate and the production startup check.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the check cannot run (e.g. insufficient privilege to
    /// read settings); callers treat that as "degraded", not "unbounded".
    pub async fn verify_server_bounds(&self) -> Result<Vec<&'static str>, Error> {
        self.inner.executor.verify_server_bounds().await
    }
}

#[cfg(test)]
mod assertions {
    use super::*;
    use static_assertions::assert_impl_all;

    // The gateway is shared across the CLI and concurrent MCP tool calls on a
    // multi-thread runtime, so it and its error type must be Send + Sync.
    assert_impl_all!(Scout: Send, Sync, Clone);
    assert_impl_all!(Error: Send, Sync);

    // The query future must be Send to be spawned on the runtime.
    fn _assert_query_future_is_send(scout: &Scout) {
        fn is_send<T: Send>(_: T) {}
        is_send(scout.query("RETURN 1", Map::new(), None));
    }
}
