FROM rust:1.79-bookworm AS build
ARG CRATE
WORKDIR /src
COPY phoenix-engine ./phoenix-engine
COPY rpc-gateway ./rpc-gateway
COPY recorder ./recorder
COPY replay ./replay
COPY live-executor ./live-executor
COPY fork-sandbox ./fork-sandbox
COPY money-path-classifier ./money-path-classifier
COPY fixtures/routes ./fixtures/routes
COPY fixtures/engine ./fixtures/engine
COPY config/phoenix-route-universe-v1.json ./config/phoenix-route-universe-v1.json
COPY config/phoenix-route-policy-v1.json ./config/phoenix-route-policy-v1.json
COPY fixtures/autonomous-hunter/v1/valid/route-policy.json ./fixtures/autonomous-hunter/v1/valid/route-policy.json
COPY fixtures/hunter-a1/v1/pinned-fork-cross-tick.json ./fixtures/hunter-a1/v1/pinned-fork-cross-tick.json
COPY fixtures/hunter-a1/v1/autonomous-candidate.json ./fixtures/hunter-a1/v1/autonomous-candidate.json
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
      live-executor) BIN=live-executor ;; \
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
    fi && \
    if [ "${CRATE}" = "live-executor" ]; then \
      cargo build --release --bin approve-execution-request && \
      cp target/release/approve-execution-request /out/approve-execution-request && \
      cargo build --release --bin autonomous-live-control && \
      cp target/release/autonomous-live-control /out/autonomous-live-control; \
    fi

FROM debian:bookworm-slim
WORKDIR /app
RUN apt-get update && apt-get install -y --no-install-recommends wget ca-certificates && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/ /usr/local/bin/
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/service"]
