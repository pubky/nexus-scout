//! Public HTTP transport (Axum) over the shared [`Scout`] core. Public and
//! unauthenticated in v1, so on top of the per-request bounds it adds aggregate
//! `DoS` hygiene: a body-size cap (413), an admission limiter (concurrency + QPS,
//! shedding excess as 429), a whole-request timeout, panic isolation, and a
//! startup/readiness cost-bound gate. TLS is terminated by a reverse proxy.

use std::sync::atomic::{AtomicI64, AtomicU64, Ordering::Relaxed};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use axum::extract::rejection::JsonRejection;
use axum::extract::{FromRequest, Request, State};
use axum::http::{HeaderValue, StatusCode};
use axum::middleware::Next;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use serde::de::DeserializeOwned;
use tower_http::catch_panic::CatchPanicLayer;
use tower_http::limit::RequestBodyLimitLayer;
use tower_http::request_id::{MakeRequestId, PropagateRequestIdLayer, RequestId, SetRequestIdLayer};
use tower_http::timeout::TimeoutLayer;
use tower_http::trace::TraceLayer;

use nexus_scout_types::{http_status, ErrorCode, ErrorResponse, QueryRequest, QueryResponse};

use crate::config::{Config, HttpLimits};
use crate::error::Error;
use crate::schema::{schema as curated_schema, GraphSchema};
use crate::{BoundsOutcome, Scout, COST_BOUNDS_HINT};

