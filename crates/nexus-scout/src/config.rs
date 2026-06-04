//! Gateway configuration.
//!
//! [`Config`] is constructed through a single populator, [`ConfigBuilder`], so
//! there is exactly one list of defaults and one precedence chain. Environment
//! variables and CLI flags both feed the builder; neither owns precedence.

use std::time::Duration;

use cypher_guard::Limits as GuardLimits;

use crate::error::Error;

/// A secret string (e.g. a database password) that never appears in logs.
///
/// `Debug` and `Display` are redacted; the cleartext is reachable only via
/// [`Secret::expose_secret`]. The type is not `Serialize`.
#[derive(Clone, PartialEq, Eq)]
pub struct Secret(String);

impl Secret {
    /// Wraps a cleartext secret.
    #[must_use]
    pub fn new(value: impl Into<String>) -> Self {
        Self(value.into())
    }

    /// Returns the cleartext secret. Call this only at the point of use.
    #[must_use]
    pub fn expose_secret(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Secret(\"***\")")
    }
}

impl std::fmt::Display for Secret {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("***")
    }
}

/// Guardrail and resource limits for the gateway, mirroring the spec §8
/// environment variables. Each value is a denial-of-service or correctness bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// Applied when the caller requests no `LIMIT`.
    pub default_limit: u32,
    /// Ceiling on rows *returned* (not on server-side work) regardless of any
    /// query `LIMIT`.
    pub max_result_rows: u32,
    /// Hard ceiling on the serialized size of the returned rows: a row that would
    /// push the response past it is dropped (and the result flagged truncated), so
    /// the response is never larger than this. The server-side transaction memory
    /// limit bounds what Neo4j materializes; this bounds what is returned.
    pub max_result_bytes: usize,
    pub max_param_count: usize,
    /// Serialized byte size across all parameters.
    pub max_param_bytes: usize,
    /// Nesting depth of any parameter value.
    pub max_param_depth: usize,
    pub guard: GuardLimits,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            default_limit: 25,
            max_result_rows: 100,
            max_result_bytes: 1024 * 1024,
            max_param_count: 32,
            max_param_bytes: 8 * 1024,
            max_param_depth: 8,
            guard: GuardLimits::default(),
        }
    }
}

/// Fully-resolved gateway configuration.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub struct Config {
    pub neo4j_uri: String,
    /// The read-only reader role in production.
    pub neo4j_user: String,
    pub neo4j_password: Secret,
    /// Client-side liveness timeout for a query.
    pub query_timeout: Duration,
    pub limits: Limits,
}

impl Config {
    /// Starts building a configuration from the built-in defaults.
    #[must_use]
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder::default()
    }
}

/// The sole populator for [`Config`].
///
/// Defaults live here and nowhere else. [`ConfigBuilder::apply_env`] layers
/// environment variables over the defaults; CLI flags should be applied after
/// that so the final precedence is CLI > env > default.
#[derive(Debug, Clone)]
pub struct ConfigBuilder {
    neo4j_uri: String,
    neo4j_user: String,
    neo4j_password: Secret,
    query_timeout: Duration,
    limits: Limits,
    max_query_length: usize,
    max_path_depth: u32,
}

impl Default for ConfigBuilder {
    fn default() -> Self {
        let guard = GuardLimits::default();
        Self {
            neo4j_uri: "bolt://localhost:7687".to_owned(),
            neo4j_user: "neo4j".to_owned(),
            neo4j_password: Secret::new(String::new()),
            query_timeout: Duration::from_secs(10),
            limits: Limits::default(),
            max_query_length: guard.max_query_length,
            max_path_depth: guard.max_path_depth,
        }
    }
}

impl ConfigBuilder {
    /// Sets the Neo4j Bolt URI.
    #[must_use]
    pub fn neo4j_uri(mut self, uri: impl Into<String>) -> Self {
        self.neo4j_uri = uri.into();
        self
    }

    /// Sets the Neo4j user.
    #[must_use]
    pub fn neo4j_user(mut self, user: impl Into<String>) -> Self {
        self.neo4j_user = user.into();
        self
    }

