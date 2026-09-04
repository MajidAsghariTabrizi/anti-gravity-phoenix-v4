# When Two RPC Providers Should Agree, Not Just Fail Over: An Agreement-First Pattern for Execution Authority

> **Status:** ARCHITECTURE + IMPLEMENTATION. Includes a worked walkthrough of Phoenix's `resolve_hunter_state`. **Profitability: NO VERIFIED ALPHA YET.**

> **Audience:** Engineers building financial-infrastructure backends that depend on blockchain reads for execution authority. Applies to MEV systems, liquidation systems, oracle consumers, keeper networks, and bridge relayers.

## The standard pattern

The most common production RPC pattern for blockchain financial systems is:

> "Use the fastest provider. If it errors or times out, fall over to the next one. Take the first response that returns."

This is an **availability** pattern. It optimizes for the system staying online when one provider goes down. The system trades correctness against latency — if the fastest provider returns a slightly stale or slightly forked view, the system uses it because the fallback never had a chance to disagree.

This is the right pattern for *observation* — systems that need to react to state but where the cost of an incorrect read is bounded (e.g., a dashboard, an alerting system, a UI showing the latest block). It is the wrong pattern for **execution** — systems where the read result determines whether real capital is committed on-chain.

## The disagreement problem

A single RPC provider, regardless of reputation, is a single point of trust for execution authority. The most common ways a single-provider view can diverge from the truth:

1. **Sequencer-side state propagation delay.** The provider's view may be a few hundred milliseconds behind the canonical state. For a liquidation at the boundary of an Aave health factor, a few hundred milliseconds is the difference between profitable and unprofitable.
2. **Mempool-level provider filtering.** Some providers filter transactions. A "missing" transaction is not necessarily a "never-submitted" transaction.
3. **Archive-node vs full-node discrepancies.** An archive provider may serve accurate historical state; a full-node provider may serve a state that was pruned.
4. **Provider-isolated reorgs.** Rare, but possible. A provider can temporarily serve a chain that has been reorged at the local view but not yet reconciled globally.

In all four cases, the failover pattern will *use* the incorrect response. The second provider is only consulted when the first errors, not when the first might be wrong.

## Phoenix's pattern: agreement-or-fail-closed

Phoenix's `rpc-gateway` crate treats execution authority as **agreement-first**. The contract is:

> Two independent providers must agree on the critical finalized state before any execution authority exists. If they disagree, execution authority is closed until fresh agreement returns.

The key word is **independent**. The pattern is not "primary + fallback." It is "primary + verification." Both providers must produce the same answer for the same query. If they do not, neither answer is trusted.

## Code walkthrough: `resolve_hunter_state`

The full path is `rpc-gateway/src/runtime.rs` line 786-880. The relevant part:

```rust
pub async fn resolve_hunter_state(
    &self,
    request: HunterStateRequest,
) -> Result<HunterStateResponse, GatewayError> {
    // ... request validation, head resolution ...

    let primary = self
        .reserve_named_provider(&head.provider_id)
        .await
        .ok_or(GatewayError::ProviderUnavailable)?;
    self.ensure_provider_verified(&primary)
        .await
        .map_err(map_call_failure)?;
    let excluded = HashSet::from([primary.provider_id().to_string()]);
    let secondary = self
        .reserve_provider(&excluded)
        .await
        .ok_or(GatewayError::ProviderUnavailable)?;
    self.ensure_provider_verified(&secondary)
        .await
        .map_err(map_call_failure)?;

    let primary_states = self
        .perform_hunter_state_bundle(&primary, &request, &head.block, ProviderSlot::Primary)
        .await?;
    let secondary_states = self
        .perform_hunter_state_bundle(&secondary, &request, &head.block, ProviderSlot::Secondary)
        .await?;

    let agreements = primary_states
        .into_iter()
        .zip(secondary_states)
        .map(|(primary_state, secondary_state)| {
            let agreement = ProviderStateAgreement {
                primary_provider_id: primary.provider_id().to_string(),
                secondary_provider_id: secondary.provider_id().to_string(),
                primary: primary_state,
                secondary: secondary_state,
            };
            agreement.agreed().map_err(map_hunter_contract_error)?;
            Ok(agreement)
        })
        .collect::<Result<Vec<_>, _>>()?;
    // ... cache and return ...
}
```

The flow:

1. The current head provider is selected as the `primary`. This is the provider that reported the current head.
2. A `secondary` provider is reserved from the pool with the primary **explicitly excluded** by provider ID. The two providers are required to be different identities.
3. Both providers are verified before any state query (chain ID, block freshness, archive capability — see `ensure_provider_verified`).
4. The same state bundle (`perform_hunter_state_bundle`) is requested from both, independently.
5. The two result vectors are zipped element-wise. For each pair, `agreement.agreed()` is called. The `ProviderStateAgreement` struct enforces exact equality on the critical fields (block number, block hash, account state, oracle prices, etc.).
6. If any pair disagrees, the call returns `GatewayError::ProviderDisagreement`. This is HTTP 502 (`http_status` returns 502 for `ProviderDisagreement`).
7. If all pairs agree, the response contains both providers' agreement evidence. The result is *cached* per `(request_hash, head_block_number, head_block_hash)` for 5 seconds (`HUNTER_STATE_CACHE_TTL`).

