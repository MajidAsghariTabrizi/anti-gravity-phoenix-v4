# Recorder Durable Delivery

Phoenix uses JetStream to move normalized transactions from `feed-ingestor` to the Recorder without the Core NATS slow-consumer loss mode. Both applications idempotently apply the same stable stream configuration before reporting dependency readiness.

## Stream

| Setting | Value |
| --- | --- |
| Name | `PHOENIX_FEED_TX` |
| Subject | `phoenix.feed.tx` |
| Retention | work queue |
| Storage | file |
| Discard policy | new |
| Maximum consumers | 1 |
| Maximum messages | 5,000,000 |
| Maximum bytes | 2 GiB |
| Maximum age | 24 hours |
| Maximum message size | 1 MiB |
| Replicas | 1 |
| Duplicate window | 2 minutes |

`DiscardNew` is intentional. When the stream reaches its message or byte bound, a new publish is rejected and feed readiness fails instead of silently evicting an unacknowledged old transaction. The 24-hour age is a final storage bound, not an archival promise. Operations must restore a stalled Recorder before that window and alert on pending-message growth.

## Durable Consumer

| Setting | Value |
| --- | --- |
| Name | `PHOENIX_RECORDER` |
| Type | durable pull |
| Filter | `phoenix.feed.tx` |
| Delivery policy | all |
| Replay policy | instant |
| ACK policy | explicit |
| ACK wait | 60 seconds |
| Maximum deliveries | 5 |
| Maximum ACK pending | 1,024 |
| Maximum request batch | 256 messages |
| Maximum request bytes | 32 MiB |
| Maximum request expiry | 1 second |
| Maximum waiting pulls | 2 |
| Consumer replicas | 1 |
| Consumer state storage | inherited file storage |

The Recorder fetches at most `RECORDER_BATCH_MAX_SIZE=256` messages and waits at most `RECORDER_BATCH_MAX_WAIT_MS=100` for a batch by default. Configuration rejects sizes above the durable consumer limit and waits above one second.

Valid messages are inserted into `origin_transactions` and `feed_events` with two multi-row statements in one PostgreSQL transaction. JetStream ACK confirmation is requested only after that transaction commits, with at most 32 confirmations in flight. PostgreSQL failures keep the batch unacknowledged and send work-in-progress acknowledgements while retrying. A crash or failed ACK therefore causes replay; existing unique constraints make replay idempotent.

Malformed messages are NAKed with a one-second delay for the first four deliveries. On delivery five the Recorder sends `TERM`, increments the poison counter, and permanently fails readiness for operator investigation. Valid siblings in the same fetched batch still persist and ACK.

## Server Sizing

The first SHADOW host is documented as 4 vCPU, 8 GiB RAM, and 80 GB SSD. `deploy/nats-server.conf` therefore sets:

- 64 MiB maximum JetStream memory storage
- 4 GiB maximum JetStream file storage
- 512 MiB Go runtime memory target through `GOMEMLIMIT`
- 10-second file sync interval
- a persistent named volume, `phoenix-nats-jetstream`

The stream itself is limited to 2 GiB, leaving server metadata, consumer state, filesystem overhead, and operational headroom inside the 4 GiB server cap. File storage must remain on local SSD-backed Docker storage, not NFS. PostgreSQL and host disk usage still need independent monitoring.

## Migration And Recovery Limits

This change cannot recover Core NATS messages dropped before JetStream was enabled. For cutover, stop `feed-ingestor`, deploy and health-check JetStream plus the Recorder, verify the stream and durable consumer, then start `feed-ingestor`. The live smoke script performs this ordering.

The deployment is a single JetStream server with one replica. It survives application restarts and NATS container replacement while the Docker volume remains intact, but it does not survive host or volume loss. A rollback to a Core-NATS Recorder does not consume queued JetStream messages; retain the volume and use a reviewed forward recovery plan. Do not delete the stream, durable consumer, or named volume during rollback.

Production durability remains unproven until `scripts/recorder-live-smoke.sh` passes against real Nitro traffic on the VPS for the full observation and restart/replay sequence.
