# Nitro Feed Integration

Status: relay adapter implemented for first runtime verification; production relay mode can start in SHADOW, but real feed validation evidence is still required before claiming production readiness.

Phoenix supports deterministic fixture JSON frames, stdin line input, and relay mode in `feed-ingestor`.

The relay adapter is pinned to Offchain Labs Nitro `v3.11.2` semantics recorded in `docs/DEPENDENCIES.md`. It implements:

- WebSocket connection to the local Nitro feed relay using feed protocol version `2` headers.
- Bounded reconnect backoff.
- Nitro broadcast envelope decoding for `version: 1`.
- Ordered feed sequence tracking across reconnects.
- Duplicate, gap, out-of-order, and feed-reset handling.
- Transaction normalization for Arbitrum unsigned transaction payload type `0x65`.
- Unsupported Nitro message and payload accounting without publishing synthetic transactions.

Primary Nitro source references used for the local adapter:

- `broadcaster/message/message.go`: `BroadcastMessage` and `BroadcastFeedMessage`.
- `arbos/arbostypes/messagewithmeta.go`: `MessageWithMetadata`.
- `arbos/arbostypes/incomingmessage.go`: `L1IncomingMessage`, header, and L2 message kind.
- `arbnode/transaction_streamer.go`: Sequencer feed message flow into Nitro transaction streamer.
- `wsbroadcastserver/wsbroadcastserver.go`: feed protocol version and WebSocket headers.
- Nitro `go-ethereum` submodule `f3a977ddf30b138da2fe673ac5cbff2bc6dd4c88`: transaction type identifiers and Arbitrum unsigned transaction type `0x65`.

The adapter intentionally does not claim broad production coverage for every Nitro payload shape. Ignored non-transaction feed messages can advance sequence state when the envelope is valid, but they do not publish to `phoenix.feed.tx`. Unsupported transaction-like or unknown messages advance sequence state only after the envelope sequence is accepted, increment unsupported metrics, and are rejected without being misclassified as decoder corruption or failing readiness by themselves.

Unsupported cases:

- Non-`version: 1` Nitro broadcast envelopes fail decode.
- Malformed JSON fails decode.
- Transaction-like L1 message kinds other than L2 message kind `3` are counted as unsupported and not published.
- L2 payload types other than Arbitrum unsigned transaction type `0x65` are counted as unsupported and not published.
- Compressed WebSocket frames are not negotiated by Phoenix relay mode.

## Supported Message And Payload Matrix

Pinned upstream source: Offchain Labs Nitro `v3.11.2` and OffchainLabs/go-ethereum submodule `f3a977ddf30b138da2fe673ac5cbff2bc6dd4c88`.

L1 incoming message kinds:

| Identifier | Upstream name | Phoenix status |
| --- | --- | --- |
| `3` | `L1MessageType_L2Message` | Supported only when `l2Msg` is Arbitrum unsigned transaction payload `0x65`. |
| `6` | `L1MessageType_EndOfBlock` | Ignored as non-transaction; may advance sequence state. |
| `7` | `L1MessageType_L2FundedByL1` | Not yet implemented; counted unsupported. |
| `8` | `L1MessageType_RollupEvent` | Ignored as non-transaction; may advance sequence state. |
| `9` | `L1MessageType_SubmitRetryable` | Not yet implemented; counted unsupported. |
| `10` | `L1MessageType_BatchForGasEstimation` | Ignored as non-transaction; may advance sequence state. |
| `11` | `L1MessageType_Initialize` | Ignored as non-transaction; may advance sequence state. |
| `12` | `L1MessageType_EthDeposit` | Not yet implemented; counted unsupported. |
| `13` | `L1MessageType_BatchPostingReport` | Ignored as non-transaction; may advance sequence state. |
| `0xff` | `L1MessageType_Invalid` | Explicitly rejected as unsupported. |
| other | unknown future message kind | Explicitly rejected as unsupported. |

L2 transaction payload types:

| Identifier | Upstream name | Phoenix status |
| --- | --- | --- |
| raw RLP list | standard legacy signed transaction | Not yet implemented; explicitly rejected because no honest `from` is derived without a signer-aware transaction parser. |
| `0x00` | `LegacyTxType` | Explicitly rejected. |
| `0x01` | `AccessListTxType` | Not yet implemented; explicitly rejected. |
| `0x02` | `DynamicFeeTxType` | Not yet implemented; explicitly rejected. |
| `0x03` | `BlobTxType` | Not yet implemented; explicitly rejected. |
| `0x04` | `SetCodeTxType` | Not yet implemented; explicitly rejected. |
| `0x64` | `ArbitrumDepositTxType` | Not yet implemented; explicitly rejected. |
| `0x65` | `ArbitrumUnsignedTxType` | Supported for Arbitrum One chain id `42161`. |
| `0x66` | `ArbitrumContractTxType` | Not yet implemented; explicitly rejected. |
| `0x68` | `ArbitrumRetryTxType` | Not yet implemented; explicitly rejected. |
| `0x69` | `ArbitrumSubmitRetryableTxType` | Not yet implemented; explicitly rejected. |
| `0x6a` | `ArbitrumInternalTxType` | Not yet implemented; explicitly rejected. |
| `0x78` | `ArbitrumLegacyTxType` | Not yet implemented; explicitly rejected. |
| other | unknown future transaction type | Explicitly rejected as unsupported. |

