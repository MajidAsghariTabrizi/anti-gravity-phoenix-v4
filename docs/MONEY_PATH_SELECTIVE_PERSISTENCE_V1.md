# Money-Path Selective Persistence v1

## Scope

`ADMISSION_POLICY_VERSION=money_path_v1` changes only structural ingress
admission and Dispatcher backlog telemetry. PostgreSQL becomes a money-evidence
store for new Feed inputs: deterministically filtered traffic contributes only
bounded aggregate evidence, while structurally admitted traffic retains the
existing canonical Recorder and Engine contracts.

The admission decision is a conservative structural superset. It is independent
of mode, profitability, RPC state, simulation, gas, wallet state, signing, and
execution eligibility. `relevant_route_input` means that at least one reviewed
release capability can structurally consume the input; it does not mean that the
input is profitable or executable.

This phase does not delete, rewrite, or compact historical rows. Migration 011 is
additive. Shipping it requires a new reviewed immutable release; existing
immutable v4/v3 release assets must not be silently reused.

## Durability Boundaries

For a `relevant_route_input`, Recorder uses one PostgreSQL transaction to insert
or confirm the canonical `origin_transactions` row, the canonical `feed_events`
row, and exactly one `engine_outbox` row. The `PHOENIX_FEED_TX` delivery is ACKed
only after that transaction commits. A rollback, transient database failure, or
integrity failure leaves the source delivery unacknowledged for redelivery.

For `irrelevant` and `unsupported_interesting`, Recorder creates none of those
three raw rows. It updates bounded in-memory evidence and ACKs only after that
required bookkeeping succeeds. Periodic aggregate/sample persistence is
non-financial telemetry; a later flush failure is observable and never triggers
fallback raw persistence.

Dispatcher claims a bounded `engine_outbox` batch with `FOR UPDATE SKIP LOCKED`,
publishes the unchanged `phoenix.engine.input.v1` envelope to
`PHOENIX_ENGINE_INPUT` on `phoenix.engine.input`, persists the JetStream ACK
sequence, and then claims the next batch. Engine ACKs that delivery only after
its classification/evidence transaction commits or confirms an already-final
identity.

## Table Lineage

### `origin_transactions`

- **Writer and file:** Recorder, `recorder/src/persistence.rs`.
- **Readers:** foreign-key consumers such as `opportunities` and
  `miss_reasons`; production safety, maintenance, and Recorder smoke scripts.
- **Transaction boundary:** same Recorder transaction as the corresponding
  `feed_events` and `engine_outbox` rows.
- **ACK timing:** `PHOENIX_FEED_TX` ACK occurs after commit.
- **Unique/idempotency:** unique `tx_hash`; `ON CONFLICT (tx_hash) DO NOTHING`
  and post-insert outcome checks make redelivery idempotent.
- **Replay dependency:** not a direct input to the current replay binary; kept
  as canonical admitted-origin provenance for deterministic reconstruction.
- **Audit dependency:** proves which admitted Arbitrum transaction produced the
  downstream event.
- **Raw duplication:** contains admitted transaction calldata and metadata also
  represented in the canonical Feed payload.
- **Money-path necessity:** retained for admitted events because current schema
  references and audit contracts depend on it.
- **Phase-1 policy:** write only for `relevant_route_input`.
- **Duplicate delivery:** confirms the existing canonical row and proceeds
  atomically without creating another origin.

### `feed_events`

- **Writer and file:** Recorder, `recorder/src/persistence.rs`.
- **Readers:** `engine_outbox` through its composite foreign key; Engine and
  Recorder smoke, positive-route, maintenance, and continuity evidence scripts.
- **Transaction boundary:** same Recorder transaction as
  `origin_transactions` and `engine_outbox`.
- **ACK timing:** `PHOENIX_FEED_TX` ACK occurs after commit.
- **Unique/idempotency:** unique `(sequence_number, tx_hash)`; conflict-safe
  insertion preserves one canonical event.
- **Replay dependency:** not directly consumed by the current replay binary;
  retained as canonical admitted Feed evidence and outbox parent.
