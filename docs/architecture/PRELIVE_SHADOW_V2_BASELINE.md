# Phoenix Pre-Live SHADOW v2 Baseline

Status: Phase 0 repository, release, and architecture audit

Audit date: 2026-07-15

This document records the repository contracts that existed before Pre-Live
SHADOW v2 implementation work. It is descriptive, not a production-readiness
claim. No VPS command was run during this audit and no prior VPS observation is
promoted to fresh evidence.

## Safety Boundary

The entire milestone remains SHADOW-only:

- `PHOENIX_MODE=SHADOW`
- `LIVE_EXECUTION=false`
- `SIGNER_PRIVATE_KEY`, `EXECUTOR_ADDRESS`, and `WALLET_ADDRESS` blank
- every persisted SHADOW decision has `execution_eligible=false`
- no execution request, signature, transaction submission, public relay, or
  private relay is permitted
- fork work, when added, must produce unsigned plans in an isolated local
  process and must not be reachable from production services

The Phase 0 change is documentation-only and does not alter these invariants.

## Repository Source Of Truth

| Item | Audited truth |
| --- | --- |
| Remote | `origin` (`MajidAsghariTabrizi/anti-gravity-phoenix-v4`) |
| Remote default branch | `main` |
| Immutable default-branch SHA | `4cf4375452bffd9b3e10b635ab687353dec6cab8` |
| Integration branch | `release/phoenix-prelive-shadow-v2` |
| Imported cumulative SHADOW tip | `33782a94757ee8fb836dce067e87711c974d4464` |
| Commits from immutable base to cumulative tip | 45 |
| Phase 0 branch | `chore/prelive-baseline-audit` |
| Starting worktree | Clean |
| Existing release tags | None |

The integration branch was created at the immutable `main` SHA and then
fast-forwarded through the coherent 45-commit SHADOW lineage already present on
remote branches. Historical SHAs were not treated as the default branch.

The open Phoenix pull-request set at audit time consisted only of Dependabot
updates. None supplied this milestone's production contract.

### Positive-route reporting

The required positive-route reporting behavior is **not** present at the
audited `main` SHA. It is present at the imported cumulative tip and is covered
by `scripts/shadow-positive-route-evidence-tests.sh`,
`phoenix-engine/tests/positive_route_evidence.rs`, and exact-SHA CI run
`29343349663`.

The verified behavior includes:

- a PostgreSQL UTC run baseline
- current-run processing-attempt selection
- newest completed-attempt selection
- exact `(source_event_identity, transaction_hash)` correlation
- no historical fallback
- explicit `not_requested` secondary verification after primary no-profit
- a rendered RPC state budget of at least 12
- no environment rewriting and no LIVE behavior

This behavior is therefore an integration-branch input, not default-branch
evidence.

## Current Production Rendering Contract

Repository intent is deterministic enough to identify a canonical contract,
but the supplied VPS observation does not match it. Resolving that mismatch is
a hard Phase 1 gate.

### Canonical repository paths

| Input or artifact | Path |
| --- | --- |
| Compose file | `/opt/phoenix/deploy/compose.prod.yml` |
| Operator environment | `/etc/phoenix/phoenix.env` |
| Release manifest | `/opt/phoenix/deploy/manifests/<release-sha>.json` |
| Per-release digest environment | `/opt/phoenix/deploy/manifests/<release-sha>.env` |
| Active digest environment | `/opt/phoenix/deploy/current-release.env` |
| Current release pointer | `/opt/phoenix/deploy/current-release` |
| Previous release pointer | `/opt/phoenix/deploy/previous-release` |
| NATS configuration | `/opt/phoenix/deploy/nats-server.conf` |
| Prometheus configuration | `/opt/phoenix/deploy/prometheus/prometheus.yml` |

`scripts/bootstrap-production.sh` installs these files and the deploy,
rollback, validation, and health-check scripts under `/opt/phoenix/deploy`.
No Compose override file is required by the repository contract.

### Exact current invocation

The deploy and rollback scripts currently construct this invocation:

```sh
PHOENIX_ENV_FILE=/etc/phoenix/phoenix.env \
PHOENIX_RELEASE_ENV=/opt/phoenix/deploy/current-release.env \
docker compose \
  --env-file /etc/phoenix/phoenix.env \
  --env-file /opt/phoenix/deploy/current-release.env \
  -f /opt/phoenix/deploy/compose.prod.yml <command>
```

The `PHOENIX_RELEASE_ENV` process variable is passed for script consistency;
Compose interpolation is performed by the two explicit `--env-file` flags.

