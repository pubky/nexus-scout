//! Read-only Cypher query gateway between AI agents and the Pubky social graph.
//! Validates a query is read-only via [`cypher_guard`], executes it against Neo4j,
//! and returns structured JSON over HTTP and (optionally) MCP, all sharing one
//! [`Scout`] core.
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
pub use http::routers as http_routers;
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

/// The shared gateway core: validate, then execute. Cheaply cloneable (`Arc`-backed).
#[derive(Clone, Debug)]
pub struct Scout {
    inner: Arc<Inner>,
}

#[derive(Debug)]
struct Inner {
    sanitizer: Sanitizer,
    executor: Executor,
    limits: Limits,
    profile: Profile,
}

impl Scout {
    /// Connects to Neo4j and builds the gateway from `config`.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the Neo4j connection cannot be established.
    pub async fn connect(config: Config) -> Result<Self, Error> {
        let limits = config.limits;
        let profile = config.profile;
        let executor = Executor::connect(&config).await?;
        Ok(Self {
            inner: Arc::new(Inner {
                sanitizer: Sanitizer::new(limits.guard),
                executor,
                limits,
                profile,
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
        // Never log the raw query or parameters: they can carry user content.
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
        let mut response = self
            .inner
            .executor
            .execute(&sanitized, &params, requested_limit)
            .await?;
        // Surface every transform to the caller: the sanitizer's rewrites (e.g. a
        // bounded variable-length path) ahead of anything the executor recorded while
        // reading rows. Prepending rather than assigning, so both survive.
        response.notes.splice(0..0, sanitized.notes().iter().cloned());
        Ok(response)
    }

    /// Returns the curated graph schema.
    #[must_use]
    pub fn schema(&self) -> &'static GraphSchema {
        schema::schema()
    }

    /// Returns the per-request limits in force, so a transport can publish the
    /// real configured bounds rather than restating the defaults.
    #[must_use]
    pub fn limits(&self) -> Limits {
        self.inner.limits
    }

    /// Returns the names of any required server-side Neo4j cost bounds that are
    /// unset or unbounded; empty means all configured.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if the check cannot run (callers treat that as "degraded").
    pub(crate) async fn verify_server_bounds(&self) -> Result<Vec<&'static str>, Error> {
        self.inner.executor.verify_server_bounds().await
    }

    /// Enforces the startup cost-bound policy: in [`Profile::Production`] the
    /// gateway fails closed if a required bound is unset or unverifiable; in
    /// development it warns.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] in production when bounds are unset or cannot be verified.
    pub async fn ensure_cost_bounds(&self) -> Result<(), Error> {
        let profile = self.inner.profile;
        let (outcome, detail) = self.cost_bounds_outcome().await;
        if production_aborts(profile, outcome) {
            return Err(Error::internal(format!("refusing to start in production: {detail}")));
        }
        match outcome {
            BoundsOutcome::AllSet => tracing::info!("server-side Neo4j cost bounds verified"),
            BoundsOutcome::SomeUnset => {
                tracing::error!("server-side Neo4j cost bounds are unset or unbounded: {detail}");
            }
            BoundsOutcome::Unverifiable => tracing::warn!("{detail} (degraded check)"),
        }
        Ok(())
    }

    /// Classifies the server-side cost-bound state into a [`BoundsOutcome`] and a
    /// detail string, shared by the startup gate and the `/ready` probe.
    pub(crate) async fn cost_bounds_outcome(&self) -> (BoundsOutcome, String) {
        match self.verify_server_bounds().await {
            Ok(missing) if missing.is_empty() => (BoundsOutcome::AllSet, String::new()),
            Ok(missing) => (
                BoundsOutcome::SomeUnset,
                format!("unset server-side cost bounds: {}", missing.join(", ")),
            ),
            Err(e) => (
                BoundsOutcome::Unverifiable,
                format!("could not verify server-side cost bounds: {e}"),
            ),
        }
    }
}

/// Operator hint for the `/ready` probe when the server-side cost bounds are not set.
pub(crate) const COST_BOUNDS_HINT: &str = "Set db.transaction.timeout and the transaction memory limits in neo4j.conf.";

/// Outcome of verifying the server-side cost bounds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BoundsOutcome {
    /// Every required bound is set.
    AllSet,
    /// At least one required bound is unset or unbounded.
    SomeUnset,
    /// The bounds could not be verified at all (the check itself errored).
    Unverifiable,
}

/// Whether production startup must abort: in production, anything short of
/// [`BoundsOutcome::AllSet`] fails closed.
const fn production_aborts(profile: Profile, outcome: BoundsOutcome) -> bool {
    matches!(profile, Profile::Production) && !matches!(outcome, BoundsOutcome::AllSet)
}

#[cfg(test)]
mod assertions {
    use super::*;
    use static_assertions::assert_impl_all;

    // Scout/Error must be Send+Sync (shared across the multi-thread runtime).
    assert_impl_all!(Scout: Send, Sync, Clone);
    assert_impl_all!(Error: Send, Sync);

    // The query future must be Send to be spawned.
    fn _assert_query_future_is_send(scout: &Scout) {
        fn is_send<T: Send>(_: T) {}
        is_send(scout.query("RETURN 1", Map::new(), None));
    }
}

#[cfg(test)]
mod policy_tests {
    use super::{production_aborts, BoundsOutcome, Profile};

    #[test]
    fn production_fails_closed_unless_all_bounds_are_set() {
        assert!(
            production_aborts(Profile::Production, BoundsOutcome::SomeUnset),
            "unset bounds"
        );
        assert!(
            production_aborts(Profile::Production, BoundsOutcome::Unverifiable),
            "verify errored"
        );
        assert!(
            !production_aborts(Profile::Production, BoundsOutcome::AllSet),
            "all set"
        );
        for outcome in [
            BoundsOutcome::AllSet,
            BoundsOutcome::SomeUnset,
            BoundsOutcome::Unverifiable,
        ] {
            assert!(!production_aborts(Profile::Development, outcome), "dev {outcome:?}");
        }
    }
}
