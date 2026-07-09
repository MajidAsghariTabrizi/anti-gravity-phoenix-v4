package nitro

import (
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strconv"

	"anti-gravity-phoenix-v4/feed-ingestor/internal/normalizer"
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

	LegacyTxType     byte = 0x00
	AccessListTxType byte = 0x01
	DynamicFeeTxType byte = 0x02
	BlobTxType       byte = 0x03
	SetCodeTxType    byte = 0x04

	ArbitrumDepositTxType         byte = 0x64
	ArbitrumUnsignedTxType        byte = 0x65
	ArbitrumContractTxType        byte = 0x66
	ArbitrumRetryTxType           byte = 0x68
	ArbitrumSubmitRetryableTxType byte = 0x69
	ArbitrumInternalTxType        byte = 0x6a
	ArbitrumLegacyTxType          byte = 0x78

	ArbitrumOneChainID uint64 = 42161
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
	Ignored         []string
}

type DecodeReport struct {
	ConfirmedSequence *uint64
	Unsupported       []string
	Ignored           []string
}

func DecodeBroadcast(raw []byte) ([]Frame, DecodeReport, error) {
	var message BroadcastMessage
	if err := json.Unmarshal(raw, &message); err != nil {
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
		frame := decodeFeedMessage(feedMessage)
		if len(frame.Unsupported) > 0 {
			report.Unsupported = append(report.Unsupported, frame.Unsupported...)
		}
		if len(frame.Ignored) > 0 {
			report.Ignored = append(report.Ignored, frame.Ignored...)
		}
		if frame.Sequence != 0 || len(frame.Transactions) > 0 || len(frame.Unsupported) > 0 || len(frame.Ignored) > 0 {
			frames = append(frames, frame)
		}
	}
	return frames, report, nil
}

func decodeFeedMessage(feedMessage *BroadcastFeedMessage) Frame {
	if feedMessage == nil {
		return Frame{Unsupported: []string{"nil broadcast feed message"}}
	}
	frame := Frame{Sequence: feedMessage.SequenceNumber}
	incoming := feedMessage.Message.Message
	if incoming == nil {
		frame.Unsupported = append(frame.Unsupported, "missing incoming message")
		return frame
	}
	if incoming.Header == nil {
		frame.Unsupported = append(frame.Unsupported, "missing incoming message header")
		return frame
	}
	frame.TimestampUnixMS = incoming.Header.Timestamp * 1000
	if incoming.Header.Kind != L1MessageTypeL2Message {
		reason := fmt.Sprintf("L1 message kind %d", incoming.Header.Kind)
		if isIgnoredNonTransactionKind(incoming.Header.Kind) {
			frame.Ignored = append(frame.Ignored, reason)
		} else {
			frame.Unsupported = append(frame.Unsupported, "unsupported "+reason)
		}
		return frame
	}
	tx, err := decodeL2Message(incoming.L2msg)
	if err != nil {
		frame.Unsupported = append(frame.Unsupported, err.Error())
		return frame
	}
	frame.Transactions = append(frame.Transactions, tx)
	return frame
}

func decodeL2Message(raw []byte) (normalizer.RelayTx, error) {
	if len(raw) == 0 {
		return normalizer.RelayTx{}, errors.New("empty L2 message")
	}
	if raw[0] != ArbitrumUnsignedTxType {
		return normalizer.RelayTx{}, fmt.Errorf("unsupported L2 transaction payload type 0x%02x", raw[0])
	}
	return decodeArbitrumUnsignedTx(raw)
}

func decodeArbitrumUnsignedTx(raw []byte) (normalizer.RelayTx, error) {
	value, rest, err := parseRLP(raw[1:])
	if err != nil {
		return normalizer.RelayTx{}, fmt.Errorf("decode arbitrum unsigned tx rlp: %w", err)
	}
	if len(rest) != 0 {
		return normalizer.RelayTx{}, errors.New("trailing bytes after arbitrum unsigned tx rlp")
	}
	if !value.isList {
		return normalizer.RelayTx{}, errors.New("arbitrum unsigned tx payload is not an rlp list")
	}
	if len(value.list) != 8 {
		return normalizer.RelayTx{}, fmt.Errorf("arbitrum unsigned tx has %d fields", len(value.list))
	}

	chainID, err := rlpUint(value.list[0])
	if err != nil {
		return normalizer.RelayTx{}, fmt.Errorf("decode chain id: %w", err)
	}
	from, err := rlpAddress(value.list[1])
	if err != nil {
		return normalizer.RelayTx{}, fmt.Errorf("decode from address: %w", err)
	}
	nonce, err := rlpUint(value.list[2])
	if err != nil {
		return normalizer.RelayTx{}, fmt.Errorf("decode nonce: %w", err)
	}
	gasFeeCap, err := rlpBig(value.list[3])
	if err != nil {
		return normalizer.RelayTx{}, fmt.Errorf("decode gas fee cap: %w", err)
	}
	gas, err := rlpUint(value.list[4])
	if err != nil {
		return normalizer.RelayTx{}, fmt.Errorf("decode gas limit: %w", err)
	}
	to, err := rlpOptionalAddress(value.list[5])
	if err != nil {
		return normalizer.RelayTx{}, fmt.Errorf("decode to address: %w", err)
	}
	amount, err := rlpBig(value.list[6])
	if err != nil {
		return normalizer.RelayTx{}, fmt.Errorf("decode value: %w", err)
	}
	if value.list[7].isList {
		return normalizer.RelayTx{}, errors.New("decode calldata: rlp list is not bytes")
	}

	hash := keccak256(raw)
	return normalizer.RelayTx{
		Hash:                 "0x" + hexLower(hash[:]),
		Type:                 "0x65",
		ChainID:              chainID,
		From:                 from,
		To:                   to,
		Nonce:                nonce,
		Value:                amount.String(),
		Calldata:             "0x" + hexLower(value.list[7].payload),
		GasLimit:             strconv.FormatUint(gas, 10),
		MaxFeePerGas:         gasFeeCap.String(),
		MaxPriorityFeePerGas: "0",
		RawTx:                "0x" + hexLower(raw),
	}, nil
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
