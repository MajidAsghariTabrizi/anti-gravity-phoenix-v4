# Fork Execution Sandbox

## Purpose

The Phase 6 sandbox turns one persisted, independently verified SHADOW
opportunity into an unsigned calldata plan and evaluates that plan with
`eth_call`, gas estimation, and a call trace against a block-pinned local Anvil
fork. It persists append-only counterfactual evidence. A passing result is not a
realized trade, a deployment result, or proof that Phoenix is ready for LIVE.

The sandbox is a standalone crate and an opt-in Compose profile. The production
Engine and `compose.prod.yml` do not reference it. It has no signing, nonce,
wallet, impersonation, transaction-send, public-broadcast, or contract-deployment
path.

## Fail-Closed Boundary

The planner rejects evidence unless all of these are exact:

- Arbitrum One chain ID, pinned block number and hash, and unexpired decision;
- accepted disposition, complete profitability evidence, and positive net PnL
  above both persisted and operator policy thresholds;
- two distinct providers with agreed block, route, and pool-state evidence;
- route, token, pool, router, protocol, direction, state-hash, amount, and gas
  fields that reconstruct consistently;
- explicit operator allowlists and bounded input, slippage, and calldata;
- `shadow_only=true`, `execution_eligible=false`, and
  `execution_request_created=false`.

The runner then verifies the Anvil fork identity, target bytecode, every pool's
static identity and dynamic state, gas bounds, trace integrity, settlement event,
balance-derived profit, gas cost, and net PnL. State drift fails before the first
call. Reverts are classified and persisted without being called successful.

## Isolated Run

The repository does not deploy `PhoenixExecutor` or any other target contract.
`FORK_TARGET_CONTRACT` must already contain reviewed bytecode in the selected
fork snapshot, and `FORK_TARGET_CODE_HASH` must be the lowercase SHA-256 digest
of those bytecode bytes. Missing or different code fails before pool reads.
`FORK_SIMULATION_FROM` is only the unfunded sender field for `eth_call`, not a
wallet or signer.

Export the required values in the operator shell without committing an env file:

```sh
export FORK_ANVIL_DIGEST='<reviewed-64-hex-digest>'
export FORK_POSTGRES_PASSWORD='<local-sandbox-password>'
export FORK_UPSTREAM_RPC_URL='<archive-capable-Arbitrum-RPC>'
export FORK_BLOCK_NUMBER='<persisted-pinned-block>'
export FORK_SHADOW_DECISION_ID='<persisted-decision-uuid>'
export FORK_ALLOWED_TOKENS='<comma-separated-addresses>'
export FORK_ALLOWED_POOLS='<comma-separated-addresses>'
export FORK_ALLOWED_ROUTERS='<comma-separated-addresses>'
export FORK_ALLOWED_PROTOCOLS='UniswapV3,SushiSwapV3'
export FORK_TARGET_CONTRACT='<reviewed-contract-address>'
export FORK_TARGET_CODE_HASH='<reviewed-bytecode-sha256>'
export FORK_SIMULATION_FROM='<unfunded-call-sender-address>'
export FORK_MINIMUM_NET_PNL='<settlement-token-base-units>'
export FORK_MAXIMUM_INPUT_AMOUNT='<settlement-token-base-units>'
export FORK_SLIPPAGE_BPS='100'
sh ./scripts/fork-sandbox-run.sh
```

The Compose topology publishes no host ports. Anvil alone can reach the upstream
fork endpoint. The sandbox shares Anvil's network namespace so its RPC URL is
loopback, while the database network is internal. The launcher rejects nonempty
signer, private-key, mnemonic, wallet, or executor environment variables and
requires a reviewed Anvil digest. Compose fixes the image repository and inserts
`@sha256:` structurally. The launcher builds both Phoenix-owned images, waits
for PostgreSQL and Anvil health, applies migrations, runs exactly one sandbox
evaluation, then stops the long-lived containers while retaining the local
evidence volume.

## Evidence And Limits

`fork_simulation_results` stores the canonical unsigned plan, result body and
hashes, fork identity, predicted and simulated economics, gas, revert reason,
and explicit false execution flags. Rows are insert-only through the sandbox.

Deterministic unit tests and PostgreSQL integration prove planner, runner,
persistence, and isolation contracts. A real archive-backed fork run remains a
separate operator evidence gate. This phase does not access a VPS, fund an
account, deploy bytecode, submit a transaction, or authorize LIVE execution.
