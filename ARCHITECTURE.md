# Architecture

Phoenix v4 follows the requested event-driven architecture:

```text
Arbitrum sequencer feed
  -> local Nitro feed relay
  -> Go feed ingestor
  -> NATS Core subject phoenix.feed.tx
  -> Rust Phoenix engine
  -> constrained execution coordinator
  -> PhoenixExecutor
  -> async reconciliation
  -> PostgreSQL + Prometheus
  -> dashboard
```

Cold reads go through one `rpc-gateway`. The hot search loop performs zero external public RPC reads.

## Material Deviations

- Local typed message publishing uses canonical JSON matching `proto/phoenix.proto` because `protoc` and generated Protobuf bindings are unavailable in this workspace. The schema is present and is the intended deployment contract.
- The current V3 simulator implements integer segment simulation with strict completeness guards. Full TickMath/SqrtPriceMath parity must be completed and verified with official package references before LIVE.
- Sushi V3 production registry fields are configuration-only until official Sushi chain constants are verified.

## Extension Points

- DEX adapters are registry-backed.
- Graph route classes currently enable only two-pool cycles.
- Execution modes are separated from strategy logic.
- Replay uses the same origin, graph, simulation, optimizer, and profit modules as live mode.