/// Runs the public HTTP gateway until a shutdown signal, building the Neo4j pool
/// once at startup.
///
/// # Errors
///
/// Returns [`Error`] if the connection cannot be established, if the production
/// profile finds the server-side cost bounds unset or unverifiable, or if the
/// listener/serve loop fails.
pub async fn serve_http(config: Config) -> Result<(), Error> {
    let bind = config.http_bind;
    let limits = config.http_limits;
    // Fail closed in production (warn in development) if admission would admit more
    // requests than the Neo4j pool can serve.
    config.check_http_pool_capacity()?;
    if config.http_limits.max_concurrency > config.neo4j_max_connections {
        tracing::warn!(
            max_concurrency = config.http_limits.max_concurrency,
            max_connections = config.neo4j_max_connections,
            "HTTP_MAX_CONCURRENCY exceeds NEO4J_MAX_CONNECTIONS; admitted requests beyond the pool will \
             stall on connection acquire until they time out"
        );
    }
    let scout = Scout::connect(config).await?;

    scout.ensure_cost_bounds().await?;

    let app = router(scout, limits);
    let listener = tokio::net::TcpListener::bind(bind).await.map_err(Error::internal)?;
    tracing::info!(%bind, "nexus-scout HTTP gateway listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(Error::internal)?;
    Ok(())
}

/// Builds the application router. Separated from [`serve_http`] so tests can drive
/// it with `oneshot`; doc-hidden, not a stable API.
#[doc(hidden)]
pub fn router(scout: Scout, limits: HttpLimits) -> Router {
    let state = AppState {
        scout,
        limits,
        shared: Arc::new(Shared::new(limits.max_rps)),
    };

    // Cost controls apply to /v1/query only; probes and schema stay cheap and are never shed.
    let query = Router::new()
        .route("/v1/query", post(query_handler))
        .layer(axum::middleware::from_fn_with_state(state.clone(), admit))
        .layer(RequestBodyLimitLayer::new(limits.max_body_bytes));

    Router::new()
        .merge(query)
        .route("/", get(index_handler))
        .route("/llms.txt", get(llms_handler))
        .route("/v1/schema", get(schema_handler))
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .route("/metrics", get(metrics_handler))
        .with_state(state)
        .layer(TimeoutLayer::with_status_code(
            StatusCode::GATEWAY_TIMEOUT,
            limits.request_timeout,
        ))
        .layer(TraceLayer::new_for_http())
        .layer(CatchPanicLayer::new())
        .layer(PropagateRequestIdLayer::x_request_id())
        .layer(SetRequestIdLayer::x_request_id(CounterRequestId::default()))
}

/// Shared, cloneable handler state.
#[derive(Clone)]
struct AppState {
    scout: Scout,
    limits: HttpLimits,
    shared: Arc<Shared>,
}

/// Process-wide counters and the QPS bucket.
struct Shared {
    in_flight: AtomicI64,
    total: AtomicU64,
    rejected: AtomicU64,
    failed: AtomicU64,
    shed: AtomicU64,
    bucket: Mutex<TokenBucket>,
}

impl Shared {
    fn new(max_rps: u32) -> Self {
        Self {
            in_flight: AtomicI64::new(0),
            total: AtomicU64::new(0),
            rejected: AtomicU64::new(0),
            failed: AtomicU64::new(0),
            shed: AtomicU64::new(0),
            bucket: Mutex::new(TokenBucket::new(max_rps)),
        }
    }
}

/// Decrements the in-flight gauge when an admitted request finishes (or panics).
struct InFlight(Arc<Shared>);

impl Drop for InFlight {
    fn drop(&mut self) {
        self.0.in_flight.fetch_sub(1, Relaxed);
    }
}

/// Admission control for `/v1/query`: a global in-flight cap and a QPS bucket.
/// Excess is shed as 429, not queued.
async fn admit(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let shared = &state.shared;
    shared.total.fetch_add(1, Relaxed);
    shared.in_flight.fetch_add(1, Relaxed);
    let _guard = InFlight(Arc::clone(&state.shared));

    let in_flight = shared.in_flight.load(Relaxed);
    if usize::try_from(in_flight).unwrap_or(usize::MAX) > state.limits.max_concurrency {
        return shed(shared);
    }
    // lock() poisons only if a holder panicked mid-mutation; the bucket has no
    // invariant to corrupt, so recover the guard and still apply the limit — a
    // public limiter must never fail open. Scoped so the guard drops before the await.
    let allowed = {
        let mut bucket = state
            .shared
            .bucket
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        bucket.try_acquire()
    };
    if !allowed {
        return shed(shared);
    }

    let response = next.run(request).await;
    record_status(shared, response.status());
    response
}

/// Sheds a request as 429, counting it in both `shed` and the 4xx total.
fn shed(shared: &Shared) -> Response {
    shared.shed.fetch_add(1, Relaxed);
    shared.rejected.fetch_add(1, Relaxed);
    ApiError(Error::rate_limited()).into_response()
}

fn record_status(shared: &Shared, status: StatusCode) {
    if status.is_server_error() {
        shared.failed.fetch_add(1, Relaxed);
    } else if status.is_client_error() {
        shared.rejected.fetch_add(1, Relaxed);
    }
}

async fn query_handler(
    State(state): State<AppState>,
    ValidatedJson(req): ValidatedJson<QueryRequest>,
) -> Result<Json<QueryResponse>, ApiError> {
    let response = state
        .scout
        .query(&req.cypher, req.params, req.limit)
        .await
        .map_err(ApiError)?;
    Ok(Json(response))
}

async fn schema_handler() -> Json<&'static GraphSchema> {
    Json(curated_schema())
}

/// The agent-facing usage guide, kept in sync by being the repo's `SKILL.md`
/// itself rather than a second copy.
const SKILL_DOC: &str = include_str!("../../../SKILL.md");

/// Strips a leading YAML frontmatter block. The header addresses skill loaders;
/// an HTTP caller wants the body. A document without one is served unchanged.
fn strip_frontmatter(doc: &str) -> &str {
    doc.strip_prefix("---\n")
        .and_then(|rest| rest.split_once("\n---\n"))
        .map_or(doc, |(_, body)| body.trim_start())
}

async fn llms_handler() -> &'static str {
    strip_frontmatter(SKILL_DOC)
}

