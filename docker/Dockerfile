# Multi-stage build: compile a static-ish release binary, ship it on a slim base.
FROM rust:1.88-slim AS builder
WORKDIR /build
COPY . .
# HTTP-only image: drop the default `mcp` feature so the served artifact carries no
# MCP dependencies (smaller image, less build cost and supply-chain surface). The
# CMD below serves HTTP; `--transport stdio` is intentionally unavailable here.
RUN cargo build --release --bin nexus-scout --no-default-features --features http

FROM debian:bookworm-slim
# wget powers the HEALTHCHECK below (and the docker-compose healthchecks) against
# /health, which is served on the operational port (METRICS_ADDR), not the public one.
RUN apt-get update \
    && apt-get install -y --no-install-recommends wget \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --no-create-home scout
COPY --from=builder /build/target/release/nexus-scout /usr/local/bin/nexus-scout
USER scout
# The public HTTP API. TLS is terminated by a reverse proxy; this serves plain
# HTTP on the private network (see docker/docker-compose.prod.yml). The operational
# endpoints (/health, /ready, /metrics) are served on a separate port (9090 in the
# compose files) that stays internal - never fronted by the reverse proxy.
EXPOSE 8080 9090
# Liveness only (/health does no DB check), so a transient Neo4j blip does not
# flap the container; readiness is observed separately via /ready. The compose
# files bind METRICS_ADDR to 0.0.0.0:9090; localhost:9090 assumes that mapping.
HEALTHCHECK --interval=30s --timeout=5s --start-period=10s --retries=3 \
    CMD wget -qO- http://localhost:9090/health || exit 1
ENTRYPOINT ["nexus-scout"]
CMD ["serve", "--transport", "http"]