This matrix is deliberately conservative. Phoenix must not claim broad Arbitrum transaction support until official Nitro/go-ethereum parsing is linked or equivalent live fixture coverage exists.

## Custom RLP Status

The local RLP helper exists only because the workspace could not fetch the desired Nitro/go-ethereum dependency graph. It is a minimal canonical RLP parser for the supported Arbitrum unsigned transaction payload subset. Tests cover single bytes, short/long strings, short/long lists, nested lists, empty string/list, trailing bytes, truncation, non-canonical length encodings, length overflow, integer leading-zero rejection, and malformed nested values.

Production readiness claims remain blocked until either this subset is proven against real feed fixtures or replaced with official go-ethereum/Nitro decoding.

## Custom Keccak Status

The local Keccak helper implements Ethereum legacy Keccak-256 using rate `136`, Keccak padding suffix `0x01`, and Keccak-f[1600] constants/rotations. Tests cover Keccak-256 of empty input, `abc`, binary bytes `000102030405`, and a multi-block 256-byte input. These vectors distinguish Ethereum Keccak from SHA3-256.

Production readiness claims still require custom crypto to be replaced with or verified against an official dependency before a live release gate is lifted.

## WebSocket Source Status

The relay source implements a minimal local `ws://` WebSocket client for the Nitro relay. It verifies `Sec-WebSocket-Accept`, feed server version `2`, and chain id `42161`; rejects `wss://`, masked server frames, fragmented frames, reserved bits, oversized frames, oversized control frames, and unknown opcodes; handles text, binary, ping, pong, and close frames; and reconnects with bounded backoff.

Compressed feed frames are not negotiated. Fragmentation is explicitly rejected rather than reassembled. The source is suitable for the first local runtime verification path, not for claiming final production feed readiness.

## Runtime Modes

- `PHOENIX_FEED_SOURCE=fixture`: deterministic development and CI mode. Requires `PHOENIX_FEED_FIXTURE`.
- `PHOENIX_FEED_SOURCE=relay`: SHADOW runtime verification mode. Requires `PHOENIX_FEED_RELAY_URL`.
- `PHOENIX_FEED_SOURCE=stdin`: line-oriented deterministic input for local tooling.

Production rejects any configured fixture path and requires relay mode. Relay startup is allowed for SHADOW runtime verification, but readiness still depends on real source connection, NATS reachability, sequence integrity, and at least one valid normalized transaction-bearing message.

## Sequence Integrity Policy

The sequence number belongs to each Nitro `BroadcastFeedMessage`, not to a WebSocket frame or an individual transaction. One WebSocket broadcast may contain multiple feed messages. One feed message contains one L1 incoming message whose L2 payload may normalize to zero, one, or multiple transactions; sequence state advances exactly once for that feed message.

Pinned Nitro `v3.11.2-3599aca` expects messages within one delivered batch to be contiguous. Its relay backlog can nevertheless discard older segments when an upstream jump occurs, and the official broadcast client advances its requested next sequence to every accepted feed message. Phoenix therefore records a received forward discontinuity once and advances its bounded local baseline instead of waiting indefinitely for messages the relay will not send later.

The relay path uses this explicit state machine:

- `FIRST_MESSAGE`: the first observed feed sequence establishes local sequence state.
- `IN_ORDER`: `sequence == previous + 1`; the message is publishable after normalization.
- `DUPLICATE`: `sequence == previous`; the message is ignored and never republished.
- `GAP`: `sequence > previous + 1`; the missing range and count are recorded once, the received current message remains publishable, and it becomes the new baseline.
- `REGRESSION`: `sequence < previous`; the message is not published and integrity readiness fails for the process lifetime.
- `RECONNECT`: the first message after reconnect is accepted only if it continues the expected next sequence.

A forward gap makes readiness transiently unhealthy. The next contiguous message re-establishes local continuity and recovers readiness, but the gap and missing-message count remain durable metrics and smoke evidence. A regression or malformed supported payload is a terminal integrity condition until operator restart; unsupported payload coverage is counted and rejected without being misclassified as decoder corruption.

## Reconnect Policy

Relay mode reconnects to the local Nitro feed WebSocket with bounded exponential backoff from 250 ms to 5 seconds. Disconnect immediately clears source and sequence readiness. On reconnect, Phoenix requests the next expected sequence and does not blindly reset sequence state. A continuing sequence is `RECONNECT`, a forward discontinuity is `GAP`, and a backward discontinuity is a terminal `REGRESSION`. Fresh sequence evidence is required before readiness recovers.

## Normalized Message Contract

