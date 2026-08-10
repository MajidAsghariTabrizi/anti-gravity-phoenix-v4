FROM rust:1.79-bookworm AS build
WORKDIR /src
COPY rpc-gateway/Cargo.toml rpc-gateway/Cargo.lock ./rpc-gateway/
COPY fork-sandbox/Cargo.toml fork-sandbox/Cargo.lock ./fork-sandbox/
RUN set -eux; \
    mkdir -p rpc-gateway/src fork-sandbox/src; \
    printf 'pub fn dependency_cache() {}\n' >rpc-gateway/src/lib.rs; \
    printf 'fn main() {}\n' >rpc-gateway/src/main.rs; \
    printf 'pub fn dependency_cache() {}\n' >fork-sandbox/src/lib.rs; \
    printf 'fn main() {}\n' >fork-sandbox/src/main.rs; \
    cargo build --locked --manifest-path fork-sandbox/Cargo.toml; \
    cargo build --locked --release --manifest-path fork-sandbox/Cargo.toml
COPY rpc-gateway ./rpc-gateway
COPY fork-sandbox ./fork-sandbox
COPY fixtures/routes ./fixtures/routes
COPY migrations ./migrations
COPY phoenix-engine/Cargo.toml ./phoenix-engine/Cargo.toml
COPY compose.prod.yml ./compose.prod.yml
RUN find rpc-gateway fork-sandbox -type f -name '*.rs' -exec touch {} +
RUN cargo test --locked --manifest-path fork-sandbox/Cargo.toml
RUN cargo build --locked --release --manifest-path fork-sandbox/Cargo.toml \
    --bin phoenix-fork-sandbox && \
    mkdir -p /out && \
    cp fork-sandbox/target/release/phoenix-fork-sandbox /out/phoenix-fork-sandbox

FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && \
    apt-get install -y --no-install-recommends ca-certificates && \
    rm -rf /var/lib/apt/lists/*
COPY --from=build /out/phoenix-fork-sandbox /usr/local/bin/phoenix-fork-sandbox
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/phoenix-fork-sandbox"]