/// Service descriptor for the bare base URL. A caller that arrives knowing only
/// the hostname has to learn the query path, the request body's field names, and
/// the row cap before it can do anything, so all three are stated here rather
/// than left to be discovered from 404s and deserialization errors.
async fn index_handler(State(state): State<AppState>) -> Json<serde_json::Value> {
    let limits = state.scout.limits();
    Json(serde_json::json!({
        "service": "nexus-scout",
        "description": "Read-only Cypher gateway to the Pubky social graph.",
        "start_here": "/llms.txt",
        "endpoints": {
            "POST /v1/query": "Run read-only Cypher.",
            "GET /v1/schema": "Node labels, relationship types, and example queries.",
            "GET /llms.txt": "Usage guide: recipes, limits, and error recovery.",
            "GET /health": "Liveness.",
            "GET /ready": "Readiness. A 503 here does not stop /v1/query from serving queries.",
        },
        "example_request": {
            "method": "POST",
            "path": "/v1/query",
            "headers": { "content-type": "application/json" },
            "body": {
                "cypher": "MATCH (u:User)<-[f:FOLLOWS]-() RETURN u.name AS name, count(f) AS followers ORDER BY followers DESC LIMIT 10",
                "params": {},
            },
        },
        "limits": {
            "default_limit": limits.default_limit,
            "max_result_rows": limits.max_result_rows,
            "max_path_depth": limits.guard.max_path_depth,
            "max_body_bytes": state.limits.max_body_bytes,
        },
        "notes": [
            format!(
                "A query with no LIMIT returns {} rows; the ceiling is {}. Page past the ceiling with \
                 SKIP/LIMIT and reconcile the total against a count() query.",
                limits.default_limit, limits.max_result_rows
            ),
            "Read-only: writes, CALL, and admin clauses are rejected. Errors carry a machine-readable \
             code plus a hint describing the fix."
                .to_owned(),
        ],
    }))
}

/// Liveness: the process is up. No database check (that is what readiness is for).
async fn health_handler() -> StatusCode {
    StatusCode::OK
}

/// Readiness: Neo4j reachable and server-side cost bounds set, via the same
/// classification as the startup gate.
async fn ready_handler(State(state): State<AppState>) -> Response {
    let (outcome, detail) = state.scout.cost_bounds_outcome().await;
    match outcome {
        BoundsOutcome::AllSet => StatusCode::OK.into_response(),
        BoundsOutcome::SomeUnset | BoundsOutcome::Unverifiable => {
            let body = ErrorResponse::new(ErrorCode::InternalError, detail, COST_BOUNDS_HINT);
            (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
        }
    }
}

/// Counters are admission-scoped for `/v1/query`: they include shed 429s but not
/// outer-layer errors (413/504/caught panic), which never reach `admit`.
async fn metrics_handler(State(state): State<AppState>) -> String {
    let s = &state.shared;
    format!(
        "nexus_scout_in_flight {}\n\
         nexus_scout_requests_total {}\n\
         nexus_scout_responses_4xx_total {}\n\
         nexus_scout_responses_5xx_total {}\n\
         nexus_scout_shed_total {}\n",
        s.in_flight.load(Relaxed),
        s.total.load(Relaxed),
        s.rejected.load(Relaxed),
        s.failed.load(Relaxed),
        s.shed.load(Relaxed),
    )
}

/// The gateway [`Error`] rendered as an HTTP response (status map + error envelope).
struct ApiError(Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(http_status(self.0.code())).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self.0.to_response())).into_response()
    }
}

/// A JSON body extractor whose rejection is the standard error envelope (400
/// `QUERY_REJECTED`).
struct ValidatedJson<T>(T);

impl<T, S> FromRequest<S> for ValidatedJson<T>
where
    T: DeserializeOwned,
    S: Send + Sync,
{
    type Rejection = ApiError;

    async fn from_request(request: Request, state: &S) -> Result<Self, Self::Rejection> {
        match Json::<T>::from_request(request, state).await {
            Ok(Json(value)) => Ok(Self(value)),
            Err(rejection) => Err(ApiError(Error::bad_request(json_rejection_message(&rejection)))),
        }
    }
}

