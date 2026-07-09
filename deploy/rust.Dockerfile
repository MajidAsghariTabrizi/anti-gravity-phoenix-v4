FROM rust:1.79-bookworm AS build
ARG CRATE
WORKDIR /src
COPY phoenix-engine ./phoenix-engine
COPY rpc-gateway ./rpc-gateway
COPY recorder ./recorder
COPY replay ./replay
RUN cd "${CRATE}" && cargo test --all
RUN case "${CRATE}" in \
      phoenix-engine) BIN=phoenix-engine ;; \
      rpc-gateway) BIN=rpc-gateway ;; \
      recorder) BIN=phoenix-recorder ;; \
      replay) BIN=phoenix-replay ;; \
      *) echo "unknown crate ${CRATE}" && exit 1 ;; \
    esac && \
    cd "${CRATE}" && cargo build --release --bin "${BIN}" && \
    mkdir -p /out && cp "target/release/${BIN}" /out/service

FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends wget ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/service /usr/local/bin/service
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/service"]
