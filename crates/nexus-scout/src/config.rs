//! Gateway configuration. [`Config`] is built through a single populator,
//! [`ConfigBuilder`], so there is one list of defaults and one precedence chain.

use std::net::SocketAddr;
use std::time::Duration;

use cypher_guard::Limits as GuardLimits;

use crate::error::Error;

/// A secret string (e.g. a database password), redacted in `Debug`/`Display`; the
/// cleartext is reachable only via [`Secret::expose_secret`]. Not `Serialize`. It
/// does not zeroize on drop, so it is not a defense against memory disclosure.
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

/// Guardrail and resource limits for the gateway; each is a denial-of-service or
/// correctness bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct Limits {
    /// Applied when the caller requests no `LIMIT`.
    pub default_limit: u32,
    /// Ceiling on rows *returned* (not on server-side work) regardless of any
    /// query `LIMIT`.
    pub max_result_rows: u32,
    /// Cap on the summed serialized size of the returned row payloads: a row that
    /// would push the total past it is dropped (and the result flagged truncated).
    /// Bounds the row payloads, not the full response or peak memory.
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

/// Denial-of-service limits for the public HTTP transport. These bound
/// *aggregate* load (the [`Limits`] above bound a single request); see
/// [`Config::http_limits`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[non_exhaustive]
pub struct HttpLimits {
    /// Maximum request body size; a larger body is rejected with `413`.
    pub max_body_bytes: usize,
    /// Maximum in-flight `/v1/query` requests; excess is shed with `429`.
    pub max_concurrency: usize,
    /// Maximum sustained `/v1/query` requests per second; excess is shed (`429`).
    pub max_rps: u32,
    /// Whole-request wall-clock timeout: a coarse backstop above the per-query
    /// [`Config::query_timeout`].
    pub request_timeout: Duration,
}

impl Default for HttpLimits {
    fn default() -> Self {
        Self {
            max_body_bytes: 64 * 1024,
            max_concurrency: 64,
            max_rps: 50,
            request_timeout: Duration::from_secs(30),
        }
    }
}

/// Deployment profile. [`Profile::Production`] fails closed on misconfiguration
/// (missing Neo4j cost bounds, or plaintext Bolt to a remote host) that is only a
/// warning in development.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Profile {
    /// Local/dev: misconfiguration is logged as a warning.
    #[default]
    Development,
    /// Hosted: misconfiguration is a hard startup error.
    Production,
}

impl std::fmt::Display for Profile {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Development => "development",
            Self::Production => "production",
        })
    }
}

impl std::str::FromStr for Profile {
    type Err = ParseProfileError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        match s.trim().to_ascii_lowercase().as_str() {
            "development" | "dev" => Ok(Self::Development),
            "production" | "prod" => Ok(Self::Production),
            _ => Err(ParseProfileError),
        }
    }
}

/// Error returned when `NEXUS_SCOUT_PROFILE` is not a known profile.
#[derive(Debug)]
pub struct ParseProfileError;

impl std::fmt::Display for ParseProfileError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("expected 'development' or 'production'")
    }
}

impl std::error::Error for ParseProfileError {}

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
    /// Size of the Neo4j connection pool. Defaults to the HTTP concurrency cap so
    /// admission never lets in more requests than the pool can serve; a smaller pool
    /// makes excess admitted requests stall on connection acquire until they time out.
    pub neo4j_max_connections: usize,
    /// Client-side liveness timeout for a query.
    pub query_timeout: Duration,
    /// Guardrail and resource limits.
    pub limits: Limits,
    /// Address the public HTTP gateway binds to. Default `127.0.0.1:8080` (not
    /// directly public); set `HTTP_ADDR=0.0.0.0:8080` behind a reverse proxy.
    pub http_bind: SocketAddr,
    /// Address the operational endpoints (`/health`, `/ready`, `/metrics`) bind
    /// to, on a listener separate from the public gateway so probes and metrics
    /// stay off the public surface. Default `127.0.0.1:9090`; set
    /// `METRICS_ADDR=0.0.0.0:9090` to reach it from another container.
    pub metrics_bind: SocketAddr,
    /// HTTP-transport denial-of-service limits.
    pub http_limits: HttpLimits,
    /// Deployment profile (fail-closed in production).
    pub profile: Profile,
    /// Permits plaintext Bolt to a non-loopback host even in [`Profile::Production`].
    /// Only safe when Neo4j is reachable solely over a trusted private link. Default
    /// `false`; enabling it always logs a warning.
    pub allow_insecure_transport: bool,
}

impl Config {
    /// Starts building a configuration from the built-in defaults.
    #[must_use]
    pub fn builder() -> ConfigBuilder {
        ConfigBuilder::default()
    }

    /// Checks that the HTTP concurrency cap does not exceed the Neo4j connection
    /// pool. When it does, admitted requests beyond the pool size stall on connection
    /// acquire until they time out — an availability cliff under load. The production
    /// profile fails closed on this; development only warns (the caller logs it).
    ///
    /// # Errors
    ///
    /// Returns [`Error`] in [`Profile::Production`] when `HTTP_MAX_CONCURRENCY`
    /// exceeds `NEO4J_MAX_CONNECTIONS`.
    pub fn check_http_pool_capacity(&self) -> Result<(), Error> {
        if self.http_limits.max_concurrency > self.neo4j_max_connections && matches!(self.profile, Profile::Production)
        {
            return Err(Error::bad_config(
                "HTTP_MAX_CONCURRENCY",
                PoolCapacityError {
                    max_concurrency: self.http_limits.max_concurrency,
                    max_connections: self.neo4j_max_connections,
                },
            ));
        }
        Ok(())
    }
}

