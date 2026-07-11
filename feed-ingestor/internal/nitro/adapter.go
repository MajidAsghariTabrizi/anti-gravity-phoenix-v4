package nitro

import (
	"context"
	"encoding/binary"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"math/big"
	"strconv"

	"anti-gravity-phoenix-v4/feed-ingestor/internal/normalizer"

	"github.com/ethereum/go-ethereum/core/types"
)

const (
	NitroVersion = "v3.11.2"
	NitroImage   = "offchainlabs/nitro-node:v3.11.2-3599aca"

	FeedServerVersion = 2
	FeedClientVersion = 2

	HeaderFeedServerVersion                  = "Arbitrum-Feed-Server-Version"
	HeaderFeedClientVersion                  = "Arbitrum-Feed-Client-Version"
	HeaderRequestedSequence                  = "Arbitrum-Requested-Sequence-Number"
	HeaderChainID                            = "Arbitrum-Chain-Id"
	BroadcastVersion                         = 1
	L1MessageTypeL2Message             uint8 = 3
	L1MessageTypeEndOfBlock            uint8 = 6
	L1MessageTypeL2FundedByL1          uint8 = 7
	L1MessageTypeRollupEvent           uint8 = 8
	L1MessageTypeSubmitRetryable       uint8 = 9
	L1MessageTypeBatchForGasEstimation uint8 = 10
	L1MessageTypeInitialize            uint8 = 11
	L1MessageTypeEthDeposit            uint8 = 12
	L1MessageTypeBatchPostingReport    uint8 = 13
	L1MessageTypeInvalid               uint8 = 0xff

	L2MessageKindUnsignedUserTx   byte = 0
	L2MessageKindContractTx       byte = 1
	L2MessageKindNonmutatingCall  byte = 2
	L2MessageKindBatch            byte = 3
	L2MessageKindSignedTx         byte = 4
	L2MessageKindHeartbeat        byte = 6
	L2MessageKindSignedCompressed byte = 7

	ArbitrumOneChainID      uint64 = 42161
	MaxL2MessageSize               = 256 * 1024
	MaxL2BatchDepth                = 16
	MaxBroadcastMessageSize        = 64 * 1024 * 1024
	maxL2BatchItems                = MaxL2MessageSize/9 + 1
)

type BroadcastMessage struct {
	Version                        int                             `json:"version"`
	Messages                       []*BroadcastFeedMessage         `json:"messages,omitempty"`
	ConfirmedSequenceNumberMessage *ConfirmedSequenceNumberMessage `json:"confirmedSequenceNumberMessage,omitempty"`
}

type BroadcastFeedMessage struct {
	SequenceNumber uint64              `json:"sequenceNumber"`
	Message        MessageWithMetadata `json:"message"`
}

type MessageWithMetadata struct {
	Message             *L1IncomingMessage `json:"message"`
	DelayedMessagesRead uint64             `json:"delayedMessagesRead"`
}

type L1IncomingMessage struct {
	Header *L1IncomingMessageHeader `json:"header"`
	L2msg  []byte                   `json:"l2Msg"`
}

type L1IncomingMessageHeader struct {
	Kind        uint8  `json:"kind"`
	Sender      string `json:"sender,omitempty"`
	BlockNumber uint64 `json:"blockNumber,omitempty"`
	Timestamp   uint64 `json:"timestamp,omitempty"`
	RequestID   string `json:"requestId,omitempty"`
	BaseFeeL1   string `json:"baseFeeL1,omitempty"`
}

type ConfirmedSequenceNumberMessage struct {
	SequenceNumber uint64 `json:"sequenceNumber"`
}

type Frame struct {
	Sequence        uint64
	TimestampUnixMS uint64
	Transactions    []normalizer.RelayTx
	Unsupported     []string
	Malformed       []string
	Ignored         []string
}

