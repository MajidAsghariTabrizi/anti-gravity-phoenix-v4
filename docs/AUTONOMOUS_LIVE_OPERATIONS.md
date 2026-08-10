# Autonomous LIVE Operations

Phoenix autonomous LIVE uses the `live-autonomous` Compose profile and the
existing `live-executor` release image. It does not reuse the bounded
`live-canary` profile.

## Release and deployment authority

Only `.github/workflows/phoenix-release-controller.yml` may start the final
deployment. Successful exact-main Phoenix CI invokes it automatically. Its
inputs and evidence bind the current merged `main` SHA, the successful
seven-image build run, and the active immutable rollback release and build
run. It revalidates source CI, release provenance, release assets, strict SSH
host identity, and the active rollback identity before invoking the
root-owned, digest-constrained gateway through a bounded forced command.

After merged-main CI and the immutable release build succeed, an authorized
administrator may install the gateway once from that verified release tree:

```text
sudo /bin/sh scripts/install-phoenix-release-platform.sh \
  --release-sha <exact-merged-sha> \
  --reuse-existing-deploy-key --reuse-existing-rpc-provider-secret
```

The explicit RPC reuse mode succeeds only when the approved root-owned
credential file already passes its path, type, owner, group, mode, link-count,
size, and content-shape checks. The protected controller supplies first-install
credential bytes once on stdin; they never belong in this command or an
environment file.

The `phoenix-deploy` account receives permission to invoke only that exact gateway
digest. It does not receive a general root shell, SCP, PTY, forwarding, or arbitrary
SSH command. Missing gateway
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
   `phoenix.executor-owner-plan.v3` contains only unsigned target, value,
   calldata, chain, expected post-state, and the verification command. The
   plan includes every verified native-USDC/WETH Uniswap V3 unwind asset and
   pool required by the current Aave route registry. Stop with
   `EXTERNAL_OWNER_AUTHORIZATION_REQUIRED`; configuration remains a separate
   paused, explicitly acknowledged owner operation.
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

## Explicit MAX_REVIEWED revenue size authority

`autonomous-live-control set-revenue-size-max-reviewed` is the only operator
override for skipping outcome-based size promotion. It requires the exact
acknowledgement
`PHOENIX_SET_REVENUE_SIZE_ACK=SET_MAX_REVIEWED_LIVE_SIZE_42161` and accepts
only the canonical reviewed maximum of `10000000000000000` wei. It is not an
activation command: both Aave/Atlas revenue lanes and all generic execution
controls must remain disarmed, the executor must be fully configured and
paused, the global submission lock and all active/unresolved work must be
empty, daily loss must remain below its unchanged limit, and the live Gateway
and Aave/Atlas hunter must report fresh exact dual-provider readiness with a
closed provider circuit.

The command atomically changes only `economic_control.current_size_level`,
`current_input_wei`, `control_epoch`, and the corresponding transition record.
The phase deliberately remains `DISARMED_EVIDENCE`; it does not claim that the
lanes are live. A later, separately acknowledged `arm-revenue-lanes` copies
the exact economic size into both lane controls. Owner configuration, owner
unpause, and executor start remain separate explicit operations.

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
