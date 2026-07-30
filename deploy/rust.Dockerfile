FROM rust:1.79-bookworm AS build
ARG CRATE
WORKDIR /src
COPY phoenix-engine/Cargo.toml phoenix-engine/Cargo.lock ./phoenix-engine/
COPY rpc-gateway/Cargo.toml rpc-gateway/Cargo.lock ./rpc-gateway/
COPY recorder/Cargo.toml recorder/Cargo.lock ./recorder/
COPY replay/Cargo.toml replay/Cargo.lock ./replay/
COPY live-executor/Cargo.toml live-executor/Cargo.lock ./live-executor/
COPY fork-sandbox/Cargo.toml fork-sandbox/Cargo.lock ./fork-sandbox/
COPY money-path-classifier/Cargo.toml money-path-classifier/Cargo.lock ./money-path-classifier/
RUN set -eux; \
    mkdir -p \
      phoenix-engine/src \
      rpc-gateway/src \
      recorder/src/bin \
      replay/src \
      live-executor/src \
      fork-sandbox/src \
      money-path-classifier/src; \
    printf 'pub fn dependency_cache() {}\n' >phoenix-engine/src/lib.rs; \
    printf 'fn main() {}\n' >phoenix-engine/src/main.rs; \
    printf 'fn main() {}\n' >phoenix-engine/src/shadow_positive_route_evidence_main.rs; \
    printf 'pub fn dependency_cache() {}\n' >rpc-gateway/src/lib.rs; \
    printf 'fn main() {}\n' >rpc-gateway/src/main.rs; \
    printf 'pub fn dependency_cache() {}\n' >recorder/src/lib.rs; \
    printf 'fn main() {}\n' >recorder/src/main.rs; \
    printf 'fn main() {}\n' >recorder/src/bin/shadow-dispatcher.rs; \
    printf 'pub fn dependency_cache() {}\n' >replay/src/lib.rs; \
    printf 'fn main() {}\n' >replay/src/main.rs; \
    printf 'pub fn dependency_cache() {}\n' >live-executor/src/lib.rs; \
    printf 'fn main() {}\n' >live-executor/src/main.rs; \
    printf 'fn main() {}\n' >live-executor/src/approve_execution_request_main.rs; \
    printf 'fn main() {}\n' >live-executor/src/autonomous_live_control_main.rs; \
    printf 'pub fn dependency_cache() {}\n' >fork-sandbox/src/lib.rs; \
    printf 'fn main() {}\n' >fork-sandbox/src/main.rs; \
    printf 'pub fn dependency_cache() {}\n' >money-path-classifier/src/lib.rs; \
    cd "${CRATE}"; \
    cargo build --locked; \
    cargo build --locked --release
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
COPY config/phoenix-route-policy-3000-500-v1.json ./config/phoenix-route-policy-3000-500-v1.json
COPY fixtures/autonomous-hunter/v1/valid/route-policy.json ./fixtures/autonomous-hunter/v1/valid/route-policy.json
COPY fixtures/hunter-a1/v1/pinned-fork-cross-tick.json ./fixtures/hunter-a1/v1/pinned-fork-cross-tick.json
COPY fixtures/hunter-a1/v1/autonomous-candidate.json ./fixtures/hunter-a1/v1/autonomous-candidate.json
COPY migrations ./migrations
COPY deploy/nats-server.conf ./deploy/nats-server.conf
COPY scripts/recorder-live-smoke.sh ./scripts/recorder-live-smoke.sh
COPY scripts/sql/prelive-money-path-report.sql ./scripts/sql/prelive-money-path-report.sql
RUN find \
      phoenix-engine \
      rpc-gateway \
      recorder \
      replay \
      live-executor \
      fork-sandbox \
      money-path-classifier \
      -type f -name '*.rs' -exec touch {} +
RUN cd "${CRATE}" && cargo test --all
RUN set -eux; \
    case "${CRATE}" in \
      phoenix-engine) BIN=phoenix-engine ;; \
      rpc-gateway) BIN=rpc-gateway ;; \
      recorder) BIN=phoenix-recorder ;; \
      replay) BIN=phoenix-replay ;; \
      live-executor) BIN=live-executor ;; \
      *) echo "unknown crate ${CRATE}" && exit 1 ;; \
    esac; \
    cd "${CRATE}"; \
    cargo build --release --bin "${BIN}"; \
    mkdir -p /out; \
    cp "target/release/${BIN}" /out/service; \
    if [ "${CRATE}" = "phoenix-engine" ]; then \
      cargo build --release --bin shadow-positive-route-evidence; \
      cp target/release/shadow-positive-route-evidence /out/shadow-positive-route-evidence; \
    fi; \
    if [ "${CRATE}" = "recorder" ]; then \
      cargo build --release --bin shadow-dispatcher; \
      cp target/release/shadow-dispatcher /out/shadow-dispatcher; \
    fi; \
    if [ "${CRATE}" = "live-executor" ]; then \
      cargo build --release --bin approve-execution-request; \
      cp target/release/approve-execution-request /out/approve-execution-request; \
      cargo build --release --bin autonomous-live-control; \
      cp target/release/autonomous-live-control /out/autonomous-live-control; \
    fi; \
    test -x /out/service; \
    if [ "${CRATE}" = "live-executor" ]; then \
      test -x /out/approve-execution-request; \
      test -x /out/autonomous-live-control; \
    fi

FROM debian:bookworm-slim
ARG CRATE
WORKDIR /app
RUN apt-get update \
    && apt-get install -y --no-install-recommends wget ca-certificates libgcc-s1 \
    && rm -rf /var/lib/apt/lists/*
COPY --from=build /out/ /usr/local/bin/
RUN set -eux; \
    test -x /usr/local/bin/service; \
    if [ "${CRATE}" = "live-executor" ]; then \
      test -x /usr/local/bin/approve-execution-request; \
      test -x /usr/local/bin/autonomous-live-control; \
      probe_stderr="$(mktemp)"; \
      probe_output="$(/usr/local/bin/autonomous-live-control __image_runtime_probe__ 2>"$probe_stderr")"; \
      [ ! -s "$probe_stderr" ]; \
      [ "$probe_output" = "AUTONOMOUS_CONTROL_RUNTIME_OK" ]; \
      case "$probe_output" in *AUTONOMOUS_CONTROL_FAILED:*) exit 1 ;; esac; \
      rm -f "$probe_stderr"; \
    fi
USER 65532:65532
ENTRYPOINT ["/usr/local/bin/service"]