- **Audit dependency:** preserves source sequence, transaction identity, and the
  exact normalized Feed envelope used to build Engine input.
- **Raw duplication:** payload overlaps the admitted transaction and outbox
  envelope.
- **Money-path necessity:** required by the current `engine_outbox` foreign key
  and existing evidence consumers.
- **Phase-1 policy:** write only for `relevant_route_input`.
- **Duplicate delivery:** reuses the existing composite identity and creates no
  duplicate Feed row.

### `engine_outbox`

- **Writer and file:** Recorder, `recorder/src/persistence.rs`.
- **Readers:** Shadow Dispatcher, `recorder/src/engine_outbox.rs` and
  `recorder/src/dispatcher.rs`; backlog reports and safety harnesses.
- **Transaction boundary:** exactly one row in the same Recorder transaction as
  the admitted origin and Feed rows.
- **ACK timing:** source Feed ACK follows transaction commit. Dispatcher marks
  the row published only after JetStream confirms publication.
- **Unique/idempotency:** primary `outbox_id`, unique
  `source_event_identity`, and `outbox_id = source_event_identity`; publish ACK
  sequence is persisted before the next claim.
- **Replay dependency:** durable bridge for redelivery to the existing Engine
  stream; an unmarked claim is leased and retryable.
- **Audit dependency:** records publish attempts, claim ownership, final publish
  time, and JetStream ACK sequence.
- **Raw duplication:** stores the unchanged bounded Engine input envelope,
  overlapping canonical Feed identity and payload.
- **Money-path necessity:** required only for inputs that may reach strategy
  evaluation.
- **Phase-1 policy:** create exactly one row for `relevant_route_input`; create
  none for filtered categories.
- **Duplicate delivery:** the stable source identity returns the existing row;
  no duplicate Engine message identity is created.

### `shadow_engine_processing_attempts`

- **Writer and file:** Phoenix Engine, `phoenix-engine/src/persistence.rs`.
- **Readers:** Engine retry/recovery logic, money-path and control reports,
  positive-route evidence, Dashboard, and smoke tests.
- **Transaction boundary:** same Engine transaction as final classification,
  decisions, profitability facts, and RPC quality evidence for the delivery.
- **ACK timing:** Engine input ACK occurs after commit; transient failure before
  commit preserves redelivery.
- **Unique/idempotency:** unique `(source_event_identity, delivery_attempt)`;
  duplicate attempt insertion is ignored.
- **Replay dependency:** provides bounded delivery-attempt history for retry and
  dependency-exhaustion analysis.
- **Audit dependency:** proves each processing attempt, result class, latency,
  and bounded error class.
- **Raw duplication:** no raw Feed payload; bounded processing evidence only.
- **Money-path necessity:** operational evidence for admitted inputs.
- **Phase-1 policy:** unchanged; populated only after an outbox event reaches
  Engine.
- **Duplicate delivery:** a new delivery attempt may be recorded, while an
  identical attempt number remains idempotent.

### `shadow_engine_classifications`

- **Writer and file:** Phoenix Engine, `phoenix-engine/src/persistence.rs`.
- **Readers:** Engine final-state lookup, reports, Dashboard, control-plane and
  positive-route evidence, route discovery, and smoke tests.
- **Transaction boundary:** same Engine transaction as processing attempts and
  all decision evidence generated for the event.
- **ACK timing:** Engine input ACK occurs after commit or after the transaction
  confirms an already-final classification.
- **Unique/idempotency:** primary `source_event_identity` plus unique
  `(source_sequence, tx_hash)`; conflict updates preserve the stable identity
  and greatest delivery-attempt count.
- **Replay dependency:** canonical final Engine classification for an admitted
  source identity.
- **Audit dependency:** records candidate/decision counts, classification,
  latency, and bounded evidence.
- **Raw duplication:** no raw calldata; identity and bounded classification
  evidence overlap the Engine envelope.
- **Money-path necessity:** proves what strategy evaluation did with an admitted
  input.
- **Phase-1 policy:** unchanged.
- **Duplicate delivery:** final identities short-circuit as already final;
  non-final retry state updates the same row.

### `shadow_decisions`

