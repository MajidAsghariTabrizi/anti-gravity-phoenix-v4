# Production LIVE Runbook

The normal operator action is a reviewed merge to protected `main`. Successful
exact-main Phoenix CI invokes the automated controller; no laptop, environment
approval, SSH prompt, signer copy, or owner-authorization file participates.

## Automated sequence

1. Verify exact source CI and immutable build provenance.
2. Validate/reconcile active SHADOW or LIVE context without container mutation.
3. Verify the rollback release tree and compatibility.
4. Install the immutable candidate without changing active pointers.
5. Render the full LIVE overlay against a root-only temporary LIVE environment,
   proving the real environment is unchanged.
6. Prove two distinct RPC providers and exactly two positive weights, signer
   metadata, executor identity/configured-paused state, gas/nonce controls,
   protected services, route policy, disk, Docker, migrations, and the lock.
7. Mark mutation, apply migrations, install LIVE mode, and start RPC Gateway and
   Engine while the contract stays paused and live-executor stays stopped.
8. Burn in for at least 120 seconds with stable container IDs/restarts/readiness
   and unchanged process-fatal integrity evidence.
9. Arm autonomous controls, submit only the reviewed owner-unpause operation,
   reconcile its receipt/state/nonce, and start live-executor.
10. Verify LIVE controls, health, active context, and coherent pointers before
    recording `COMPLETED`.

No profitable opportunity is required during deployment. Capital limits, route
universe, owner, searcher, routers, pools, factory, and fee limits are not changed.

## Observing a release

Use the GitHub Deployment, controller Step Summary, and
`phoenix-release-evidence-<sha>`. Final evidence must show LIVE environment,
unpaused configured contract, armed controls, cleared kill switch, healthy
rpc-gateway/phoenix-engine/live-executor, stable restarts, and coherent
`current-release`/`release-assets.sha`.

The legacy `.github/workflows/deploy-autonomous-live.yml` is intentionally disabled.
Exact-SHA resume uses the controller’s `workflow_dispatch`, not the legacy workflow.
