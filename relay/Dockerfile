# syntax=docker/dockerfile:1.7
# sidekar-relay container. Two-stage: rust builder → minimal runtime.
# No Chrome, no extensions — the relay is pure HTTP/WS + MongoDB client.

FROM rust:1.88-slim AS builder
WORKDIR /app

# Cache dependency compilation by faking a minimal crate first.
COPY Cargo.toml Cargo.lock ./
RUN mkdir -p src && echo "fn main() {}" > src/main.rs && \
    cargo build --release && \
    rm -rf src target/release/deps/sidekar_relay* target/release/sidekar-relay

# Real sources + final build.
COPY src ./src
RUN cargo build --release && strip target/release/sidekar-relay

FROM debian:bookworm-slim
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=builder /app/target/release/sidekar-relay /usr/local/bin/
EXPOSE 8080
CMD ["sidekar-relay"]