fn json_rejection_message(rejection: &JsonRejection) -> String {
    match rejection {
        JsonRejection::MissingJsonContentType(_) => "request body must be application/json".to_owned(),
        other => other.body_text(),
    }
}

/// Assigns a monotonic `x-request-id` per request for log correlation; a
/// deterministic per-process counter, stable across test runs and needing no RNG.
#[derive(Clone, Default)]
struct CounterRequestId(Arc<AtomicU64>);

impl MakeRequestId for CounterRequestId {
    fn make_request_id<B>(&mut self, _request: &axum::http::Request<B>) -> Option<RequestId> {
        let n = self.0.fetch_add(1, Relaxed);
        HeaderValue::from_str(&n.to_string()).ok().map(RequestId::new)
    }
}

/// A simple token bucket bounding sustained `/v1/query` throughput.
struct TokenBucket {
    capacity: f64,
    tokens: f64,
    refill_per_sec: f64,
    last: Instant,
}

impl TokenBucket {
    fn new(rps: u32) -> Self {
        let capacity = f64::from(rps.max(1));
        Self {
            capacity,
            tokens: capacity,
            refill_per_sec: capacity,
            last: Instant::now(),
        }
    }

    fn try_acquire(&mut self) -> bool {
        let now = Instant::now();
        let elapsed = now.duration_since(self.last).as_secs_f64();
        self.last = now;
        self.tokens = (self.tokens + elapsed * self.refill_per_sec).min(self.capacity);
        if self.tokens >= 1.0 {
            self.tokens -= 1.0;
            true
        } else {
            false
        }
    }
}

/// Resolves on SIGINT/SIGTERM so in-flight requests drain on restart.
async fn shutdown_signal() {
    let ctrl_c = async {
        let _ = tokio::signal::ctrl_c().await;
    };
    #[cfg(unix)]
    let terminate = async {
        match tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate()) {
            Ok(mut signal) => {
                signal.recv().await;
            }
            Err(_) => std::future::pending::<()>().await,
        }
    };
    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }
    tracing::info!("shutdown signal received; draining in-flight requests");
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn llms_txt_serves_the_skill_body_without_its_frontmatter() {
        let served = strip_frontmatter(SKILL_DOC);
        // The YAML header addresses skill loaders, not HTTP callers.
        assert!(!served.starts_with("---"), "frontmatter leaked: {:?}", &served[..40]);
        assert!(!served.contains("\nname: nexus-scout\n"), "frontmatter leaked");
        assert!(served.starts_with('#'), "body should open on a heading: {:?}", &served[..40]);

        // The three facts a caller cannot get anywhere else: the base URL, the query
        // endpoint, and the request body's field name.
        assert!(served.contains("https://nexus-scout.pubky.org"));
        assert!(served.contains("/v1/query"));
        assert!(served.contains("\"cypher\""));
    }

    #[test]
    fn strip_frontmatter_leaves_a_plain_document_alone() {
        assert_eq!(strip_frontmatter("# Title\n\nbody\n"), "# Title\n\nbody\n");
        // An unterminated header is not frontmatter; serve it rather than eat the file.
        assert_eq!(strip_frontmatter("---\nname: x\n"), "---\nname: x\n");
    }

    #[test]
    fn api_error_maps_each_code_to_its_status() {
        // Lock the error-kind → HTTP status mapping.
        let cases = [
            (Error::bad_request("nope"), 400),
            (Error::timeout(10), 504),
            (Error::rate_limited(), 429),
            (Error::internal("boom"), 500),
        ];
        for (err, status) in cases {
            assert_eq!(ApiError(err).into_response().status().as_u16(), status);
        }
    }

    #[test]
    fn token_bucket_allows_a_burst_then_throttles() {
        let mut bucket = TokenBucket::new(3);
        assert!(bucket.try_acquire());
        assert!(bucket.try_acquire());
        assert!(bucket.try_acquire());
        assert!(!bucket.try_acquire());
    }
}