type DecodeReport struct {
	ConfirmedSequence *uint64
	Unsupported       []string
	Malformed         []string
	Ignored           []string
}

type l2DecodeResult struct {
	transactions []normalizer.RelayTx
	unsupported  []string
	malformed    []string
	ignored      []string
}

type issueClass uint8

const (
	issueNone issueClass = iota
	issueUnsupported
	issueMalformed
)

func DecodeBroadcast(raw []byte) ([]Frame, DecodeReport, error) {
	return DecodeBroadcastContext(context.Background(), raw)
}

func DecodeBroadcastContext(ctx context.Context, raw []byte) ([]Frame, DecodeReport, error) {
	if err := ctx.Err(); err != nil {
		return nil, DecodeReport{}, err
	}
	if len(raw) > MaxBroadcastMessageSize {
		return nil, DecodeReport{}, fmt.Errorf("Nitro broadcast message exceeds %d bytes", MaxBroadcastMessageSize)
	}

	var message BroadcastMessage
	if err := json.Unmarshal(raw, &message); err != nil {
		return nil, DecodeReport{}, err
	}
	if err := ctx.Err(); err != nil {
		return nil, DecodeReport{}, err
	}
	if message.Version != BroadcastVersion {
		return nil, DecodeReport{}, fmt.Errorf("unsupported Nitro broadcast version %d", message.Version)
	}

	report := DecodeReport{}
	if message.ConfirmedSequenceNumberMessage != nil {
		sequence := message.ConfirmedSequenceNumberMessage.SequenceNumber
		report.ConfirmedSequence = &sequence
	}

	frames := make([]Frame, 0, len(message.Messages))
	for _, feedMessage := range message.Messages {
		if err := ctx.Err(); err != nil {
			return nil, DecodeReport{}, err
		}
		frame, err := decodeFeedMessage(ctx, feedMessage)
		if err != nil {
			return nil, DecodeReport{}, err
		}
		report.Unsupported = append(report.Unsupported, frame.Unsupported...)
		report.Malformed = append(report.Malformed, frame.Malformed...)
		report.Ignored = append(report.Ignored, frame.Ignored...)
		if frame.Sequence != 0 || len(frame.Transactions) > 0 || len(frame.Unsupported) > 0 || len(frame.Malformed) > 0 || len(frame.Ignored) > 0 {
			frames = append(frames, frame)
		}
	}
	return frames, report, nil
}

func decodeFeedMessage(ctx context.Context, feedMessage *BroadcastFeedMessage) (Frame, error) {
	if feedMessage == nil {
		return Frame{Malformed: []string{"nil broadcast feed message"}}, nil
	}
	frame := Frame{Sequence: feedMessage.SequenceNumber}
	incoming := feedMessage.Message.Message
	if incoming == nil {
		frame.Malformed = append(frame.Malformed, "missing incoming message")
		return frame, nil
	}
	if incoming.Header == nil {
		frame.Malformed = append(frame.Malformed, "missing incoming message header")
		return frame, nil
	}
	frame.TimestampUnixMS = incoming.Header.Timestamp * 1000
	if incoming.Header.Kind != L1MessageTypeL2Message {
		reason := fmt.Sprintf("L1 message kind %d", incoming.Header.Kind)
		if isIgnoredNonTransactionKind(incoming.Header.Kind) {
			frame.Ignored = append(frame.Ignored, reason)
		} else {
			frame.Unsupported = append(frame.Unsupported, "unsupported "+reason)
		}
		return frame, nil
	}

	result, err := decodeL2Message(ctx, incoming.L2msg, 0)
	if err != nil {
		return Frame{}, err
	}
	frame.Transactions = append(frame.Transactions, result.transactions...)
	frame.Unsupported = append(frame.Unsupported, result.unsupported...)
	frame.Malformed = append(frame.Malformed, result.malformed...)
	frame.Ignored = append(frame.Ignored, result.ignored...)
	return frame, nil
}

