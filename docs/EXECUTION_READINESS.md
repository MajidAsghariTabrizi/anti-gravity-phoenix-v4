# Execution Readiness

## Verdict

Execution is not ready and is not enabled. Existing code is sufficient for review and SHADOW contract modeling only.

## Wallet

Implemented: no funded wallet or runtime wallet dependency.

Missing before canary: isolated key custody design, dedicated low-balance account, configurable balance floor/ceiling, chain-ID enforcement at signing, destination/selector/value validation, audited funding and withdrawal procedure, and independent monitoring.

## Signer

Implemented: none. SHADOW and simulation require no signer.

Missing before canary: isolated signer process or hardware-backed custody, one-way request schema, secret loading that never enters logs/environment diagnostics, Arbitrum signing-domain checks, nonce/destination/calldata/value/gas validation, authorization policy, audit trail, and failure isolation.

## Executor Service

Implemented: `ExecutionCoordinator` defaults to SHADOW and only derives LIVE when both mode and `LIVE_EXECUTION` permit it. It does not sign or submit.

Missing before canary: durable nonce manager, pending transaction state, replacement policy, gas cap, submission timeout, sequencer/provider endpoint policy, receipt confirmation depth, revert-data handling, reconciliation, circuit breaker, manual arm/disarm, automatic SHADOW fallback, and loss-budget enforcement.

## Solidity Executor

Implemented controls:

- owner and authorized-searcher access control;
- two-step ownership transfer;
- pause control;
- approved asset, factory, and pool registries;
- factory/pool token and fee validation;
- Aave callback sender/initiator/context validation;
- V3 callback sender/factory/token validation;
- route continuity and maximum four legs;
- per-leg minimum output;
- deadline;
- baseline balance, flash repayment, and minimum-profit guard;
- top-level reentrancy guard and atomic revert;
- zero-before-set repayment approval;
- owner-only rescue.

Missing evidence and controls:

- no verified Arbitrum deployment or bytecode hash;
- no fork tests against pinned deployed Aave/V3 contracts;
- no invariant/fuzz suite for balances, callbacks, approvals, and arbitrary leg combinations;
- no verified token behavior allowlist for fee-on-transfer, rebasing, callback, or non-standard approval tokens;
- fixed extreme V3 price limits are not an explicit per-leg price-limit policy;
- leg `amountIn` values are not proven equal to the prior leg's actual output and can consume pre-existing intermediate balances;
- owner/searcher custody and governance are unspecified;
- flash-provider changes and emergency operational procedures are unaudited;
- rescue authority is centralized;
- no independent security audit.

## Required Review Order

1. Finish and validate the SHADOW data/state/simulation loop.
2. Verify protocol addresses and token behavior from primary deployment sources.
3. Add deterministic fork, fuzz, and invariant tests.
4. Resolve every high/medium contract risk in the register.
5. Review deployment bytecode and configuration.
6. Design signer, nonce, risk, and reconciliation services in a separate release.
7. Satisfy all SHADOW evidence gates before proposing a manually armed canary.
