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
/// [`Secret::expose_secret`]. The type is not `Serialize`, and intentionally not
/// `PartialEq`/`Eq` (so it can't be compared in non-constant time). The
/// requirement this type meets is log/serialization redaction; it deliberately
/// does **not** zeroize its buffer on drop (the cleartext is handed to the driver
/// anyway), so it is not a defense against memory disclosure.
#[derive(Clone)]
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
    /// the response is never larger than this. Note this bounds the *returned*
    /// size, not peak memory: up to the row budget's worth of rows may be
    /// materialized and serialized before the cap engages. The server-side
    /// transaction memory limit is what bounds what Neo4j itself materializes.
    pub max_result_bytes: usize,
    /// Maximum number of top-level parameters in a request.
    pub max_param_count: usize,
    /// Serialized byte size across all parameters.
    pub max_param_bytes: usize,
    /// Nesting depth of any parameter value.
    pub max_param_depth: usize,
    /// Sanitizer guardrail limits (query length, variable-length path depth).
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
    /// The Neo4j Bolt URI to connect to.
    pub neo4j_uri: String,
    /// The read-only reader role in production.
    pub neo4j_user: String,
    /// The Neo4j password (redacted in logs; see [`Secret`]).
    pub neo4j_password: Secret,
    /// Client-side liveness timeout for a query.
    pub query_timeout: Duration,
    /// Guardrail and resource limits.
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
/// environment variables over the defaults. The only CLI override is the Neo4j
/// URI ([`ConfigBuilder::maybe_neo4j_uri`]), applied after env so the URI
/// precedence is CLI > env > default; every other setting is env-or-default.
#[derive(Debug, Clone)]
pub struct ConfigBuilder(Config);

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self(Config {
            neo4j_uri: "bolt://localhost:7687".to_owned(),
            neo4j_user: "neo4j".to_owned(),
            neo4j_password: Secret::new(String::new()),
            query_timeout: Duration::from_secs(10),
            limits: Limits::default(),
        })
    }
}

impl ConfigBuilder {
    /// Sets the Neo4j Bolt URI.
    #[must_use]
    pub fn neo4j_uri(mut self, uri: impl Into<String>) -> Self {
        self.0.neo4j_uri = uri.into();
        self
    }

    /// Sets the Neo4j user.
    #[must_use]
    pub fn neo4j_user(mut self, user: impl Into<String>) -> Self {
        self.0.neo4j_user = user.into();
        self
    }

    /// Sets the Neo4j password.
    #[must_use]
    pub fn neo4j_password(mut self, password: impl Into<String>) -> Self {
        self.0.neo4j_password = Secret::new(password.into());
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
            self.0.neo4j_uri = v;
        }
        if let Ok(v) = var("NEO4J_USER") {
            self.0.neo4j_user = v;
        }
        if let Ok(v) = var("NEO4J_PASSWORD") {
            self.0.neo4j_password = Secret::new(v);
        }
        let limits = &mut self.0.limits;
        parse_env_into("DEFAULT_LIMIT", &mut limits.default_limit)?;
        parse_env_into("MAX_RESULT_ROWS", &mut limits.max_result_rows)?;
        parse_env_into("MAX_RESULT_BYTES", &mut limits.max_result_bytes)?;
        parse_env_into("MAX_PARAM_COUNT", &mut limits.max_param_count)?;
        parse_env_into("MAX_PARAM_BYTES", &mut limits.max_param_bytes)?;
        parse_env_into("MAX_PARAM_DEPTH", &mut limits.max_param_depth)?;
        parse_env_into("MAX_QUERY_LENGTH", &mut limits.guard.max_query_length)?;
        parse_env_into("MAX_PATH_DEPTH", &mut limits.guard.max_path_depth)?;
        if let Some(ms) = parse_env::<u64>("QUERY_TIMEOUT_MS")? {
            self.0.query_timeout = Duration::from_millis(ms);
        }
        Ok(self)
    }

    /// Overrides the Neo4j URI if `value` is `Some` (CLI precedence helper).
    #[must_use]
    pub fn maybe_neo4j_uri(mut self, value: Option<String>) -> Self {
        if let Some(v) = value {
            self.0.neo4j_uri = v;
        }
        self
    }

    /// Finalizes the configuration.
    #[must_use]
    pub fn build(self) -> Config {
        self.0
    }
}

fn parse_env<T: std::str::FromStr>(key: &'static str) -> Result<Option<T>, Error>
where
    T::Err: std::error::Error + Send + Sync + 'static,
{
    match std::env::var(key) {
        // An explicitly empty value (`KEY=`) means "use the default" — matching
        // twelve-factor convention and the string vars above — rather than a hard
        // parse error that would abort startup.
        Ok(v) if v.trim().is_empty() => Ok(None),
        Ok(v) => v.parse::<T>().map(Some).map_err(|e| Error::bad_config(key, e)),
        Err(_) => Ok(None),
    }
}

fn parse_env_into<T: std::str::FromStr>(key: &'static str, slot: &mut T) -> Result<(), Error>
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
        // The guard defaults are owned by cypher-guard; assert the builder
        // propagates them rather than re-typing the spec literals (which would
        // duplicate the source of truth across crates).
        assert_eq!(cfg.limits.guard, GuardLimits::default());
    }

    #[test]
    fn env_example_documents_the_actual_defaults() {
        // Lock the documented `.env.example` values to the canonical defaults so
        // the doc copy cannot silently drift from Limits/builder defaults.
        let example = include_str!("../../../.env.example");
        let value_of = |key: &str| -> String {
            example
                .lines()
                .map(str::trim)
                .find_map(|l| l.strip_prefix(&format!("{key}=")))
                .unwrap_or_else(|| panic!(".env.example is missing {key}"))
                .to_owned()
        };
        let l = Limits::default();
        assert_eq!(value_of("DEFAULT_LIMIT"), l.default_limit.to_string());
        assert_eq!(value_of("MAX_RESULT_ROWS"), l.max_result_rows.to_string());
        assert_eq!(value_of("MAX_RESULT_BYTES"), l.max_result_bytes.to_string());
        assert_eq!(value_of("MAX_PARAM_COUNT"), l.max_param_count.to_string());
        assert_eq!(value_of("MAX_PARAM_BYTES"), l.max_param_bytes.to_string());
        assert_eq!(value_of("MAX_PARAM_DEPTH"), l.max_param_depth.to_string());
        assert_eq!(value_of("MAX_QUERY_LENGTH"), l.guard.max_query_length.to_string());
        assert_eq!(value_of("MAX_PATH_DEPTH"), l.guard.max_path_depth.to_string());
        let timeout_ms = Config::builder().build().query_timeout.as_millis().to_string();
        assert_eq!(value_of("QUERY_TIMEOUT_MS"), timeout_ms);
    }
}
