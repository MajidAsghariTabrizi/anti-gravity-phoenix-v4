# ADR: Transactional Outbox for SHADOW Engine Input

- Status: Accepted
- Date: 2026-07-13
- Scope: SHADOW evidence delivery only

## Context

The proven production path is Nitro Relay to Feed Ingestor to the
`PHOENIX_FEED_TX` JetStream WorkQueue, then the `PHOENIX_RECORDER` durable
consumer and PostgreSQL. Recorder acknowledges each feed delivery only after
its `feed_events` and `origin_transactions` transaction commits. Phoenix Engine
needs the same recorded traffic without weakening that durability boundary or
competing with Recorder for WorkQueue messages.

The Engine input must be replayable, auditable, bounded, idempotent, and
recoverable across PostgreSQL, NATS, dispatcher, and Engine failures. This ADR
does not authorize signing, submission, or LIVE execution.

## Options Considered

### 1. Change PHOENIX_FEED_TX to LimitsPolicy with two consumers

This would let Recorder and Engine independently consume the original feed.
It also changes retention and deletion behavior on the already proven path,
adds a second acknowledgement lifecycle to the feed stream, and makes the two
database evidence sets independently observable rather than atomically linked.
It increases operational coupling and migration risk for the production
Recorder path.

### 2. Dual-publish from Feed Ingestor

Feed Ingestor could publish each normalized event to both Recorder and Engine
streams. NATS cannot atomically persist those two publishes. A process crash,
timeout, or ambiguous acknowledgement between publishes can leave one stream
without the event. Retrying both publishes reduces but does not eliminate the
dual-write ambiguity, and it adds Engine delivery work to the latency-sensitive
feed path.

### 3. PostgreSQL transactional outbox

Recorder inserts `feed_events`, `origin_transactions`, and `engine_outbox` in
one PostgreSQL transaction. A separate dispatcher publishes committed outbox
rows to a dedicated Engine stream and marks each row published only after the
JetStream persistence acknowledgement. Engine commits its classification and
decision evidence before acknowledging its own durable delivery.

## Decision

Use option 3, a PostgreSQL transactional outbox.

Recorder stores the complete canonical normalized event in the outbox. The
payload is already validated and bounded to 1 MiB at Recorder ingress, and the
database adds an independent encoded JSON size check. Storing the payload avoids
a race-prone join or reread whose referenced rows could be unavailable or later
evolve. The small duplication is preferable to an ambiguous Engine input.

Each outbox row has a deterministic source identity derived from schema version,
source sequence, and transaction hash. The identity is unique in PostgreSQL,
used as the JetStream message ID, and used as the Engine classification key.
Redelivery can repair a historical `feed_events` row that is missing its outbox
row without creating duplicate outbox work.

## Engine Stream Contract

`PHOENIX_ENGINE_INPUT` uses subject `phoenix.engine.input`, file storage, and
WorkQueue retention. There is one durable consumer,
`PHOENIX_ENGINE_SHADOW`. Configuration is explicit and compatibility-checked:

| Setting | Value |
| --- | --- |
| Maximum age | 7 days |
| Maximum bytes | 1 GiB |
| Maximum messages | 2,000,000 |
| Maximum message size | 1 MiB |
| Duplicate window | 24 hours |
| ACK policy | Explicit |
| ACK wait | 120 seconds |
| Maximum deliveries | 20 |
| Maximum ACK pending | 512 |
| Pull batch size | 64 |
| Fetch expiry | 1 second |

The PostgreSQL outbox remains the durable source for rows not yet acknowledged
by JetStream. WorkQueue retention is appropriate while there is one SHADOW
consumer and makes stream storage bounded after Engine acknowledgement. Adding a
second consumer requires a new ADR and a different stream contract.

## Atomicity and Recovery

- Recorder commit succeeds: recorded evidence and Engine work become visible
  together; only then may Recorder ACK the feed delivery.
- Recorder transaction fails: none of the three records commit and the feed
  delivery remains unacknowledged.
- Dispatcher crashes before publish: the lease expires and the row becomes
  claimable again.
- Dispatcher crashes after publish but before `published_at`: the row may be
  republished with the same message ID. JetStream suppresses duplicates within
  its duplicate window; Engine database idempotency remains authoritative after
  that window.
- Dispatcher receives a JetStream ACK: it records `published_at` and the stream
  acknowledgement sequence. A crash after this mark does not republish.
- Engine crashes before its database commit: the message remains unacknowledged
  and is redelivered.
- Engine crashes after commit but before ACK: redelivery finds the deterministic
  classification identity and skips duplicate decision creation before ACK.

Claims use bounded batches and expiring ownership leases selected with
PostgreSQL row locking, so future dispatcher replicas do not concurrently own
the same pending row. Retry attempts and sanitized error classes are persisted;
payloads are never deleted on transient failure.

## Consequences

The proven Recorder stream and consumer are unchanged, and Feed Ingestor does
not gain a second publish. PostgreSQL provides the atomic boundary that removes
dual-write loss. The outbox and deterministic identities make delivery history,
replay, duplicates, retry state, and crash recovery auditable.

The tradeoff is additional commit, polling, publish, and Engine-consumer latency,
plus bounded duplicate storage in PostgreSQL. That latency is acceptable for a
SHADOW evidence system and is exposed through backlog and age metrics.

This architecture is not a future LIVE execution path by itself. LIVE would
need a separately reviewed low-latency state model, freshness guarantees,
pre-trade risk controls, signer isolation, nonce management, submission and
replacement semantics, chain reorganization handling, and execution outcome
reconciliation. None of those capabilities are enabled or implied here.
