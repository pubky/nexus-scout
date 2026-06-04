//! Public HTTP transport (Axum).
//!
//! A thin, additive transport over the shared [`Scout`] core, mirroring the MCP
//! `server` module. It is **public and unauthenticated in v1**, so on top of the
//! per-request bounds already enforced by `Scout` it adds aggregate
//! denial-of-service hygiene:
//! a request body-size cap (`413`), a hand-rolled admission limiter (global
//! concurrency + QPS, shedding excess as `429` rather than queueing), a
//! whole-request timeout, panic isolation, and a server-side cost-bound gate at
//! startup / readiness. TLS is terminated by a reverse proxy; this binds plain
//! HTTP. Authentication is a documented follow-up (a middleware at the top of the
//! stack); the structure leaves an obvious insertion point.

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

use crate::config::{Config, HttpLimits, Profile};
use crate::error::Error;
use crate::schema::{schema as curated_schema, GraphSchema};
use crate::Scout;

/// Runs the public HTTP gateway until a shutdown signal, building the Neo4j pool
/// once at startup.
///
/// # Errors
///
/// Returns [`Error`] if the connection cannot be established, if the production
/// profile finds the server-side cost bounds unset, or if the listener/serve
/// loop fails.
pub async fn serve_http(config: Config) -> Result<(), Error> {
    let bind = config.http_bind;
    let limits = config.http_limits;
    let profile = config.profile;
    let scout = Scout::connect(config).await?;

    check_server_bounds(&scout, profile).await?;

    let app = router(scout, limits);
    let listener = tokio::net::TcpListener::bind(bind).await.map_err(Error::internal)?;
    tracing::info!(%bind, "nexus-scout HTTP gateway listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .map_err(Error::internal)?;
    Ok(())
}

/// Verifies the server-side Neo4j cost bounds at startup; in production an unset
/// bound is fail-closed (these are the real backstop for expensive reads).
async fn check_server_bounds(scout: &Scout, profile: Profile) -> Result<(), Error> {
    match scout.verify_server_bounds().await {
        Ok(missing) if missing.is_empty() => {
            tracing::info!("server-side Neo4j cost bounds verified");
            Ok(())
        }
        Ok(missing) => {
            tracing::error!(unset = ?missing, "server-side Neo4j cost bounds are unset or unbounded");
            if profile == Profile::Production {
                return Err(Error::internal(format!(
                    "refusing to start in production: unset server-side cost bounds: {}",
                    missing.join(", ")
                )));
            }
            Ok(())
        }
        Err(e) => {
            tracing::warn!(error = %e, "could not verify server-side cost bounds (degraded check)");
            Ok(())
        }
    }
}

/// Builds the application router. Separated from [`serve_http`] so tests can
/// drive it with `tower::ServiceExt::oneshot` against an already-built [`Scout`].
/// Exposed (doc-hidden) for the integration suite; not a stable public API.
#[doc(hidden)]
pub fn router(scout: Scout, limits: HttpLimits) -> Router {
    let state = AppState {
        scout,
        limits,
        shared: Arc::new(Shared::new(limits.max_rps)),
    };

    // Cost controls apply to /v1/query only; probes and schema stay cheap and
    // are never shed, so a busy server remains observable and orchestratable.
    let query = Router::new()
        .route("/v1/query", post(query_handler))
        .layer(axum::middleware::from_fn_with_state(state.clone(), admit))
        .layer(RequestBodyLimitLayer::new(limits.max_body_bytes));

    Router::new()
        .merge(query)
        .route("/v1/schema", get(schema_handler))
        .route("/health", get(health_handler))
        .route("/ready", get(ready_handler))
        .route("/metrics", get(metrics_handler))
        .with_state(state)
        .layer(TimeoutLayer::with_status_code(StatusCode::GATEWAY_TIMEOUT, limits.request_timeout))
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
/// Excess is **shed** immediately as `429` rather than queued, so a flood cannot
/// pile up behind the timeout.
async fn admit(State(state): State<AppState>, request: Request, next: Next) -> Response {
    let shared = &state.shared;
    shared.total.fetch_add(1, Relaxed);
    shared.in_flight.fetch_add(1, Relaxed);
    let _guard = InFlight(Arc::clone(&state.shared));

    let in_flight = shared.in_flight.load(Relaxed);
    if usize::try_from(in_flight).unwrap_or(usize::MAX) > state.limits.max_concurrency {
        shared.shed.fetch_add(1, Relaxed);
        return ApiError(Error::rate_limited()).into_response();
    }
    // `lock()` only poisons if a holder panicked while mutating; the bucket has
    // no invariant to corrupt, so recover the guard rather than propagating.
    let allowed = state.shared.bucket.lock().map_or(true, |mut b| b.try_acquire());
    if !allowed {
        shared.shed.fetch_add(1, Relaxed);
        return ApiError(Error::rate_limited()).into_response();
    }

    let response = next.run(request).await;
    record_status(shared, response.status());
    response
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
    let response = state.scout.query(&req.cypher, req.params, req.limit).await.map_err(ApiError)?;
    Ok(Json(response))
}

async fn schema_handler() -> Json<&'static GraphSchema> {
    Json(curated_schema())
}

/// Liveness: the process is up. No database check (a Neo4j blip should not
/// trigger a restart — that is what readiness is for).
async fn health_handler() -> StatusCode {
    StatusCode::OK
}

/// Readiness: Neo4j is reachable **and** the server-side cost bounds are set.
async fn ready_handler(State(state): State<AppState>) -> Response {
    match state.scout.verify_server_bounds().await {
        Ok(missing) if missing.is_empty() => StatusCode::OK.into_response(),
        Ok(missing) => {
            let body = ErrorResponse::new(
                ErrorCode::InternalError,
                format!("server-side cost bounds unset: {}", missing.join(", ")),
                "Set db.transaction.timeout and the transaction memory limits in neo4j.conf.",
            );
            (StatusCode::SERVICE_UNAVAILABLE, Json(body)).into_response()
        }
        Err(e) => (StatusCode::SERVICE_UNAVAILABLE, Json(e.to_response())).into_response(),
    }
}

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

/// The gateway [`Error`] rendered as an HTTP response: the shared status map plus
/// the standard error envelope, so every enveloped error is one shape.
struct ApiError(Error);

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let status = StatusCode::from_u16(http_status(self.0.code())).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
        (status, Json(self.0.to_response())).into_response()
    }
}

/// A JSON body extractor whose rejection is the standard error envelope (a `400`
/// `QUERY_REJECTED`) rather than axum's default plain-text rejection, so a
/// malformed request body is the same wire shape as any other 4xx.
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

/// Assigns a monotonic `x-request-id` to each request for log correlation. A
/// process-local counter avoids a `uuid` dependency; it is sufficient to
/// correlate a request across the access log within one process lifetime.
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

/// Resolves when the process receives SIGINT or SIGTERM, so in-flight requests
/// drain on deploy/restart.
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
    fn api_error_maps_each_code_to_its_status() {
        // The envelope body's machine code is covered by the types crate's
        // serialization tests; here we lock the error-kind → HTTP status mapping.
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
        // Capacity exhausted within the same instant; the next is denied.
        assert!(!bucket.try_acquire());
    }
}
