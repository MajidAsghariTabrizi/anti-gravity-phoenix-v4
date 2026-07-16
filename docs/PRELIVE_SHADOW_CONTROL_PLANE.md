# PRE-LIVE SHADOW Control Plane

## Scope

`prelive-shadow-control.sh` is the host-side controller for bounded and continuous
PRE-LIVE SHADOW observation. It does not deploy a release, create transaction
requests, access a wallet or signer, or submit transactions. It requires an
already authorized digest-pinned release and keeps these invariants fixed:

- `PHOENIX_MODE=SHADOW`
- `LIVE_EXECUTION=false`
- `execution_eligible=false`
- `execution_request_created=false`
- no configured signer, wallet, executor, or submission method

The exact modes are `15m`, `1h`, `6h`, `24h`, and `continuous`. Finite modes
cannot report completion before 900, 3600, 21600, or 86400 real seconds have
elapsed. Continuous mode reports `interrupted` only after an operator signal.

## Service Boundary

The protected data plane is:

- `nitro-feed-relay`
- `feed-ingestor`
- `nats`
- `postgres`
- `recorder`

The controller verifies those containers and the reviewed JetStream streams and
durable consumers before, during, and after a run. It never starts, stops,
recreates, or removes them. It also never runs broad Compose shutdown,
`--remove-orphans`, pruning, stream deletion, consumer reset, database
truncation, or volume removal.

Only `prometheus`, `rpc-gateway`, `shadow-dispatcher`, `phoenix-engine`, and
`dashboard` are controlled. They start one at a time with explicit
`up -d --no-deps` operations and stop in reverse order with an explicit service
list. The fork sandbox remains isolated and on demand.

## Preflight

Preflight fails closed unless all 18 reviewed checks pass. They cover canonical
Compose rendering, immutable release images and manifests, route-registry hash,
Arbitrum chain ID, SHADOW and blank execution configuration, positive RPC
budgets, PostgreSQL and NATS connectivity, JetStream identity, additive
migrations, Dashboard isolation, Prometheus configuration, and zero execution
activity. It records a PostgreSQL clock baseline and fingerprints protected
container and JetStream identity without retaining container IDs, mount names,
provider endpoints, addresses, or credentials.

The run then requires bounded positive-route evidence before continuous
sampling. Missing positive evidence is a blocker, not permission to inject a
fixture or weaken the funnel.

Database-backed Dashboard facts are clipped to the later of the requested
window and the preflight database-clock baseline. This prevents an older
database window from being compared with the current process-lifetime counters;
shorter historical coverage remains unavailable until real time elapses.

## Evidence Production

Each sample rechecks all service health, SHADOW safety, zero execution activity,
and protected identity. It collects:

- bounded Prometheus and repeatable-read PostgreSQL money-path facts
- direct NATS JetStream resource state and pending/redelivery counts
- normalized Docker health, image, start, exit, OOM, and restart evidence
- opaque route and provider identities
- profitability, verification, retry, persistence, database, and fork facts

Raw database route/provider identities and Docker details stay in a mode `0700`
temporary directory and are removed after finalization. The Dashboard receives
only generation-stamped, bounded, redacted JSON artifacts. Artifacts are
SHA-256 checked, promoted atomically, and retained in a bounded rolling set.
Unavailable historical windows, rate intervals, or percentile evidence remain
JSON `null` and create explicit Dashboard gate alerts; they are never replaced
with zero.

The final control evidence records the exact release and route hashes, preflight
results, PostgreSQL clock baseline, service states, protected identity,
first/last samples, funnel and operational metrics, bounded errors, and artifact
digests. It records the execution-request count at both the preflight and final
boundaries, and both values must remain zero. The final Dashboard snapshot
attaches that validated evidence bundle.

## Commands

Run only after repository, CI, release, manifest, rollback, and host-access gates
authorize the operation:

```sh
sudo -u phoenix /opt/phoenix/deploy/prelive-shadow-control.sh plan 15m
sudo -u phoenix /opt/phoenix/deploy/prelive-shadow-control.sh preflight 15m
sudo -u phoenix /opt/phoenix/deploy/prelive-shadow-control.sh run 15m
```

For continuous observation:

```sh
sudo -u phoenix /opt/phoenix/deploy/prelive-shadow-control.sh run continuous
```

Send `INT`, `TERM`, or `HUP` once to request controlled continuous shutdown.
Do not claim any finite stage unless the promoted evidence proves its full real
duration. A valid snapshot or `evidence_clear` Dashboard state is not LIVE
authorization and is not a PRE-LIVE milestone verdict.

## Local Verification

```sh
python3 -m py_compile scripts/prelive_shadow_control.py scripts/prelive_dashboard_live.py
python3 -m unittest scripts.tests.test_prelive_shadow_control scripts.tests.test_prelive_dashboard_live -v
sh -n scripts/prelive-shadow-control.sh scripts/prelive-shadow-control-tests.sh
sh scripts/prelive-shadow-control-tests.sh
```

Linux CI also executes the repeatable-read Dashboard SQL against the fully
migrated PostgreSQL schema. Real VPS execution and staged soak evidence belong
to the later authorized deployment and soak phases.
