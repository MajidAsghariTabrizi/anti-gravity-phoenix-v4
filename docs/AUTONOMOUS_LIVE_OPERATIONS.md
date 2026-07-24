# Autonomous LIVE Operations

Phoenix autonomous LIVE uses the `live-autonomous` Compose profile and the
existing `live-executor` release image. It does not reuse the bounded
`live-canary` profile.

## Release and deployment authority

Only `.github/workflows/deploy-autonomous-live.yml` may start the final
deployment. Its inputs bind the current merged `main` SHA, the successful
seven-image build run, and the active immutable rollback release and build
run. It revalidates source CI, release provenance, release assets, strict SSH
host identity, and the active rollback identity before invoking the
root-owned, digest-constrained gateway.

After merged-main CI and the immutable release build succeed, an authorized
administrator may install the gateway once from that verified release tree:

```text
sudo /bin/sh scripts/install-autonomous-live-deploy-gateway.sh
```

The `phoenix` account receives permission to invoke only that exact gateway
digest. It does not receive a general root shell. Missing gateway
installation or SSH access is `EXTERNAL_VPS_ACCESS_REQUIRED`; missing signer,
RPC, owner, or gas prerequisites use their corresponding exact external
blocker class.

## Activation order

Deployment is permitted only from an immutable seven-image release built from
the merged `main` SHA after required main-push CI succeeds.

The constrained deployment path performs these operations in order:

1. Verify release and rollback identities, immutable release assets, host and
   protected-container identities, PostgreSQL/NATS identity, RPC chain
   identity, signer-file metadata, wallet gas, executor runtime code hash,
   owner, flash provider, and executor configuration.
2. If owner configuration is incomplete, run
   `autonomous-live-control owner-plan`. The emitted
   `phoenix.executor-owner-plan.v1` contains only unsigned target, value,
   calldata, chain, expected post-state, and the verification command. Stop
   with `EXTERNAL_OWNER_AUTHORIZATION_REQUIRED`.
3. Install the LIVE operator-mode flags atomically without exposing or
   changing secret values.
4. Apply service-owned migrations through v4 and verify schema identity.
5. Start the digest-pinned application services while preserving feed,
   NATS, PostgreSQL, and recorder container identities.
6. Atomically arm the one-route global and route controls with one active
   attempt, nonzero size/loss limits, three-loss cutoff, and immediate
   disarm for unknown submission or integrity failure.
7. Start the continuous executor and observe health, event metrics, controls,
   and reconciliation state. Do not inject an event or transaction.

There is no production dry run, smoke, SHADOW soak, manual Canary,
one-transaction test, signerless start, or executor-disabled start. The first
transaction must come from a naturally occurring event that survives the
committed LIVE policy.

## Rollback

`rollback-release.sh` is the exact rollback entrypoint. It:

1. sets the global kill switch and disarms autonomous claims;
2. leaves the executor running for a bounded receipt-reconciliation interval;
3. stops the executor when reconciliation completes or the timeout elapses;
4. restores SHADOW operator-mode flags atomically;
5. verifies and installs the previous immutable release;
6. restarts only replaceable application services and verifies protected
   container identities did not change.

Candidate, request, attempt, attribution, and outcome history are retained.
Successful chain transactions are never reversed. Reviewed on-chain
allowlists may remain configured; rollback does not issue an owner
transaction.

## Observational verification

After start, verification is limited to process health, real event counters,
block-state movement, control state, signer/nonce integrity indicators, and
receipt reconciliation. Operators must not publish a synthetic event or
force a transaction.
