# Phoenix Autonomous Hunter v1 contracts

## Scope

A0 defines data contracts only. It does not discover routes, materialize a
candidate at runtime, claim work, load a signer, reserve a nonce, sign, submit,
reconcile a receipt, mutate `PhoenixExecutor`, change a VPS, arm a database, or
perform an on-chain operation.

The existing manual Canary v2 request and approval path remains unchanged.
Autonomous Hunter records extend its service-owned `live_canary` PostgreSQL
schema so later PRs can use one execution authority rather than create a
competing ledger.

## Canonical artifacts

`schemas/phoenix-autonomous-hunter-v1.schema.json` is a strict JSON Schema
Draft 2020-12 union. Every variant rejects unknown fields and has bounded
arrays, strings, and numeric ranges.

| Contract | Schema version | Canonical hash field |
| --- | --- | --- |
| Route universe | `phoenix.route-universe.v1` | `universe_hash` |
| Route policy | `phoenix.route-policy.v1` | `policy_hash` |
| Global control | `phoenix.autonomous-global-control.v1` | `control_hash` |
| Per-route control | `phoenix.autonomous-route-control.v1` | `control_hash` |
| Risk snapshot | `phoenix.risk-snapshot.v1` | `risk_snapshot_hash` |
| Submission quote | `phoenix.submission-quote.v1` | `quote_evidence_hash` |
| Autonomous candidate | `phoenix.autonomous-candidate.v1` | `candidate_hash` |
| Automatic approval | `phoenix.automatic-approval.v1` | `automatic_approval_digest` |
| Outcome attribution | `phoenix.outcome.v1` | `outcome_hash` |

Economic and on-chain quantities are canonical decimal strings. Block numbers,
bounded counts, basis points, and versions are JSON integers. Binary floating
point is forbidden. Addresses and digests are lower-case.

The bounded release universe is
`config/phoenix-route-universe-v1.json`. A0 includes only the Arbitrum One
Uniswap V3 WETH/native-USDC identities already reviewed in
`docs/DEPENDENCIES.md` and
`fixtures/routes/arbitrum_uniswap_v3_pool_proofs.json`. Its presence does not
approve or modify an on-chain allowlist.

## Canonical hashing

`scripts/hunter_contracts.py` implements `phoenix.canonical-json.v1`.

For a contract:

1. Reject duplicate object keys, floats, NaN, and infinities.
2. Remove only that contract's top-level canonical hash field.
3. Serialize UTF-8 JSON with lexicographically sorted object keys, no
   insignificant whitespace, and unescaped Unicode.
4. Prefix the bytes with:

   ```text
   phoenix.canonical-json.v1:<contract-domain>:<schema-version>\n
   ```

5. Compute lower-case SHA-256 without a `0x` prefix.

Arrays remain ordered. Reordering a route, path, pool, channel, or evidence
element changes the digest. A digest also changes when any size, economic,
state, control, quote, executor, or calldata binding changes.

In v1, `RoutePolicyV1` contains the complete immutable route risk limits.
Accordingly, an autonomous candidate's `risk_policy_hash` must equal its
`route_policy_hash`. A separately versioned risk-policy artifact may replace
that alias only in a later schema version.

## Binding graph

```text
RouteUniverseV1
  -> RoutePolicyV1
     -> GlobalControlV1 + RouteControlV1
        -> RiskSnapshotV1
SubmissionQuoteV1
  -> AutonomousCandidateV1
     -> AutomaticApprovalV1
        -> OutcomeV1
```

The automatic approval is not an operator acknowledgement. Its
`approval_source` is fixed to `autonomous_policy`, and it binds the candidate,
universe, policy, risk snapshot, quote, state, plan, simulation, calldata, and
executor identities. Runtime creation begins in A2; A0 supplies only the
contract.

`OutcomeV1` permits exactly one of the bounded outcome classes in the master
program. It retains predicted economics separately from realized chain and
business economics. The canonical equations are:

```text
realized_chain_net_pnl =
    realized_gross_profit
  - actual_gas_cost
  - actual_ordering_cost

realized_business_net_pnl =
    realized_chain_net_pnl
  - allocated_infrastructure_cost
```

## Control state

Global and per-route controls are separate canonical objects. Both use an
epoch and a digest so a risk snapshot binds one exact control generation.

The database defaults are fail closed:

```text
global armed = false
global kill_switch = true
global execution_mode = disabled
no route control rows
```

An open global `live` state requires `armed=true`, `kill_switch=false`, and no
disarm reason. An open route requires `enabled=true`, `kill_switch=false`, and
no disarm reason. Automatic re-enable behavior is not implemented in A0.

## Additive PostgreSQL contract

`live-executor/schema/003_autonomous_hunter_contracts.sql` adds:

- `live_canary.autonomous_global_control`;
- `live_canary.autonomous_route_controls`;
- `live_canary.autonomous_candidates`;
- `live_canary.autonomous_approvals`;
- `live_canary.autonomous_outcome_attributions`.

The migration is forward-only, retains v1 and v2 manual Canary schema
identities, uses restrictive foreign keys, and persists the canonical JSON
alongside indexed identity columns. It does not add triggers, background work,
claiming, nonce allocation, signing, or submission.

Candidate status names reserve the complete program state machine, but A0 does
not transition them. Claim concurrency and nonce ownership remain A3 work.

## Fixtures and validation

`fixtures/autonomous-hunter/v1` contains one valid fixture per contract and
targeted invalid fixtures for duplicate identities, broken routes, unsafe
controls, hash drift, ordering-cap violations, unknown fields, approval
mutation, and an unbounded outcome class.

Run:

```text
python3 scripts/hunter_contracts.py validate-fixtures
python3 -m unittest scripts.tests.test_hunter_contracts
```

The fixture validator also checks the cross-contract binding graph and proves
that the release route universe is byte-equivalent as JSON to its valid
fixture.

## Release and compatibility

The schema, universe, fixtures, validator, architecture document, and
service-owned SQL migration are immutable release assets. They do not add an
eighth image. No contract source or bytecode changes in A0. No protected
service source changes in A0.

Rollback is compatibility-only: old binaries continue to require and find the
v2 schema row, while the additive v3 tables remain unused. Removing applied
tables is not a supported rollback.
