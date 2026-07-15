# Engine Dependency Exhaustion Quarantine

Phoenix Engine remains SHADOW-only. A supported route whose bounded RPC Gateway retries are
exhausted is isolated as `dependency_exhausted`; it is not promoted to an integrity failure and
is not counted as dependency recovery.

## State transition

```text
transient_dependency_failure
  -> bounded JetStream redelivery
  -> dependency_exhausted / dependency_retries_exhausted
  -> atomic persistence
  -> confirmed ACK for that source event
  -> continue consuming later events
```

The production delivery limit remains the durable consumer limit of 20. Engine startup rejects
limits below 2, negative or unlimited values, and values above 100. Malformed internal events,
contradictory persisted evidence, invalid database schema, and other integrity failures retain
their terminal behavior.

## Evidence contract

The final classification records source identity, sequence, transaction hash, route fingerprints,
original and final failure classes, first and final failure times, delivery and retry counts, the
exhaustion limit, quarantine reason, logical provider identifier, and a SHA-256 error evidence hash.
Provider URLs are forbidden. Every record states `execution_mode=SHADOW`, `shadow_only=true`,
`execution_eligible=false`, and `execution_request_created=false`.

The first transient attempt remains in `shadow_engine_processing_attempts`. Small original evidence
is copied into the quarantine row. If that copy would exceed the 1 MiB evidence limit, the final row
contains a bounded reference to the preserved first-attempt ledger row instead.

## Operations

Use `phoenix_engine_dependency_exhausted_total` for the low-cardinality exhaustion count. Inspect
individual quarantined events in `shadow_engine_classifications` and their complete attempt history
in `shadow_engine_processing_attempts`. The SHADOW live-smoke recovery query explicitly excludes
both `transient_dependency_failure` and `dependency_exhausted`.

No quarantine path signs, submits, broadcasts, or creates an execution request.
