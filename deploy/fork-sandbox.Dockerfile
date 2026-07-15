FROM rust:1.79-bookworm AS build
WORKDIR /src
COPY rpc-gateway ./rpc-gateway
COPY fork-sandbox ./fork-sandbox
COPY migrations ./migrations
COPY phoenix-engine/Cargo.toml ./phoenix-engine/Cargo.toml
COPY compose.prod.yml ./compose.prod.yml
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