The disagreement case is not a fallback to whichever provider is faster. It is a structured error that the upstream caller is required to handle.

## The single-provider path is also explicit

Phoenix also has a single-provider endpoint: `resolve_aave_primary_screen` (`rpc-gateway/src/runtime.rs` line 936-982). It returns discovery-only data — bounded information used by the hunter to identify *candidates* for further investigation.

The source comment is unusually explicit:

```rust
/// Returns bounded discovery data from the highest-priority healthy
/// provider only.  This endpoint has no execution authority: callers must
/// obtain a fresh single-primary Aave Exact result before a candidate can be
/// persisted or submitted.
```

The single-provider endpoint *cannot be used* to authorize execution. A candidate discovered through `resolve_aave_primary_screen` must subsequently go through a separate authoritative path that performs the dual-provider agreement. The single-provider read is for *triage* only.

## The Aave Exact path: deeper than block identity

For Aave liquidations, the agreement is not just "do both providers report the same block." The agreement extends to:

- The exact finalized block number and hash.
- The user's Aave account state (collateral, debt, health factor, emode category).
- The user's reserve configuration (collateral enabled, borrowing enabled, LTV set).
- The reserve's underlying configuration (LTV, liquidation threshold, liquidation bonus).
- The oracle price for the collateral and debt assets.
- The flash-loan premium configuration.
- The state root of the finalized block.

A disagreement on any of these closes execution authority for that specific borrower.

## What this costs

The agreement pattern is more expensive than the failover pattern:

- **Latency.** Two providers must respond. Phoenix's `MethodTimeouts` and `MAX_STATE_RESOLUTION` (25 seconds) bound the worst case, but the typical case is bounded by the slower provider.
- **RPC budget.** Two providers per authoritative read means double the upstream calls. The `GlobalBudget` per minute (`GatewayLimits::state_requests_per_minute = 12`) and per second (`upstream_calls_per_second = 1`) are calibrated to this cost.
- **Cache invalidation.** A disagreement invalidates cached agreements. The cache TTL is 5 seconds, which is short enough that disagreements self-heal quickly when the providers re-sync.

The benefits are:

- **No silent incorrectness.** If a provider diverges from canonical state, the divergence is detected before execution authority is granted.
- **No silent reorg acceptance.** A provider that reorgs locally will disagree with the secondary. The system waits for re-synchronization.
- **Observable disagreement.** Provider disagreement is a Prometheus metric (`RuntimeRpcMetrics`). Operations can alert on it.

## The error class

`GatewayError::ProviderDisagreement` (`rpc-gateway/src/runtime.rs` line 329-330):

```rust
#[error("RPC Gateway providers disagree on canonical Hunter state")]
ProviderDisagreement,
```

The class string is `"provider_disagreement"`. It is `http_status() = 502`. It is **not** `retryable()` — `retryable` returns `false` for `ProviderDisagreement`. The caller is expected to handle the disagreement explicitly: refresh head, recompute, try a different operation. Not to blindly retry.

This is intentional. Retry-on-disagreement would just produce more disagreements faster. The system must wait for the providers to converge.

## What this is not

This is not a claim that dual-provider agreement is always better than failover. There are operational regimes where it is wrong:

- When both providers are operated by the same upstream entity (they may share infrastructure and the same failure mode).
- When the operation is read-only and the cost of an incorrect read is bounded.
- When the budget cannot tolerate the latency cost.

Phoenix uses it specifically for *execution authority reads* on revenue lanes. Discovery uses single-provider (with the explicit non-authority annotation). Operational dashboards use single-provider. The agreement pattern is reserved for the small fraction of reads that determine whether capital is committed.

## Reviewable evidence

- `rpc-gateway/src/runtime.rs` line 786-880 — `resolve_hunter_state`, full agreement path.
- `rpc-gateway/src/runtime.rs` line 317-376 — `GatewayError` enum, class strings, HTTP status mapping, `retryable` policy.
- `rpc-gateway/src/hunter_state.rs` — `ProviderStateAgreement`, `agreed()` method.
- `rpc-gateway/src/budget.rs` — `GlobalBudget` and per-minute/per-second limits.
- `docs/RPC_BUDGET.md` — the budget policy in human-readable form.

## For your own system

If you are evaluating whether to adopt this pattern, the questions to ask:

1. **What reads have execution authority, and what reads are observation-only?** The answer determines where agreement is required and where failover is sufficient.
2. **Are your two providers operationally independent?** Independent in the sense that they have different upstream entities, different geographic locations, and different operational teams. Two endpoints of the same provider are not independent.
3. **Can your system tolerate the latency cost?** Agreement is at least 2x the latency of failover. For some operations, this is fatal.
4. **Is your disagreement-handling code path tested?** A disagreement handler that has never been exercised in production is a handler that has never been verified to work. Phoenix's integration tests cover the disagreement path explicitly.

If the answer to all four is "yes," the pattern is appropriate. If any answer is "no," it is not.