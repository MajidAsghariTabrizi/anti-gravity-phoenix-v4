# PRE-LIVE SHADOW Dashboard

## Purpose

The Dashboard is a read-only view of bounded PRE-LIVE SHADOW evidence. It does
not query PostgreSQL, Prometheus, NATS, RPC providers, Docker, or application
services. A missing, stale, malformed, contradictory, or unsafe snapshot is a
blocked evidence state; it is never converted to an empty table or a zero.

All financial values are counterfactual or fork-simulated. Realized PnL is
always shown as `0 / not applicable in SHADOW`.

## Security Boundary

The production Dashboard:

- receives no production env file, database connection string, provider
  endpoint, credential, wallet, signer, or executor setting
- has no Docker socket and no container control path
- mounts only `/opt/phoenix/evidence/dashboard` at `/evidence` read-only
- runs with a read-only root filesystem, a bounded temporary filesystem,
  dropped capabilities, and no-new-privileges
- publishes Streamlit only on `127.0.0.1:8501`
- has no database mutation, execution-request, signing, or submission control

Use an SSH tunnel for operator access. Prometheus remains internal and is not
queried by the Dashboard.

## Snapshot Contract

`/evidence/latest-dashboard.json` must use schema identifier
`phoenix.prelive.dashboard.v1`. The parser limits the snapshot to 2 MiB, rejects
duplicate JSON keys and non-finite numbers, and requires complete bounded
sections for:

- executive safety and business evidence
- opportunity funnel and drop-off accounting
- profitability, cost, trend, model, and prediction-error evidence
- opaque route intelligence and component scores
- normalized service health and immutable image metadata
- feed completeness and bounded message-kind counts
- logical RPC provider IDs and independent-verification evidence
- JetStream streams, consumers, backlog, and Recorder persistence
- PostgreSQL readiness, growth, WAL, migration, and retention evidence
- retries, quarantine, integrity, restart, and protected-identity evidence
- isolated fork simulation evidence
- bounded report metadata and redacted structured logs

Route IDs are fixed opaque identifiers. Provider URLs, transaction hashes,
addresses, source identities, private payloads, and arbitrary route identities
are rejected. Logs are capped at 500 rows and 512 characters per message.

Cross-section accounting is checked before display. Funnel counts and drop-offs
must balance; business, route-sample, and fork totals must agree; required
services must appear exactly once; and on-demand fork state must remain separate
from continuously expected services.

## Alerts And Gate State

The Dashboard derives alerts from snapshot facts. It does not accept a supplied
healthy or go-live flag. Blocking or review conditions cover:

- a mode, lock, execution, sensitive-setting, or submission-method breach
- stale evidence or clock skew
- protected service, Engine, RPC Gateway, image, or route-registry failure
- JetStream resource loss or sustained backlog growth
- low PostgreSQL disk headroom
- feed incompleteness or unsupported-message volume
- verification disagreement or self-verification risk
- integrity failure, restart loop, or protected-identity drift
- fork contract-guard or prediction-error policy failure

`evidence_clear` means only that the bounded snapshot has no active alert
condition. It is not a LIVE authorization or a PRE-LIVE milestone verdict.

## Evidence Promotion

Available report files must be in the same evidence directory as the snapshot.
Each is limited to 2 MiB and must match its declared byte count and SHA-256
digest before it can be downloaded. JSON and text artifacts are decoded and
redaction-checked independently; digest-valid endpoint, address, sensitive-key,
control-character, malformed, or excessively nested content is rejected.

Validate and atomically promote a collector-produced candidate on the host:

```sh
python3 /opt/phoenix/deploy/prelive_dashboard_snapshot.py \
  --input /opt/phoenix/evidence/dashboard/candidate-dashboard.json \
  --output /opt/phoenix/evidence/dashboard/latest-dashboard.json
```

The command emits only a bounded status code, gate state, and alert count. It
does not print snapshot contents. Phase 9 owns live source collection and
continuous snapshot production. Until that control plane supplies valid
evidence, the production Dashboard remains `UNAVAILABLE` and blocked.

## Verification

The deterministic fixture intentionally contains feed incompleteness,
JetStream backlog growth, and RPC disagreement. The Dashboard must render those
conditions as `review_required`.

```sh
python3 -m py_compile dashboard/app.py dashboard/snapshot_model.py
python3 -m unittest discover -s dashboard/tests -p 'test_*.py' -v
python3 scripts/prelive_dashboard_snapshot.py \
  --input fixtures/dashboard/latest-dashboard.json \
  --output fixtures/dashboard/checked-dashboard.json \
  --check
python3 dashboard/smoke_import.py
```

The Streamlit smoke requires the dependencies in `dashboard/requirements.txt`.
Rendered production Compose isolation is checked by
`scripts/verify_dashboard_compose.py` in Linux CI.
