SHELL := /bin/sh

.PHONY: verify go-test rust-test contract-test python-smoke secret-scan integration bench

verify: go-test rust-test contract-test python-smoke secret-scan

go-test:
	cd feed-ingestor && test -z "$$(gofmt -l .)" && go vet ./... && go test ./...
	cd migration-runner && test -z "$$(gofmt -l .)" && go vet ./... && go test ./...

rust-test:
	@if command -v cargo >/dev/null 2>&1; then \
		cargo fmt --check --manifest-path phoenix-engine/Cargo.toml && \
		cargo clippy --manifest-path phoenix-engine/Cargo.toml --all-targets --all-features -- -D warnings && \
		cargo test --manifest-path phoenix-engine/Cargo.toml --all && \
		cargo fmt --check --manifest-path rpc-gateway/Cargo.toml && \
		cargo clippy --manifest-path rpc-gateway/Cargo.toml --all-targets --all-features -- -D warnings && \
		cargo test --manifest-path rpc-gateway/Cargo.toml --all && \
		cargo test --manifest-path recorder/Cargo.toml --all && \
		cargo test --manifest-path replay/Cargo.toml --all; \
	else \
		echo "cargo unavailable; Rust verification blocked"; \
		exit 2; \
	fi

contract-test:
	@if command -v forge >/dev/null 2>&1; then \
		cd contracts && forge fmt --check && forge test; \
	else \
		echo "forge unavailable; contract verification blocked"; \
		exit 2; \
	fi

python-smoke:
	python -m py_compile dashboard/app.py

secret-scan:
	./scripts/secret-scan.sh

integration:
	@if [ -n "$$ARBITRUM_RPC_URL" ]; then echo "integration credentials present"; else echo "ARBITRUM_RPC_URL missing; live/fork integration skipped"; fi

bench:
	@if command -v cargo >/dev/null 2>&1; then cargo test --manifest-path phoenix-engine/Cargo.toml --release bench_decision_path -- --ignored --nocapture; else echo "cargo unavailable; benchmark not measured"; exit 2; fi
