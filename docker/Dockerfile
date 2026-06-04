# Multi-stage build: compile a static-ish release binary, ship it on a slim base.
FROM rust:1.88-slim AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --bin nexus-scout

FROM debian:bookworm-slim
# wget is used by the container/compose healthcheck against /health.
RUN apt-get update \
    && apt-get install -y --no-install-recommends wget \
    && rm -rf /var/lib/apt/lists/* \
    && useradd --system --no-create-home scout
COPY --from=builder /build/target/release/nexus-scout /usr/local/bin/nexus-scout
USER scout
# The public HTTP API. TLS is terminated by a reverse proxy; this serves plain
# HTTP on the private network (see docker/docker-compose.prod.yml).
EXPOSE 8080
ENTRYPOINT ["nexus-scout"]
CMD ["serve", "--transport", "http"]
