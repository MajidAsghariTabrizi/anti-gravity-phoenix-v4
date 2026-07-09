# Nitro Feed Integration

Status: relay adapter implemented for first runtime verification; production relay ingestion remains blocked until live validation passes.

Phoenix supports deterministic fixture JSON frames, stdin line input, and non-production relay mode in `feed-ingestor`.

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

The adapter intentionally does not claim broad production coverage for every Nitro payload shape. Ignored non-transaction feed messages can advance sequence state when the envelope is valid, but they do not publish to `phoenix.feed.tx`. Unsupported transaction-like or unknown messages advance sequence state only after the envelope sequence is accepted, increment unsupported metrics, and keep readiness unhealthy.

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

Production readiness stays blocked until either this subset is proven against real feed fixtures or replaced with official go-ethereum/Nitro decoding.

## Custom Keccak Status

The local Keccak helper implements Ethereum legacy Keccak-256 using rate `136`, Keccak padding suffix `0x01`, and Keccak-f[1600] constants/rotations. Tests cover Keccak-256 of empty input, `abc`, binary bytes `000102030405`, and a multi-block 256-byte input. These vectors distinguish Ethereum Keccak from SHA3-256.

Production readiness still remains blocked because custom crypto should be replaced with or verified against an official dependency before a live release gate is lifted.

## WebSocket Source Status

The relay source implements a minimal local `ws://` WebSocket client for the Nitro relay. It verifies `Sec-WebSocket-Accept`, feed server version `2`, and chain id `42161`; rejects `wss://`, masked server frames, fragmented frames, reserved bits, oversized frames, oversized control frames, and unknown opcodes; handles text, binary, ping, pong, and close frames; and reconnects with bounded backoff.

Compressed feed frames are not negotiated. Fragmentation is explicitly rejected rather than reassembled. The source is suitable for the first local runtime verification path, not for claiming final production feed readiness.

## Runtime Modes

- `PHOENIX_FEED_SOURCE=fixture`: deterministic development and CI mode. Requires `PHOENIX_FEED_FIXTURE`.
- `PHOENIX_FEED_SOURCE=relay`: non-production runtime verification mode. Requires `PHOENIX_FEED_RELAY_URL`.
- `PHOENIX_FEED_SOURCE=stdin`: line-oriented deterministic input for local tooling.

Production rejects any configured fixture path and refuses startup until real relay validation has passed.

## Sequence Integrity Policy

The relay path uses an explicit state machine:

- `FIRST_MESSAGE`: the first observed feed sequence establishes local sequence state.
- `IN_ORDER`: `sequence == previous + 1`; the message is publishable after normalization.
- `DUPLICATE`: an already seen sequence is ignored and never republished.
- `GAP`: a future sequence is observed before the expected next sequence; readiness is degraded and the future message is not published.
- `OUT_OF_ORDER`: an older unseen sequence is observed without a reconnect reset signal; readiness remains degraded and the message is not published.
- `RECONNECT`: the first message after reconnect is accepted only if it continues the expected next sequence.
- `FEED_RESET`: the first message after reconnect is older than the expected next sequence and not already seen; Phoenix does not reset local sequence state or publish the message.

Unresolved gaps and feed resets keep readiness unhealthy until ordered sequence evidence recovers. This preserves SHADOW observation evidence without presenting degraded feed data as trustworthy opportunity input.

## Reconnect Policy

Relay mode reconnects to the local Nitro feed WebSocket with bounded exponential backoff from 250 ms to 5 seconds. On reconnect, Phoenix requests the next expected sequence from the relay and does not blindly reset sequence state. A continuing sequence is accepted as `RECONNECT`; a forward discontinuity is `GAP`; a backward discontinuity is `FEED_RESET`.

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

Valid normalized transactions publish synchronously to NATS Core subject `phoenix.feed.tx`. The publisher uses the existing NATS Core TCP protocol implementation, applies a write deadline, records publish success/failure counters, and returns errors instead of dropping failed publishes silently. JetStream is not introduced into the hot path.

## Readiness Policy

Liveness only reports that the process is alive. Readiness requires:

- source initialized
- feed adapter initialized
- relay or input source connected
- NATS reachable
- at least one valid normalized transaction-bearing feed message observed
- sequence state known
- no unresolved sequence gap or feed reset condition
- no unsupported transaction-like or unknown feed coverage observed

The production guard is deliberate:

- If `PHOENIX_ENV=production` and `PHOENIX_FEED_FIXTURE` is set, startup fails.
- If `PHOENIX_ENV=production` and `PHOENIX_FEED_SOURCE` is not `relay`, startup fails.
- If `PHOENIX_ENV=production` and `PHOENIX_FEED_RELAY_URL` is missing, startup fails.
- If production relay mode is requested, startup fails until the relay adapter has been live-verified against the real Arbitrum sequencer feed.

This is the truthful outcome for the current repository. Do not claim live relay testing has passed.

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
- `feed_duplicates_total`
- `feed_out_of_order_total`
- `feed_publish_success_total`
- `feed_publish_failures_total`
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
