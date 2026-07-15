FROM rust:1.79-bookworm AS build
ARG CRATE
WORKDIR /src
COPY phoenix-engine ./phoenix-engine
COPY rpc-gateway ./rpc-gateway
COPY recorder ./recorder
COPY replay ./replay
COPY fixtures/routes ./fixtures/routes
COPY fixtures/engine ./fixtures/engine
COPY migrations ./migrations
COPY deploy/nats-server.conf ./deploy/nats-server.conf
COPY scripts/recorder-live-smoke.sh ./scripts/recorder-live-smoke.sh
COPY scripts/sql/prelive-money-path-report.sql ./scripts/sql/prelive-money-path-report.sql
RUN cd "${CRATE}" && cargo test --all
RUN case "${CRATE}" in \
      phoenix-engine) BIN=phoenix-engine ;; \
      rpc-gateway) BIN=rpc-gateway ;; \
      recorder) BIN=phoenix-recorder ;; \
      replay) BIN=phoenix-replay ;; \
      *) echo "unknown crate ${CRATE}" && exit 1 ;; \
    esac && \
    cd "${CRATE}" && cargo build --release --bin "${BIN}" && \
    mkdir -p /out && cp "target/release/${BIN}" /out/service && \
    if [ "${CRATE}" = "phoenix-engine" ]; then \
      cargo build --release --bin shadow-positive-route-evidence && \
      cp target/release/shadow-positive-route-evidence /out/shadow-positive-route-evidence; \
    fi && \
    if [ "${CRATE}" = "recorder" ]; then \
      cargo build --release --bin shadow-dispatcher && \
      cp target/release/shadow-dispatcher /out/shadow-dispatcher; \
    fi

FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends wget ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/ /usr/local/bin/
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/service"]