    /// Sets the Neo4j password.
    #[must_use]
    pub fn neo4j_password(mut self, password: impl Into<String>) -> Self {
        self.neo4j_password = Secret::new(password.into());
        self
    }

    /// Layers environment variables over the current values.
    ///
    /// # Errors
    ///
    /// Returns [`Error`] if a numeric variable is present but unparseable.
    pub fn apply_env(mut self) -> Result<Self, Error> {
        use std::env::var;

        if let Ok(v) = var("NEO4J_URI") {
            self.neo4j_uri = v;
        }
        if let Ok(v) = var("NEO4J_USER") {
            self.neo4j_user = v;
        }
        if let Ok(v) = var("NEO4J_PASSWORD") {
            self.neo4j_password = Secret::new(v);
        }
        parse_env_into("DEFAULT_LIMIT", &mut self.limits.default_limit)?;
        parse_env_into("MAX_RESULT_ROWS", &mut self.limits.max_result_rows)?;
        parse_env_into("MAX_RESULT_BYTES", &mut self.limits.max_result_bytes)?;
        parse_env_into("MAX_PARAM_COUNT", &mut self.limits.max_param_count)?;
        parse_env_into("MAX_PARAM_BYTES", &mut self.limits.max_param_bytes)?;
        parse_env_into("MAX_PARAM_DEPTH", &mut self.limits.max_param_depth)?;
        parse_env_into("MAX_QUERY_LENGTH", &mut self.max_query_length)?;
        parse_env_into("MAX_PATH_DEPTH", &mut self.max_path_depth)?;
        if let Some(ms) = parse_env::<u64>("QUERY_TIMEOUT_MS")? {
            self.query_timeout = Duration::from_millis(ms);
        }
        Ok(self)
    }

    /// Overrides the Neo4j URI if `value` is `Some` (CLI precedence helper).
    #[must_use]
    pub fn maybe_neo4j_uri(mut self, value: Option<String>) -> Self {
        if let Some(v) = value {
            self.neo4j_uri = v;
        }
        self
    }

    /// Finalizes the configuration.
    #[must_use]
    pub fn build(mut self) -> Config {
        self.limits.guard = GuardLimits::new(self.max_query_length, self.max_path_depth);
        Config {
            neo4j_uri: self.neo4j_uri,
            neo4j_user: self.neo4j_user,
            neo4j_password: self.neo4j_password,
            query_timeout: self.query_timeout,
            limits: self.limits,
        }
    }
}

fn parse_env<T: std::str::FromStr>(key: &str) -> Result<Option<T>, Error>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match std::env::var(key) {
        Ok(v) => v.parse::<T>().map(Some).map_err(Error::internal),
        Err(_) => Ok(None),
    }
}

fn parse_env_into<T: std::str::FromStr>(key: &str, slot: &mut T) -> Result<(), Error>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    if let Some(v) = parse_env::<T>(key)? {
        *slot = v;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn secret_is_redacted_in_debug_and_display() {
        let s = Secret::new("hunter2");
        assert_eq!(format!("{s:?}"), "Secret(\"***\")");
        assert_eq!(format!("{s}"), "***");
        assert!(!format!("{s:?}").contains("hunter2"));
        assert_eq!(s.expose_secret(), "hunter2");
    }

    #[test]
    fn config_debug_never_leaks_password() {
        let cfg = Config::builder().neo4j_password("topsecret").build();
        assert!(!format!("{cfg:?}").contains("topsecret"));
    }

    #[test]
    fn builder_defaults_match_spec() {
        let cfg = Config::builder().build();
        assert_eq!(cfg.limits.default_limit, 25);
        assert_eq!(cfg.limits.max_result_rows, 100);
        assert_eq!(cfg.query_timeout, Duration::from_secs(10));
        assert_eq!(cfg.limits.guard.max_query_length, 2000);
        assert_eq!(cfg.limits.guard.max_path_depth, 5);
    }
}
