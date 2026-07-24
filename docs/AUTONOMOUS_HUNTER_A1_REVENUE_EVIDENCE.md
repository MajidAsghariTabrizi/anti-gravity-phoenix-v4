# Phoenix Autonomous Hunter A1: Discovery and Simulation Evidence

## Scope

A1 adds the SHADOW/dry-run discovery and exact-simulation core inside the
existing `phoenix-engine` and `rpc-gateway` components. It does not add an
image, approve an execution request, load a signer, reserve a nonce, submit a
transaction, arm a database, change `PhoenixExecutor.sol`, or perform a VPS or
on-chain operation.

The bounded flow is:

```text
pool-affecting event
-> affected-route index
-> independently agreed pinned pool states
-> exact multi-leg V3 simulation
-> deterministic bounded size optimization
-> conservative net-PnL filter
-> canonical AutonomousCandidateV1 materialization
```

The materializer exposes only `Shadow` and `DryRun` modes. Its candidate sink is
idempotent by canonical candidate hash and cannot create an approval or an
execution request.

## Route breadth

The Engine now constructs a deterministic directed multigraph from the A0
`RouteUniverseV1` contract. A directed edge binds factory, pool, protocol,
token-in, token-out, fee, tick spacing, and swap direction. Enumeration:

- supports multiple pools for the same pair and multiple fee tiers;
- supports cross-factory V3-compatible edges;
- enumerates simple two-, three-, and four-leg settlement cycles;
- never reuses a pool or intermediate asset inside a cycle;
- deduplicates semantic routes independently of input ordering;
- binds enabled routes to a canonical `RoutePolicyV1`;
- indexes every pool to the bounded ordered routes that contain it.

The committed reviewed universe remains intentionally narrow: the already
reviewed Arbitrum One WETH/native-USDC Uniswap V3 pools at fee tiers 500 and
3000. It yields two enumerable directed cycles, while the existing 500->3000
route is the only SHADOW-enabled policy. No unverified pool, asset, factory, or
router was promoted, and LIVE remains disabled.

## Exact V3 state and simulation

`rpc-gateway::hunter_state` defines a strict block-keyed state contract that
binds:

- chain, block number, and block hash;
- pool, factory, protocol, token, fee, and tick-spacing identity;
- `slot0` square-root price and current tick;
- active liquidity;
- bounded tick bitmap words;
- ordered initialized ticks with `liquidityGross` and signed `liquidityNet`;
- the covered tick interval;
- a canonical full-state hash.

Primary and secondary providers must produce byte-equivalent state contracts.
The bounded cache keys state by pool, block number, and block hash.

The Engine uses Solidity-compatible integer rounding, per-step fee removal,
square-root price movement, initialized-tick traversal, direction-correct
liquidity-net application, exact price-impact measurement, and policy rejection
above the maximum impact. Reaching an unproven tick boundary returns
`hunter_state_incomplete`; it never approximates beyond supplied evidence.
Current-range vectors remain semantically identical in both directions, and
`fixtures/hunter-a1/v1/pinned-fork-cross-tick.json` pins the complete state,
expected outputs, crossing counts, and price impact for initialized-tick
traversal in both directions. Its provenance explicitly identifies it as a
synthetic offline parity vector, not production or realized-profit evidence.

## Sizing and economics

The optimizer applies the strictest of the route-policy maximum, settlement
asset maximum, universe hard cap, and A1 SHADOW cap. It evaluates a bounded
geometric ladder, refines around the best observed region, and emits at most one
candidate per route/event/block. Ties choose the smaller input.

For every input, leg outputs are sequential. Exact output movement makes price
impact part of the objective and a hard policy filter. Crossing count increases
both the bound gas estimate and a configured per-crossing cost. The conservative
decision is:

```text
gross profit
- flash premium
- gas cost
- ordering-cost reserve
- model-error reserve
```

Only a result strictly above zero and the route's retained-profit floor becomes
an `AutonomousCandidateV1`. Events older than the policy quote or candidate-age
limit are rejected before materialization. The plan binds every leg output, minimum output,
pool-state hash, tick-crossing count, block identity, route and policy identity,
economic reserve, unsigned calldata hash, and executor identity.

## Bounded deterministic fixture evidence

The committed report is
`fixtures/hunter-a1/v1/revenue-replay-evidence.json`. It is fixture/replay
evidence, not a production-profit claim.

| Metric | Fixture result |
| --- | ---: |
| Baseline configured routes | 1 |
| Enumerable reviewed-universe routes | 2 |
| SHADOW-enabled routes | 1 |
| Baseline/reviewed pools | 2 / 2 |
| New reviewed pools | 0 |
| Events processed | 3 |
| Affected routes evaluated | 1 |
| Qualified canonical candidates | 1 |
| Candidate rate | 3333 bps |
| Positive conservative net PnL, p50/p95 | 263211006474615 / 263211006474615 base units |
| Evaluation latency, p50/p95 | 38900 / 7637400 ns |
| RPC state reads per event | 2, 0, 0 |
| State-incomplete rate | 0 bps |

The duplicate event emitted no second candidate, and the unrelated pool event
evaluated no route. The replay does not claim a live-fork prediction-error
value; that field is explicitly null. Exact cross-tick parity is evidenced by
the committed bounded pinned-state fixture, while future controlled fork
observations can populate the metric without changing the candidate contract.

## Expected revenue effect

The capability can expose revenue that the prior single configured route and
current-range-only path could miss:

- fee-tier dislocations between multiple pools for one pair;
- same-pair cross-pool cycles in either direction;
- triangular and four-leg cycles when reviewed assets and pools are added;
- opportunities crossing initialized ticks;
- a better retained-profit point than a single fixed input;
- less hot-path work because unrelated events select no routes.

These are capability expectations. A1 does not assert realized production
profit and does not activate LIVE execution.

## Explicit bounds

A1 fails closed on configured maxima for assets, pools, routes, cycles per
settlement asset, routes per pool, affected routes per event, tick words,
initialized ticks, tick crossings, size probes, local refinements, concurrent
evaluations, candidate outputs, cached states, and event/route dedupe keys.
Error codes are bounded labels and metrics never include addresses or route
fingerprints as labels.
