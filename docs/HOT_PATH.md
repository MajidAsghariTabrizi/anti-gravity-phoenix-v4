# Hot Path

Phoenix v4 hot path:

1. Nitro relay receives ordered sequencer feed messages.
2. Go feed ingestor normalizes ordered L2 transactions and publishes `phoenix.feed.tx`.
3. Phoenix engine decodes supported origin calldata.
4. Engine maps deterministic paths to touched configured pools.
5. Pool graph returns only affected two-pool cross-DEX cycles.
6. Engine requests bounded primary evidence only for the matched route and canonical block.
7. RPC Gateway reads both pools through one explicit-block Multicall3 and verifies the block hash.
8. Engine locally simulates the route with integer AMM math and evaluates dynamic amounts.
9. Routes clearly below the existing economics threshold stop without secondary RPC evidence.
10. Potentially policy-passing routes receive one same-block secondary Multicall3 verification.
11. SHADOW policy records accepted or fail-closed evidence; state-only evidence remains non-executable.

Forbidden in the synchronous hot decision path:

- direct external public RPC reads outside the bounded RPC Gateway evidence verifier
- RPC-based opportunity discovery or broad pool scanning
- per-candidate chain-ID, static metadata, or unpinned `latest` reads
- database calls
- dashboard calls
- blocking filesystem writes
- synchronous log flushes
- Python processes
- unbounded queues

State model:

- `canonical_reconciled_state`: last state validated through cold reconciliation.
- `feed_projected_state`: speculative projection from deterministic supported origin transactions.
- `opportunity_snapshot_state`: immutable state captured with an opportunity.

If simulation enters unknown tick coverage, the candidate is rejected with `STATE_INCOMPLETE`. Unknown tick state is never extrapolated.

Metric guard:

- `hot_path_external_rpc_calls_total` must stay at zero in production.