`proto/phoenix.proto` remains the canonical contract. The Go ingestor still publishes canonical JSON matching `NormalizedTx` because generated Protobuf bindings are not part of this workspace.

For supported Nitro Arbitrum unsigned transactions, the adapter derives:

- `sequence` from the Nitro broadcast sequence number.
- `timestamp_unix_ms` from the Nitro incoming message header timestamp.
- `tx_hash` as Keccak-256 over the typed Arbitrum transaction payload.
- `tx_type` as `0x65`.
- `chain_id`, `from`, `to`, `nonce`, `value`, `calldata`, `gas_limit`, and `max_fee_per_gas` from the decoded Arbitrum unsigned transaction RLP payload.
- `max_priority_fee_per_gas` as `0` because the Arbitrum unsigned payload has a single gas fee cap field.
- `raw_tx` from the original typed transaction bytes.

If a field cannot be derived honestly from a supported payload, normalization fails and the transaction is not published.

## NATS Publishing

Valid normalized transactions publish synchronously to JetStream subject `phoenix.feed.tx` using `nats.go`. The publisher idempotently creates or updates stream `PHOENIX_FEED_TX`, supplies a stable sequence-plus-transaction-hash message ID, expects the configured stream, and counts success only after the server returns a valid persistence acknowledgement. A timeout, missing stream, disconnected server, or invalid acknowledgement increments durable publish failure metrics and clears readiness. No public RPC read or fixture fallback is part of this path.

## Readiness Policy

Liveness only reports that the process is alive. Readiness requires:

- source initialized
- feed adapter initialized
- relay or input source connected
- NATS reachable
- at least one valid normalized transaction-bearing feed message observed
- sequence state known
- no currently unresolved forward gap
- no terminal decoder, normalization, or sequence-regression integrity condition

Readiness effects are explicit:

- Decoder corruption or malformed supported data: terminal integrity failure; no malformed transaction is published.
- Unsupported message: counted and rejected; no readiness failure by itself.
- Forward sequence gap: transient readiness failure until the next contiguous feed message.
- Duplicate: counted and ignored; no readiness failure by itself.
- Sequence regression: terminal integrity failure.
- Disconnect or reconnect attempt: not ready until a new accepted sequence is observed.
- JetStream unavailable or Publish ACK timeout: not ready; publication returns an error.

The production guard is deliberately limited to source selection:

- If `PHOENIX_ENV=production` and `PHOENIX_FEED_FIXTURE` is set, startup fails.
- If `PHOENIX_ENV=production` and `PHOENIX_FEED_SOURCE` is not `relay`, startup fails.
- If `PHOENIX_ENV=production` and `PHOENIX_FEED_RELAY_URL` is missing, startup fails.

Do not claim live relay testing has passed until a real relay smoke test has actually run.

## Required Adapter Behavior

The real adapter must stay verified against the pinned Offchain Labs Nitro release documented in `docs/DEPENDENCIES.md`. The current implementation mirrors the official Nitro broadcast JSON envelope locally because this workspace could not fetch the Nitro/go-ethereum dependency graph. Before production unblocking, prefer replacing the local unsigned-payload parser with official Nitro/go-ethereum parsing utilities or prove equivalent coverage with live feed fixtures.

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

- `feed_connections_total`
- `feed_messages_total`
- `feed_normalized_transactions_total`
- `feed_decode_failures_total`
- `feed_reconnects_total`
- `feed_sequence_gaps_total`
- `feed_sequence_gap_messages_total`
- `feed_sequence_regressions_total`
- `feed_sequence_duplicates_total`
- `feed_duplicates_total`
- `feed_out_of_order_total`
- `feed_publish_success_total`
- `feed_publish_failures_total`
- `feed_jetstream_publish_success_total`
- `feed_jetstream_publish_failures_total`
- `feed_jetstream_publish_latency`
- `feed_jetstream_stream_unavailable_total`
- `feed_unsupported_messages_total`
- `feed_last_sequence`
- `feed_last_message_timestamp`
- `feed_readiness`
- `feed_ingest_latency_seconds`

Current fixture decoder tests cover malformed frames, duplicate sequences, sequence gaps, unsupported-router fixture boundaries, incomplete-state fixture boundaries, and profitable/non-profitable fixture boundaries. They do not prove official Nitro websocket compatibility.

Current relay adapter tests cover Nitro broadcast envelope decoding, Arbitrum unsigned transaction payload extraction, Keccak-256 transaction hashing, unsupported message kinds, unsupported payload types, reconnect-aware sequence state, readiness gates, and publish failure accounting. They do not prove real feed operation on this Windows workstation.

## Real Feed Test Result

Not run in this workspace. A valid real feed smoke test still requires running the pinned Nitro relay against the real Arbitrum sequencer feed, observing an actual broadcast sequence, decoding a supported transaction-bearing payload, and publishing an honest normalized transaction to NATS without public RPC reconstruction.

The exact remaining production blocker is real-feed evidence plus broader unsupported payload coverage or official Nitro/go-ethereum parser integration.