The supplied runtime observation instead reports the project config file as
`/opt/phoenix/app/compose.prod.yml`. A plain render there produced local-style
images and no visible route registry while running containers used GHCR
digests. That is evidence of context divergence, not evidence that the
repository contract is ambiguous.

### Release inputs and immutable images

`Build Phoenix Images` builds five Phoenix-owned images from one `main` SHA:

- `feed-ingestor`
- `phoenix-engine`
- `rpc-gateway`
- `recorder`
- `dashboard`

The workflow publishes `sha-<40-hex-sha>` tags, captures returned digests, and
assembles a `phoenix.release.v1` manifest. `deploy-release.sh` verifies the
requested SHA, exact tag, required image set, and `sha256:` digest, then writes
the five image variables as `repository@sha256:digest` values.

This immutability applies only to Phoenix-owned images. The current Compose
file references Nitro, NATS, PostgreSQL, and Prometheus by version tag rather
than digest. Phase 1 must decide and enforce the milestone's required immutable
policy for those external images.

The current deploy and rollback scripts call broad
`compose up -d --remove-orphans`. That can recreate protected services and is
incompatible with the new evidence-run and protected-identity contract.

### Generated release-file classification

Production release state under `/opt/phoenix/deploy` is host runtime state and
is outside the repository. CI's `release-manifest.json` and `.ci-prod.env` are
ephemeral runner artifacts.

Repository-local fallbacks also exist for `deploy/current-release.env` and
`current-release.env`. The current `.gitignore` does not explicitly ignore
those files, `deploy/manifests/`, `deploy/current-release`,
`deploy/previous-release`, or root `FETCH_HEAD`. The secret and forbidden-file
scripts do not comprehensively classify them either. No such generated file
was present in the clean Phase 0 worktree. Phase 1 owns closing this hygiene
gap.

## Route Registry Contract

`ENGINE_ROUTE_REGISTRY_JSON` is read from the operator environment and injected
into `phoenix-engine.environment` through a Compose block scalar. Engine startup
parses it as a JSON array with `serde(deny_unknown_fields)` route, leg, and
strategy schemas. The current registry is bounded by bytes, route count, two
legs per route, unique route IDs and fingerprints, canonical addresses,
direction, fee, cycle continuity, state targets, and checked integer strategy
fields.

`scripts/verify-compose-route-registry.py` currently proves two things:

1. the rendered string is byte-for-byte equal to the last operator value found
   across the supplied env files; and
2. both strings decode to equal JSON arrays.

**No route-registry hash is calculated anywhere in the current repository.**
There is no canonical JSON serialization plus digest contract and no release
metadata field for a route hash. Phase 1 must add one deterministic algorithm
and make deployment, evidence, monitoring, and Dashboard metadata consume the
same value.

The evidence script does not yet use the canonical host paths by default. It
defaults to the checked-out repository's `compose.prod.yml` and
`deploy/current-release.env`, with optional `PHOENIX_COMPOSE_FILE`,
`PHOENIX_ENV_FILE`, and `PHOENIX_RELEASE_ENV` overrides. This is the concrete
rendering mismatch Phase 1 must remove.

## Service Modes And Protection

The repository has no Compose profiles for these modes today. The mode sets are
encoded by operational scripts.

### Data-capture-only

The isolated canary's minimum healthy dependency set is:

- `nitro-feed-relay`
- `nats`
- `postgres`
- `feed-ingestor`
- `recorder`

`migration-runner` must have completed before persistent services rely on the
schema. `shadow-dispatcher`, Prometheus, and Dashboard may remain running, but
the current minimum dependency preflight does not require them. Engine and RPC
Gateway are the optional stopped pair.

### Full SHADOW

All long-lived Compose services are expected:

- `nitro-feed-relay`, `nats`, and `postgres`
- `feed-ingestor`, `recorder`, and `shadow-dispatcher`
- `rpc-gateway` and `phoenix-engine`
- `prometheus` and `dashboard`

`migration-runner` is a successful one-shot prerequisite, not a long-lived
service.

### Evidence run

The positive-route workflow requires the five data-capture dependencies,
healthy Feed/Recorder/PostgreSQL, the Engine stream and durable consumer, and
locally available digest-pinned Engine/RPC images. It snapshots protected
services, stops only `phoenix-engine` and `rpc-gateway`, captures a database
clock baseline, starts only those two with `--no-deps`, performs the bounded
search, then stops only those two and verifies the protected snapshot.

The current protected set is:

- `nitro-feed-relay`
- `nats`
- `postgres`
- `feed-ingestor`
- `recorder`
- `shadow-dispatcher`
- `prometheus`
- `dashboard`

Current deploy and rollback behavior does not honor this protection and must be
fixed before a production evidence run.

### Private monitoring access

NATS has no host port. Operational scripts query its monitoring endpoints from
inside the container with `compose exec -T nats wget ... 127.0.0.1:8222`.
Prometheus binds host port `9090` only to `127.0.0.1`; Dashboard queries it over
the internal Compose network at `http://prometheus:9090`. Dashboard itself binds
port `8501` only to host loopback. No Docker socket is mounted.

## Engine Exit And Retry Truth

Engine configuration rejects every non-SHADOW mode before the durable runtime
starts. In the consumer loop:

- fetch/state, storage, and acknowledgement failures leave the inner consumer;
  the daemon retries those dependencies after bounded recovery
- unsupported schema and evaluator terminal errors use `Terminate`
- malformed input uses `Retry`
- any retry at delivery count `ENGINE_MAX_DELIVERIES` (currently 20) is rewritten
  as `terminal_integrity_failure` with detail `engine_retries_exhausted`
- a successful terminal acknowledgement returns `RuntimeExit::IntegrityFailure`
- the daemon then cancels its monitors and exits with
  `Engine stopped on a terminal integrity condition`

The last behavior incorrectly treats exhausted transient dependency work as
global integrity loss. Docker's restart policy can then restart the entire
Engine. Phase 2 owns persisting a bounded per-message dependency-exhausted
classification, acknowledging only that message, and continuing later work.
True schema/event integrity failures must remain terminal.

## Production Execution Surface

Execution-related code exists, but it is not a reachable production submission
path:

- `contracts/src/PhoenixExecutor.sol` is a real allowlisted flash-loan and swap
  executor contract, with a Foundry deploy script and unit tests
- `phoenix-engine/src/execution/mod.rs` contains a mode/coordinator model whose
  LIVE result is only `RequiresSignerAndSequencerSubmit`; it has no signer or
  submit implementation
- the durable Engine binary hard-rejects non-SHADOW mode
- decision construction hardcodes `execution_eligible=false`
- PostgreSQL migration `003_shadow_profitability_evidence.sql` enforces
  `CHECK (execution_eligible = false)`
- production Compose blanks signer, executor, and wallet values for Engine and
  Dispatcher
- no production service implements `eth_sendRawTransaction`, transaction
  signing, nonce management, public relay submission, or private relay
  submission
- the executor contract and deploy script are not Compose services and no
  production executor address is configured

Therefore no production signing or broadcast path is reachable at this
baseline. The contract remains security-sensitive dormant code and must not be
deployed or wired during this milestone.

## CI Contract Map

| Contract | Existing CI job or step |
| --- | --- |
| Secrets, forbidden files, shell syntax, tracked hygiene | `hygiene` |
| Canary isolation and positive-route control behavior | `hygiene` bounded Engine canary step |
| Go formatting, vet, unit tests | `go` |
| Migration runner formatting, vet, tests | `go` |
| Engine fmt, clippy `-D warnings`, unit and PostgreSQL integration | `rust-phoenix` |
| RPC Gateway fmt, clippy `-D warnings`, tests | `rust-rpc-gateway` |
| Recorder fmt, clippy `-D warnings`, PostgreSQL integration | `rust-recorder` |
| Replay fmt, clippy `-D warnings`, tests | `rust-replay` |
| Solidity formatting and tests | `solidity` |
| Dashboard compile and import smoke | `python-dashboard` |
| Compose rendering and route JSON preservation | `docker-validation` |
| NATS config and Phoenix-owned image builds | `docker-validation` |
| Deterministic decoder and Engine evidence fixtures | `integration-fixtures` |
| Real local JetStream publication/consumption contracts | `jetstream-integration` |
| SHA image publication and manifest assembly | `Build Phoenix Images` on `main` |
| Manifest-bound host deployment | `Deploy Shadow Production` after image build |

CI currently has no dedicated contract for a route hash, protected deploy
service selection, database growth reporting, fork sandbox isolation, or the
complete pre-live control-plane evidence schema.

## Dashboard And Observability Baseline

The Dashboard is a single read-only Streamlit process in `dashboard/app.py`.
It reads PostgreSQL with psycopg/pandas and Prometheus through the HTTP query
API. It has no Docker socket, mutation endpoint, execution control, signer, or
wallet dependency. It already shows basic command-center, origin, decision,
execution, realized-PnL, miss, pool, RPC, health, economics, and risk tabs.