func decodeL2Message(ctx context.Context, raw []byte, depth int) (l2DecodeResult, error) {
	if err := ctx.Err(); err != nil {
		return l2DecodeResult{}, err
	}
	if len(raw) == 0 {
		return l2DecodeResult{malformed: []string{"empty L2 message"}}, nil
	}
	if len(raw) > MaxL2MessageSize {
		return l2DecodeResult{malformed: []string{fmt.Sprintf("L2 message exceeds %d bytes", MaxL2MessageSize)}}, nil
	}

	switch raw[0] {
	case L2MessageKindBatch:
		return decodeL2Batch(ctx, raw[1:], depth)
	case L2MessageKindSignedTx:
		tx, class, err := decodeSignedEthereumTx(raw[1:])
		if err == nil {
			return l2DecodeResult{transactions: []normalizer.RelayTx{tx}}, nil
		}
		if class == issueUnsupported {
			return l2DecodeResult{unsupported: []string{err.Error()}}, nil
		}
		return l2DecodeResult{malformed: []string{err.Error()}}, nil
	case L2MessageKindHeartbeat:
		return l2DecodeResult{ignored: []string{"heartbeat L2 message"}}, nil
	case L2MessageKindUnsignedUserTx,
		L2MessageKindContractTx,
		L2MessageKindNonmutatingCall,
		L2MessageKindSignedCompressed:
		return l2DecodeResult{unsupported: []string{fmt.Sprintf("unsupported L2 message kind 0x%02x", raw[0])}}, nil
	default:
		return l2DecodeResult{unsupported: []string{fmt.Sprintf("unknown L2 message kind 0x%02x", raw[0])}}, nil
	}
}

func decodeL2Batch(ctx context.Context, payload []byte, depth int) (l2DecodeResult, error) {
	if depth >= MaxL2BatchDepth {
		return l2DecodeResult{malformed: []string{fmt.Sprintf("L2 message batches exceed max depth %d", MaxL2BatchDepth)}}, nil
	}
	nested, err := splitL2Batch(ctx, payload)
	if err != nil {
		if ctx.Err() != nil {
			return l2DecodeResult{}, ctx.Err()
		}
		return l2DecodeResult{malformed: []string{"malformed L2 batch: " + err.Error()}}, nil
	}
	if len(nested) == 0 {
		return l2DecodeResult{ignored: []string{"empty L2 message batch"}}, nil
	}

	result := l2DecodeResult{}
	for index, nestedMessage := range nested {
		if err := ctx.Err(); err != nil {
			return l2DecodeResult{}, err
		}
		nestedResult, err := decodeL2Message(ctx, nestedMessage, depth+1)
		if err != nil {
			return l2DecodeResult{}, err
		}
		result.transactions = append(result.transactions, nestedResult.transactions...)
		result.unsupported = appendPrefixed(result.unsupported, nestedResult.unsupported, index)
		result.malformed = appendPrefixed(result.malformed, nestedResult.malformed, index)
		result.ignored = appendPrefixed(result.ignored, nestedResult.ignored, index)
	}
	return result, nil
}

func splitL2Batch(ctx context.Context, payload []byte) ([][]byte, error) {
	messages := make([][]byte, 0)
	for offset := 0; offset < len(payload); {
		if err := ctx.Err(); err != nil {
			return nil, err
		}
		if len(messages) >= maxL2BatchItems {
			return nil, errors.New("too many batch items")
		}
		if len(payload)-offset < 8 {
			return nil, errors.New("truncated uint64 batch item length")
		}
		size := binary.BigEndian.Uint64(payload[offset : offset+8])
		offset += 8
		if size == 0 {
			return nil, errors.New("empty batch item")
		}
		if size > MaxL2MessageSize {
			return nil, fmt.Errorf("batch item length %d exceeds %d", size, MaxL2MessageSize)
		}
		if size > uint64(len(payload)-offset) {
			return nil, fmt.Errorf("batch item length %d exceeds remaining %d bytes", size, len(payload)-offset)
		}
		end := offset + int(size)
		messages = append(messages, payload[offset:end])
		offset = end
	}
	return messages, nil
}

