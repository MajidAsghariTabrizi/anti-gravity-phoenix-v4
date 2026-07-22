# Phoenix LIVE Canary Executor v1

`live-executor` is an isolated, disabled-by-default service. It does not change
the SHADOW behavior of Phoenix Engine, Feed, Recorder, or the reviewed v5
production Compose contract.

## Activation boundary

The service can submit only when all environment gates are exact:

- `PHOENIX_MODE=LIVE`
- `LIVE_EXECUTION=true`
- `LIVE_EXECUTOR_ARMED=true`
- `LIVE_EXECUTOR_KILL_SWITCH=false`
- `CHAIN_ID=42161`
- the configured wallet exactly matches the signer loaded from the absolute
  `SIGNER_PRIVATE_KEY_FILE`
- `LIVE_EXECUTOR_EXECUTOR_CODE_HASH` is the reviewed lowercase SHA-256
  digest of the configured PhoenixExecutor runtime bytecode
- `LIVE_EXECUTOR_PNL_ASSET_ADDRESS` is canonical Arbitrum WETH
- the selected HTTPS RPC exactly matches `LIVE_EXECUTOR_RPC_ALLOWLIST`
- every gas, fee, input, profit, loss, polling, and timeout limit is positive
- `LIVE_EXECUTOR_ONE_TRANSACTION_AT_A_TIME=true`

The database control row must independently have `armed=true` and
`kill_switch=false`. Its installed defaults are `armed=false` and
`kill_switch=true`.

Production Compose mounts `LIVE_EXECUTOR_SIGNER_FILE` read-only at
`/run/secrets/phoenix-live-executor-signer`; it never passes the key through the
container environment. `SIGNER_PRIVATE_KEY` remains mutually exclusive,
local/test-only compatibility input.

The profile is not part of `compose.prod.yml`. A future reviewed release must
provide a digest-pinned `LIVE_EXECUTOR_IMAGE` and explicitly load the overlay:

```sh
docker compose \
  --env-file /etc/phoenix/phoenix.env \
  -f compose.prod.yml \
  -f compose.live-canary.yml \
  --profile live-canary config
```

This repository task does not publish that image, apply the service schema, arm
the service, or run the profile.

## Service-owned schema

`live-executor/schema/001_live_canary.sql` and
`live-executor/schema/002_approval_evidence.sql` are intentionally outside the
exact v5 root migration set. The runtime never installs or modifies schema. It
validates `phoenix.live-canary-schema.v2` at startup and fails closed when the
schema is absent.

Only rows in `live_canary.execution_requests` with `status='approved'`, complete
v2 approval evidence, a future approval deadline, and a matching canonical
approval digest can be claimed. The canonical digest binds the independently
verified simulation and plan hashes, route fingerprint, selected size, token
path, pinned block, executor identity, and calldata hash.

`approve-execution-request` is the only repository operator materializer. It
accepts one stored simulation result hash, bounded approval metadata, and the
exact `APPROVE_ONE_SIMULATED_PHOENIX_CANARY` confirmation. It reads the
canonical unsigned plan from PostgreSQL, rejects reverted, non-independent,
expired, or below-floor evidence, reconstructs PhoenixExecutor calldata, and
inserts one idempotent approved request. It has no calldata argument.

Before querying or allocating a nonce, `live-executor` recomputes the approval
digest and PhoenixExecutor calldata and requires the configured executor address
and code hash to match the approved evidence.

## State machine

```text
disarmed_shadow
  -> armed_idle
  -> claimed
  -> nonce_allocated
  -> pending
  -> confirmed

nonce_allocated -> submission_unknown -> disarmed_shadow
pending -> reverted  -> disarmed_shadow
pending -> replaced  -> disarmed_shadow
pending -> timed_out -> disarmed_shadow (hash reconciliation continues)
timed_out -> confirmed | reverted | replaced
any RPC or nonce failure -> disarmed_shadow
daily loss boundary -> disarmed_shadow
```

The transaction hash is persisted immediately after a successful RPC
submission and before receipt polling. A restart with a pre-hash active attempt
is treated as an unknown-submission integrity incident and disarms the canary.
Pending and timed-out hashes, plus durable nonce state, are recovered from
PostgreSQL. A timed-out hash remains the sole active canary obligation until a
receipt or replacement is proven, so manual re-arming cannot create a second
transaction around an unresolved first submission.

An RPC submission error, returned-hash mismatch, or crash after nonce
allocation can leave acceptance ambiguous. That request becomes
`submission_unknown`, remains the sole active canary obligation, and cannot be
cleared by the runtime. A reviewed operator reconciliation is required before
another canary can be claimed.

If a database kill switch or remaining-loss check rejects the request after
nonce reservation but before submission, the store releases that nonce only
when its durable next-nonce value still exactly matches the reservation.

Malformed receipt economics, a mismatched receipt hash, or missing settlement
evidence also disarms without clearing the submitted hash. This keeps
accounting failures fail-closed and prevents a second canary from bypassing an
unresolved first result.

Realized PnL is accepted only from one matching `OpportunitySettled` event
emitted by the configured PhoenixExecutor. The canary restricts settlement to
canonical Arbitrum WETH so realized profit and transaction fees are compared in
wei without a price-conversion ambiguity. Reverted receipt gas is persisted as
loss before the canary disarms. Before submission, the request's worst-case gas
fee is checked against the remaining UTC daily-loss budget.

Private keys and signed transaction bytes have redacted debug representations
and are never included in structured diagnostics.
