# Phoenix PRE-LIVE SHADOW v5 Release

## Scope

This repository change prepares, but does not publish or deploy, the first
`money_path_v1` release. The candidate identity is
`phoenix-prelive-shadow-v5`. Its source SHA, build run, run-evidence hash,
manifest hash, and provenance hash remain `UNSET` until a later exact main SHA
completes one explicitly authorized image-publication run.

There is no migration bridge. The v5 candidate does not preserve, import,
backfill, or share the existing raw database.

## Release Environments

Environment A is the untouched fallback:

- tag `phoenix-prelive-shadow-v4`;
- source `a7f19ab165d93dafb4bcc20463f9d010f587281a`;
- build run `29638026962`;
- migrations 001-010;
- the existing PostgreSQL state;
- no mutation while Environment B is validated.

Environment B is the candidate:

- tag `phoenix-prelive-shadow-v5`;
- one future canonical source SHA and build run;
- a distinct, empty PostgreSQL database;
- migrations 001-011 applied from zero;
- `money_path_v1` selective persistence;
- no historical rows or pending outbox import;
- SHADOW-only runtime and zero execution capability.

Rollback during candidate validation is an environment-level return to
Environment A. It does not downgrade Environment B, remove migration 011, or
run v4 against the v5 database. No claim is made that v4 is compatible with
the candidate database.

## Image Publication

`Build Phoenix Images` has only a `workflow_dispatch` trigger. A dispatch must
provide:

- a lowercase 40-character `release_sha` reachable from `origin/main`;
- `release_intent=PHOENIX_PRELIVE_SHADOW_V5`;
- `confirm_publish=PUBLISH_IMMUTABLE_PHOENIX_IMAGES`.

The workflow checks out that exact SHA. Only matrix image jobs receive
`packages: write`. Each image fragment binds the image digest, exact SHA,
release intent, and GitHub run ID. The final manifest job can run only after
all six image jobs and release-assets succeed.

Run `29683234024` is permanently classified
`NON_CANONICAL_INCOMPLETE_BUILD`. Its partial Feed and Dashboard images,
fragments, and release assets must not be referenced by any release. They are
retained as incident evidence.

`release-provenance.json` records one build run, one SHA, six fragment hashes,
release-assets hashes, and the release-manifest hash. The artifact is not
canonical until `scripts/release_provenance.py validate-canonical` verifies a
completed successful `workflow_dispatch` run with every required job and
release artifact present. Cancelled, skipped, failed, incomplete, duplicate,
mixed-run, mixed-SHA, placeholder, and quarantined evidence fails closed.

## Fresh Database Gate

The candidate database gate uses only
`PHOENIX_V5_CANDIDATE_POSTGRES_DSN`; it has no fallback database DSN and no
copy path. It requires:

```text
PHOENIX_V5_DATABASE_ROLE=v5_candidate
PHOENIX_V5_DATABASE_GENERATION=fresh-001-011
PHOENIX_V5_CANDIDATE_DATABASE_NAME=<distinct candidate name>
PHOENIX_V4_FALLBACK_DATABASE_NAME=<fallback name>
PHOENIX_V5_FRESH_DATABASE_CONFIRM=INITIALIZE_EMPTY_PHOENIX_V5_DATABASE
```

The DSN remains secret-bearing operator input and is never printed.

The future authorized initialization sequence is:

1. Run `prelive-v5-fresh-database-gate.sh preflight`.
2. Apply migrations 001-011 with the migration runner.
3. Apply the same migration set again to prove idempotency.
4. Run `prelive-v5-fresh-database-gate.sh post-migration`.
5. Start only the v5 SHADOW services against Environment B.

Preflight accepts no public application tables, or only an empty
`schema_migrations` table. It verifies the connected database name is the
explicit candidate and differs from the fallback name. Post-migration
requires exactly migrations 001-011, the Recorder/Dispatcher/Engine and
money-path schema columns, and zero application, execution, and financial
rows. The gate is read-only: it contains no import, backfill, delete,
truncate, database drop, or cleanup operation.

The isolated CI integration test creates a uniquely named loopback-only test
database, applies migrations 001-011 twice, verifies checksums/schema/empty
tables, and removes only that generated fixture database. It cannot accept a
remote DSN and requires an explicit test-only confirmation.

## Runtime Safety

The v5 contract requires:

```text
PHOENIX_MODE=SHADOW
LIVE_EXECUTION=false
CHAIN_ID=42161
RECORDER_PERSISTENCE_POLICY=money_path_v1
SIGNER_PRIVATE_KEY=
WALLET_ADDRESS=
EXECUTOR_ADDRESS=
```

Recorder aggregation, sample, and Dispatcher refresh settings must remain
within the reviewed bounds encoded in
`deploy/prelive-v5-release.example.json`. Transaction submission, private
relay submission, and broadcast capabilities remain disabled. The candidate
acceptance contract fixes `execution_attempts`, `executions`, and
`realized_pnl` at zero.

Existing PostgreSQL and JetStream integration tests prove that a relevant
message commits its canonical origin row, Feed row, and exactly one outbox row
atomically, and that the source ACK follows the visible commit. Irrelevant and
unsupported-interest traffic creates no raw Firehose rows.

## Post-Deploy Gates

These gates are design only and are not authorized by this change:

1. Initialize the fresh candidate database.
2. Start v5 SHADOW services against Environment B.
3. Complete and pass the formal 15-minute gate.
4. Only then begin the one-hour gate.

Evidence must include Feed input, irrelevant, unsupported, relevant,
persistence ratio, PostgreSQL growth, bytes per input, projected MB/day,
outbox input/output rate, oldest claimable Dispatcher age, classifications,
candidates, decisions, execution counts, and realized PnL.

Acceptance requires no safety regression or relevant fixture loss, zero raw
persistence for irrelevant and unsupported traffic, materially lower storage
growth than the prior approximately 7 GiB/day, execution counts `0:0:0`, and
realized PnL zero. No LIVE gate is authorized.

## Materialization

`deploy/prelive-v5-release.example.json` is intentionally not deployable.
After a later complete canonical build, an operator can bind it to the
same-run manifest and provenance:

```sh
python3 scripts/prelive_v5_release.py materialize \
  --template deploy/prelive-v5-release.example.json \
  --release-manifest release-manifest.json \
  --release-provenance release-provenance.json \
  --run-evidence build-run-evidence.json \
  --output prelive-v5-release.json
```

Materialization itself validates and hashes the completed run evidence. It
rejects placeholders, run `29683234024`, cancelled or incomplete runs, mixed
identity, and invalid provenance. A separate reviewed deployment authorization
and workflow are still required before any database or VPS action.