- **Writer and file:** Phoenix Engine, `phoenix-engine/src/persistence.rs`.
- **Readers:** profitability, fork, route-discovery, Dashboard, reporting,
  control, positive-route, and safety scripts.
- **Transaction boundary:** same Engine transaction as classification,
  processing attempt, profitability fact, and RPC quality rows.
- **ACK timing:** Engine input ACK occurs only after commit.
- **Unique/idempotency:** deterministic decision UUID plus unique
  `(source_event_identity, strategy_version, route_fingerprint)` for non-null
  source identities.
- **Replay dependency:** decision evidence is a canonical output used for
  deterministic comparison, not an ingress source.
- **Audit dependency:** preserves route, market, economics, simulation, policy,
  disposition, and rejection evidence with `execution_eligible=false`.
- **Raw duplication:** no raw Feed payload; normalized evidence may repeat route
  and identity facts needed for standalone audit.
- **Money-path necessity:** canonical strategy-decision evidence.
- **Phase-1 policy:** unchanged.
- **Duplicate delivery:** a final source classification prevents reinsertion;
  any conflicting decision identity is an integrity failure rather than a
  second decision.

### `shadow_profitability_facts`

- **Writer and file:** Phoenix Engine, `phoenix-engine/src/persistence.rs`.
- **Readers:** fork sandbox, technical/business reports, Dashboard, route
  discovery, control-plane, and safety scripts.
- **Transaction boundary:** inserted in the same Engine transaction as its
  `shadow_decisions` parent and classification.
- **ACK timing:** Engine input ACK occurs only after commit.
- **Unique/idempotency:** primary and foreign key `shadow_decision_id`; one
  canonical profitability fact per decision.
- **Replay dependency:** supplies normalized financial truth for replay
  comparison and fork-only planning.
- **Audit dependency:** preserves complete cost decomposition, verification
  lifecycle, source identity, route configuration, and SHADOW execution-zero
  assertions.
- **Raw duplication:** no raw Feed payload; intentionally denormalizes bounded
  route and financial facts for auditability.
- **Money-path necessity:** canonical profitability evidence after strategy
  evaluation.
- **Phase-1 policy:** unchanged.
- **Duplicate delivery:** existing final classification prevents a second fact;
  duplicate decision identity is fail-closed.

### `rpc_quality_records`

- **Writer and file:** Phoenix Engine, `phoenix-engine/src/persistence.rs`.
- **Readers:** Dashboard, money-path report, route discovery, and verification
  evidence queries.
- **Transaction boundary:** inserted with the owning decision and
  profitability fact in the Engine transaction.
- **ACK timing:** Engine input ACK occurs only after commit.
- **Unique/idempotency:** rows belong to one decision by foreign key; the final
  classification guard prevents duplicate decision evaluation from appending a
  second set.
- **Replay dependency:** not an ingress source; records provider evidence used
  to explain a deterministic decision.
- **Audit dependency:** proves pinned block, response hash, latency, timeout,
  retry, staleness, and disagreement state.
- **Raw duplication:** no raw Feed payload or RPC response body.
- **Money-path necessity:** bounded evidence for state quality behind financial
  decisions.
- **Phase-1 policy:** unchanged.
- **Duplicate delivery:** already-final source identities do not write another
  provider set.

### `fork_simulation_results`

- **Writer and file:** fork-only sandbox, `fork-sandbox/src/store.rs`.
- **Readers:** fork reports, Dashboard, control-plane evidence, and production
  safety checks.
- **Transaction boundary:** one validated result insert; separate from Recorder
  and Engine input transactions.
- **ACK timing:** no Feed or Engine NATS ACK is controlled by this writer.
- **Unique/idempotency:** primary `result_hash`, unique `plan_hash`, and foreign
  key to one profitability fact.
- **Replay dependency:** immutable fork result for comparison with the unsigned
  plan and predicted economics.
- **Audit dependency:** proves fork identity, pinned block, predicted versus
  simulated economics, and all fork-only/SHADOW execution-zero fields.
- **Raw duplication:** no Feed payload; stores bounded plan and simulation
  evidence.
- **Money-path necessity:** counterfactual evidence only for selected positive
  SHADOW opportunities.