Important limitations:

- database and Prometheus failures are swallowed and rendered as empty data or
  zero, which can be mistaken for healthy zero activity
- the DSN has a development credential fallback instead of requiring an
  explicit production value
- the SHADOW financial banner contains a text-encoding defect
- release SHA, image digests, route hash, deployment context, protected service
  identity, JetStream detail, PostgreSQL growth, and control-plane runs are not
  represented
- no status collector normalizes Docker/host state for read-only consumption

The Dashboard can be extended safely through PostgreSQL views/tables,
Prometheus metrics, and a bounded status collector that writes sanitized state
to one of those read-only data planes. Mounting the Docker socket into the
Dashboard is unnecessary and prohibited.

## Growth And Retention Baseline

- Prometheus has explicit `30d` TSDB retention and persistent host storage.
- `PHOENIX_FEED_TX` uses JetStream WorkQueue retention with limits of five
  million messages, 2 GiB, 24 hours, and 1 MiB per message.
- `PHOENIX_ENGINE_INPUT` uses WorkQueue retention with limits of two million
  messages, 1 GiB, seven days, and 1 MiB per message.
- PostgreSQL uses persistent host storage, but there is no production data
  retention, partition-pruning, archive, or bounded cleanup mechanism.
- No current collector reports per-table/index growth, database size trend,
  volume free space, or retention pressure.

Phase 7 must add bounded growth visibility. Any later retention policy must be
explicit, reviewed, and additive; this milestone must not silently delete
evidence.

## Existing Domain Components To Extend

The repository already contains reusable foundations and should not gain
parallel abstractions without evidence:

- checked-integer SHADOW economics and decision models
- route registry, pool graph, official Uniswap entrypoint classification, and
  deterministic positive-route replay
- block-pinned RPC state requests, provider priority, budgets, cache,
  disagreement status, and verification evidence
- simulation evidence types, but no dedicated fork runner
- transactional Recorder outbox and durable Engine JetStream stream
- additive checksum-verified migration runner with advisory locking
- low-cardinality Feed, Recorder, Dispatcher, RPC, and Engine metrics

## Planned Workstream File Ownership

The ownership below is the expected primary edit surface. Shared-file changes
must be rebased from the integration branch and kept to the owning phase's
contract.

| Branch | Primary ownership |
| --- | --- |
| `chore/prelive-baseline-audit` | `docs/architecture/PRELIVE_SHADOW_V2_BASELINE.md`, initial worklog |
| `fix/production-compose-route-registry-truth` | `compose.prod.yml`, `.gitignore`, production render/deploy/rollback/env scripts and tests, release workflows and release metadata docs |
| `fix/engine-transient-exhaustion-quarantine` | Engine runtime, classifications, persistence/migration, retry metrics, focused Engine tests |
| `feat/shadow-profitability-truth` | Engine economics/decision/opportunity persistence, additive migrations, economics fixtures/docs |
| `feat/arbitrum-route-discovery` | route discovery/ranking tool, reviewed route artifacts, route provenance tests/docs |
| `feat/shadow-secondary-verification` | RPC/Engine secondary-verification contracts, statuses, persistence, metrics, fixtures/docs |
| `feat/fork-execution-sandbox` | isolated fork runner/service, unsigned plan schema, fork-only Compose/config/scripts/tests/docs |
| `feat/prelive-money-path-observability` | service metrics, Prometheus config, bounded reports, growth views/queries, observability tests/docs |
| `feat/prelive-technical-business-dashboard` | `dashboard/`, dashboard image/runtime wiring, read-only status ingestion and UI tests |
| `feat/continuous-shadow-control-plane` | bounded run controller, evidence schema, protected-service orchestration tests and runbook |
| `release/phoenix-prelive-shadow-v2-docs` | release/readiness/rollback/operations docs, final worklog entries, release workflow metadata only |

The integration branch owns conflict resolution for shared Compose, migration,
workflow, Prometheus, and worklog files. No child branch owns unrelated service
refactors.

## Phase 0 Gate Decision

The exact base SHA, current positive-route behavior, intended production
context, generated-file gaps, execution surface, Dashboard architecture,
retention baseline, CI contracts, and workstream ownership are now documented.

Phase 1 may begin only after Phase 0 checks pass. Phase 1 remains blocked from
production use until it proves one canonical renderer, one route hash, exact
release context, digest truth, fail-closed blank LIVE-only settings, and
protected-service-safe deploy/evidence commands.