func decodeSignedEthereumTx(raw []byte) (normalizer.RelayTx, issueClass, error) {
	if len(raw) == 0 {
		return normalizer.RelayTx{}, issueMalformed, errors.New("signed Ethereum transaction payload is empty")
	}
	if raw[0] <= 0x7f {
		switch raw[0] {
		case types.AccessListTxType, types.DynamicFeeTxType, types.SetCodeTxType:
		case types.BlobTxType:
			return normalizer.RelayTx{}, issueUnsupported, errors.New("unsupported signed Ethereum blob transaction")
		default:
			return normalizer.RelayTx{}, issueUnsupported, fmt.Errorf("unsupported signed Ethereum transaction type 0x%02x", raw[0])
		}
	}

	tx := new(types.Transaction)
	if err := tx.UnmarshalBinary(raw); err != nil {
		if errors.Is(err, types.ErrTxTypeNotSupported) {
			return normalizer.RelayTx{}, issueUnsupported, errors.New("unsupported signed Ethereum transaction type")
		}
		return normalizer.RelayTx{}, issueMalformed, fmt.Errorf("decode signed Ethereum transaction: %w", err)
	}
	if tx.Type() == types.BlobTxType {
		return normalizer.RelayTx{}, issueUnsupported, errors.New("unsupported signed Ethereum blob transaction")
	}

	expectedChainID := new(big.Int).SetUint64(ArbitrumOneChainID)
	actualChainID := tx.ChainId()
	if tx.Type() != types.LegacyTxType || tx.Protected() {
		if actualChainID == nil || actualChainID.Cmp(expectedChainID) != 0 {
			return normalizer.RelayTx{}, issueMalformed, fmt.Errorf("signed Ethereum transaction chain id is not %d", ArbitrumOneChainID)
		}
	}

	var signer types.Signer
	if tx.Type() == types.LegacyTxType && !tx.Protected() {
		signer = types.HomesteadSigner{}
	} else {
		signer = types.LatestSignerForChainID(expectedChainID)
	}
	from, err := types.Sender(signer, tx)
	if err != nil {
		return normalizer.RelayTx{}, issueMalformed, fmt.Errorf("recover signed Ethereum transaction sender: %w", err)
	}

	to := ""
	if destination := tx.To(); destination != nil {
		to = destination.Hex()
	}
	return normalizer.RelayTx{
		Hash:                 tx.Hash().Hex(),
		Type:                 fmt.Sprintf("0x%02x", tx.Type()),
		ChainID:              ArbitrumOneChainID,
		From:                 from.Hex(),
		To:                   to,
		Nonce:                tx.Nonce(),
		Value:                tx.Value().String(),
		Calldata:             "0x" + hexLower(tx.Data()),
		GasLimit:             strconv.FormatUint(tx.Gas(), 10),
		MaxFeePerGas:         tx.GasFeeCap().String(),
		MaxPriorityFeePerGas: tx.GasTipCap().String(),
		RawTx:                "0x" + hexLower(raw),
	}, issueNone, nil
}

func appendPrefixed(destination, reasons []string, index int) []string {
	for _, reason := range reasons {
		destination = append(destination, fmt.Sprintf("batch item %d: %s", index, reason))
	}
	return destination
}

func hexLower(input []byte) string {
	return hex.EncodeToString(input)
}

func isIgnoredNonTransactionKind(kind uint8) bool {
	switch kind {
	case L1MessageTypeEndOfBlock,
		L1MessageTypeRollupEvent,
		L1MessageTypeBatchForGasEstimation,
		L1MessageTypeInitialize,
		L1MessageTypeBatchPostingReport:
		return true
	default:
		return false
	}
}
