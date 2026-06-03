# Multi-stage build: compile a static-ish release binary, ship it on a slim base.
FROM rust:1.85-slim AS builder
WORKDIR /build
COPY . .
RUN cargo build --release --bin nexus-scout

FROM debian:bookworm-slim
RUN useradd --system --no-create-home scout
COPY --from=builder /build/target/release/nexus-scout /usr/local/bin/nexus-scout
USER scout
ENTRYPOINT ["nexus-scout"]
CMD ["serve", "--transport", "stdio"]
