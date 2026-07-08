# Security

Phoenix v4 is shadow-first and secrets-clean by default.

## Requirements

- No private keys in source.
- No API tokens in source.
- No credential values in examples.
- `.env` is ignored.
- `.env.example` uses placeholders only.
- Raw secret environment variables are never logged.
- LIVE execution requires explicit gates.

## Threat Model

- stolen signer key
- malicious callback
- fake pool
- compromised RPC response
- stale local state
- NATS injection
- malformed feed message
- duplicated feed event
- sequence gap
- database unavailable
- dashboard compromise
- arbitrary contract call injection

## Mitigations

- The executor is not a generic arbitrary-call wallet.
- Approved flash provider, assets, pool factories, and pools are enforced.
- Flash callback verifies provider, initiator, asset, amount, and active context.
- V3 callbacks are rejected unless they match active execution context and approved pool configuration.
- Hot path has zero external RPC reads.
- Read RPC goes through budgets, caches, and validation.
- Unsupported origins and incomplete state are measured and rejected.

## Security Tooling

Run:

```bash
./scripts/secret-scan.sh
cargo audit
govulncheck ./...
slither contracts/src/PhoenixExecutor.sol
```

Unavailable tools must be documented in the final verification report.