/// The HTTP concurrency cap exceeds the Neo4j connection pool.
#[derive(Debug)]
struct PoolCapacityError {
    max_concurrency: usize,
    max_connections: usize,
}

impl std::fmt::Display for PoolCapacityError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "HTTP_MAX_CONCURRENCY ({}) exceeds NEO4J_MAX_CONNECTIONS ({}); raise the pool or lower concurrency",
            self.max_concurrency, self.max_connections
        )
    }
}

impl std::error::Error for PoolCapacityError {}

/// The sole populator for [`Config`]. Defaults live here;
/// [`ConfigBuilder::apply_env`] layers env vars over them. URI precedence is
/// CLI > env > default; every other setting is env-or-default.
#[derive(Debug, Clone)]
pub struct ConfigBuilder(Config);

impl Default for ConfigBuilder {
    fn default() -> Self {
        Self(Config {
            neo4j_uri: "bolt://localhost:7687".to_owned(),
            neo4j_user: "neo4j".to_owned(),
            neo4j_password: Secret::new(String::new()),
            neo4j_max_connections: HttpLimits::default().max_concurrency,
            query_timeout: Duration::from_secs(10),
            limits: Limits::default(),
            http_bind: SocketAddr::from(([127, 0, 0, 1], 8080)),
            metrics_bind: SocketAddr::from(([127, 0, 0, 1], 9090)),
            http_limits: HttpLimits::default(),
            profile: Profile::default(),
            allow_insecure_transport: false,
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
        parse_env_into("NEO4J_MAX_CONNECTIONS", &mut self.0.neo4j_max_connections)?;
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
        let http = &mut self.0.http_limits;
        parse_env_into("HTTP_MAX_BODY_BYTES", &mut http.max_body_bytes)?;
        parse_env_into("HTTP_MAX_CONCURRENCY", &mut http.max_concurrency)?;
        parse_env_into("HTTP_MAX_RPS", &mut http.max_rps)?;
        if let Some(ms) = parse_env::<u64>("HTTP_REQUEST_TIMEOUT_MS")? {
            http.request_timeout = Duration::from_millis(ms);
        }
        parse_env_into("HTTP_ADDR", &mut self.0.http_bind)?;
        parse_env_into("METRICS_ADDR", &mut self.0.metrics_bind)?;
        parse_env_into("NEXUS_SCOUT_PROFILE", &mut self.0.profile)?;
        parse_env_into("NEO4J_ALLOW_INSECURE_TRANSPORT", &mut self.0.allow_insecure_transport)?;
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
        // An explicitly empty value (`KEY=`) means "use the default", not a parse
        // error that would abort startup.
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
        // Assert the builder propagates cypher-guard's defaults rather than re-typing them.
        assert_eq!(cfg.limits.guard, GuardLimits::default());
        // The Neo4j pool defaults to the HTTP concurrency cap so admission never
        // admits more than the pool can serve.
        assert_eq!(cfg.neo4j_max_connections, cfg.http_limits.max_concurrency);
        assert_eq!(cfg.neo4j_max_connections, 64);
        // The operational endpoints bind to a separate loopback port by default.
        assert_eq!(cfg.http_bind.to_string(), "127.0.0.1:8080");
        assert_eq!(cfg.metrics_bind.to_string(), "127.0.0.1:9090");
    }

    #[test]
    fn http_pool_capacity_fails_closed_in_production_when_concurrency_exceeds_pool() {
        let check = |profile: Profile, concurrency: usize, pool: usize| {
            let mut cfg = Config::builder().build();
            cfg.profile = profile;
            cfg.http_limits.max_concurrency = concurrency;
            cfg.neo4j_max_connections = pool;
            cfg.check_http_pool_capacity()
        };
        // Concurrency above the pool: production refuses, development tolerates.
        assert!(check(Profile::Production, 64, 16).is_err());
        assert!(check(Profile::Development, 64, 16).is_ok());
        // Aligned (or pool larger) is fine in either profile.
        assert!(check(Profile::Production, 64, 64).is_ok());
        assert!(check(Profile::Production, 32, 64).is_ok());
    }

    #[test]
    fn env_example_documents_the_actual_defaults() {
        // Lock `.env.example` values to the canonical defaults so the doc copy cannot drift.
        let example = include_str!("../../../.env.example");
        let value_of = |key: &str| -> String {
            example
                .lines()
                .map(str::trim)
                .find_map(|l| l.strip_prefix(&format!("{key}=")))
                .unwrap_or_else(|| panic!(".env.example is missing {key}"))
                .to_owned()
        };
        assert_eq!(
            value_of("NEO4J_MAX_CONNECTIONS"),
            Config::builder().build().neo4j_max_connections.to_string()
        );
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

        let h = HttpLimits::default();
        assert_eq!(value_of("HTTP_ADDR"), Config::builder().build().http_bind.to_string());
        assert_eq!(
            value_of("METRICS_ADDR"),
            Config::builder().build().metrics_bind.to_string()
        );
        assert_eq!(value_of("HTTP_MAX_BODY_BYTES"), h.max_body_bytes.to_string());
        assert_eq!(value_of("HTTP_MAX_CONCURRENCY"), h.max_concurrency.to_string());
        assert_eq!(value_of("HTTP_MAX_RPS"), h.max_rps.to_string());
        assert_eq!(
            value_of("HTTP_REQUEST_TIMEOUT_MS"),
            h.request_timeout.as_millis().to_string()
        );
        assert_eq!(value_of("NEXUS_SCOUT_PROFILE"), Profile::default().to_string());
    }
}