- **Phase-1 policy:** unchanged.
- **Duplicate delivery:** the same plan cannot create another row; conflicting
  persistence fails closed.

### `execution_attempts`

- **Writer and file:** no production writer exists in this PRE-LIVE SHADOW
  release; schema is defined in `migrations/001_init.sql`.
- **Readers:** control, smoke, positive-route, protected-maintenance, and safety
  scripts require the count to remain unchanged/zero.
- **Transaction boundary:** reserved for a separately reviewed future execution
  lifecycle; excluded from Recorder and Engine admission transactions.
- **ACK timing:** no current NATS ACK depends on this table.
- **Unique/idempotency:** primary key only in the current schema; future writer
  must define reviewed attempt idempotency before LIVE use.
- **Replay dependency:** none in this phase.
- **Audit dependency:** reserved submission-attempt audit record.
- **Raw duplication:** none created by this release.
- **Money-path necessity:** future execution accounting, not structural
  admission.
- **Phase-1 policy:** must remain unchanged and empty.
- **Duplicate delivery:** Recorder/Engine redelivery never creates an execution
  attempt.

### `executions`

- **Writer and file:** no production writer exists in this PRE-LIVE SHADOW
  release; schema is defined in `migrations/001_init.sql`.
- **Readers:** control, smoke, positive-route, protected-maintenance, and safety
  scripts require the count to remain unchanged/zero.
- **Transaction boundary:** reserved for future receipt reconciliation and
  excluded from all Phase-1 ingress/evidence transactions.
- **ACK timing:** no current NATS ACK depends on this table.
- **Unique/idempotency:** unique transaction hash plus primary key; no active
  writer.
- **Replay dependency:** none in this phase.
- **Audit dependency:** reserved immutable execution receipt and fee evidence.
- **Raw duplication:** none created by this release.
- **Money-path necessity:** future settled-execution accounting only.
- **Phase-1 policy:** must remain unchanged and empty.
- **Duplicate delivery:** Recorder/Engine redelivery cannot create a row.

### `realized_pnl`

- **Writer and file:** no production writer exists in this PRE-LIVE SHADOW
  release; schema is defined in `migrations/001_init.sql`.
- **Readers:** control, smoke, positive-route, protected-maintenance, and safety
  scripts require the count to remain unchanged/zero.
- **Transaction boundary:** reserved for future post-receipt reconciliation and
  excluded from Recorder, Engine, and fork transactions.
- **ACK timing:** no current NATS ACK depends on this table.
- **Unique/idempotency:** primary key and foreign key to `executions`; future
  reconciliation requires an independently reviewed idempotency contract.
- **Replay dependency:** none in this phase.
- **Audit dependency:** reserved realized financial truth after a settled
  execution.
- **Raw duplication:** none created by this release.
- **Money-path necessity:** future realized-PnL accounting only.
- **Phase-1 policy:** must remain unchanged and empty.
- **Duplicate delivery:** Recorder/Engine redelivery cannot create a row.

## Compatibility

- Engine input schema changed: no.
- Engine stream or subject changed: no.
- Decision or execution schemas changed: no.
- Future LIVE ingress migration required: no.
- Future LIVE outbox migration required: no.

Future capability additions extend the shared decoder/capability adapter,
reviewed immutable manifest, fixtures, and tests. They do not replace the
Recorder transaction, source ACK ordering, transactional outbox, Dispatcher,
Engine input transport, or downstream financial schemas.

## Loss And Recovery Model

Raw admitted records and outbox delivery are durable and transactional.
Low-cardinality aggregate counters and redacted unsupported samples are buffered
in memory to avoid one PostgreSQL upsert per filtered Feed input. An abrupt
Recorder stop can lose only the not-yet-flushed non-financial telemetry window;
it cannot lose an admitted event after ACK, create raw fallback rows, or mutate
financial/execution tables. Flush failures and sample-cap rejections are
observable through bounded metrics.

Historical backlog deletion, retention, table compaction, and production
cleanup are intentionally deferred until the new relevance ratio and measured
storage rate have been proven under a separately reviewed immutable release.
