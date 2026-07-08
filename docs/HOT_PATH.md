# Hot Path

Phoenix v4 hot path:

1. Nitro relay receives ordered sequencer feed messages.
2. Go feed ingestor normalizes ordered L2 transactions and publishes `phoenix.feed.tx`.
3. Phoenix engine decodes supported origin calldata.
4. Engine maps deterministic paths to touched configured pools.
5. Pool graph returns only affected two-pool cross-DEX cycles.
6. Engine clones the relevant state snapshot and locally simulates the origin.
7. Engine locally simulates backrun routes with integer AMM math.
8. Optimizer evaluates dynamic amounts.
9. Profit model gates expected net profit.
10. Execution coordinator records or submits depending on mode.

Forbidden in the synchronous hot decision path:

- external public RPC reads
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

