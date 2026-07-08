# Nitro Feed Integration

Status: production relay ingestion is blocked.

Phoenix currently supports deterministic fixture JSON frames and stdin line input in `feed-ingestor`. It does not yet include a compiled websocket adapter using official Offchain Labs Nitro feed parsing utilities.

The production guard is deliberate:

- If `PHOENIX_ENV=production` and `PHOENIX_FEED_FIXTURE` is set, startup fails.
- If `PHOENIX_ENV=production` and `PHOENIX_FEED_SOURCE` is not `relay`, startup fails.
- If `PHOENIX_ENV=production` and `PHOENIX_FEED_RELAY_URL` is missing, startup fails.
- If production relay mode is requested, startup fails with a readiness error until the official adapter is implemented.

This is the truthful outcome for the current repository. Do not claim live relay testing has passed.

## Required Adapter Behavior

The real adapter must be verified against the pinned Offchain Labs Nitro release documented in `docs/DEPENDENCIES.md`. Prefer official Nitro packages/utilities over manually invented binary decoding.

Required behavior:

- Relay reconnect with bounded backoff.
- Duplicate detection.
- Sequence gap detection.
- Malformed message handling.
- Unsupported message handling.
- Ordered transaction preservation.
- Fixture boundary tests.
- Local adapter tests that compile in CI.

Required metrics:

- `feed_messages_total`
- `feed_transactions_total`
- `feed_decode_errors_total`
- `feed_reconnects_total`
- `feed_sequence_gaps_total`
- `feed_duplicates_total`
- `feed_ingest_latency_seconds`

Current fixture decoder tests cover malformed frames, duplicate sequences, sequence gaps, unsupported-router fixture boundaries, incomplete-state fixture boundaries, and profitable/non-profitable fixture boundaries. They do not prove official Nitro websocket compatibility.
