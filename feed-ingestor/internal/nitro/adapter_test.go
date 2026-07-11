package nitro

import (
	"context"
	"encoding/binary"
	"encoding/json"
	"errors"
	"math/big"
	"strings"
	"testing"

	"github.com/ethereum/go-ethereum/common"
	"github.com/ethereum/go-ethereum/core/types"
	"github.com/ethereum/go-ethereum/crypto"
)

func TestKeccak256Vectors(t *testing.T) {
	tests := []struct {
		name string
		data []byte
		want string
	}{
		{name: "empty", data: nil, want: "c5d2460186f7233c927e7db2dcc703c0e500b653ca82273b7bfad8045d85a470"},
		{name: "abc", data: []byte("abc"), want: "4e03657aea45a94fc7d47ba826c8d667c0d1e6e33a64a036ec44f58fa12d6c45"},
		{name: "binary", data: []byte{0, 1, 2, 3, 4, 5}, want: "51e8babe8b42352100dffa7f7b3843c95245d3d545c6cbf5052e80258ae80627"},
		{name: "multi-block", data: byteRange(256), want: "dc924469b334aed2a19fac7252e9961aea41f8d91996366029dbe0884229bf36"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			hash := keccak256(tt.data)
			if got := hexLower(hash[:]); got != tt.want {
				t.Fatalf("unexpected keccak256 hash: got %s want %s", got, tt.want)
			}
		})
	}
}

func TestDecodeBroadcastPayloadType04SignedLegacyTransaction(t *testing.T) {
	signed := signTestTransaction(t, types.NewTx(&types.LegacyTx{
		Nonce:    7,
		GasPrice: big.NewInt(100),
		Gas:      21000,
		To:       testDestination(),
		Value:    big.NewInt(3),
		Data:     []byte{0x12, 0x34},
	}), new(big.Int).SetUint64(ArbitrumOneChainID))
	rawSigned := signedTransactionMessage(t, signed)
	if rawSigned[0] != L2MessageKindSignedTx {
		t.Fatalf("expected payload type 0x04, got 0x%02x", rawSigned[0])
	}

	frame, report := decodeSingleFrame(t, rawSigned)
	if len(report.Unsupported) != 0 || len(report.Malformed) != 0 {
		t.Fatalf("unexpected issues: %+v", report)
	}
	if frame.Sequence != 460530858 || frame.TimestampUnixMS != 1700000000000 {
		t.Fatalf("unexpected frame metadata: %+v", frame)
	}
	if len(frame.Transactions) != 1 {
		t.Fatalf("expected one transaction, got %d", len(frame.Transactions))
	}

	got := frame.Transactions[0]
	encoded, err := signed.MarshalBinary()
	if err != nil {
		t.Fatal(err)
	}
	if got.Hash != signed.Hash().Hex() || got.RawTx != "0x"+hexLower(encoded) {
		t.Fatalf("transaction hash/raw mismatch: %+v", got)
	}
	if got.From != testSender(t).Hex() || got.To != testDestination().Hex() {
		t.Fatalf("sender recovery mismatch: %+v", got)
	}
	if got.Type != "0x00" || got.ChainID != ArbitrumOneChainID || got.Nonce != 7 {
		t.Fatalf("transaction identity mismatch: %+v", got)
	}
	if got.Value != "3" || got.Calldata != "0x1234" || got.GasLimit != "21000" || got.MaxFeePerGas != "100" || got.MaxPriorityFeePerGas != "100" {
		t.Fatalf("transaction fields mismatch: %+v", got)
	}
}

func TestDecodeBroadcastSupportsTypedEthereumTransactions(t *testing.T) {
	chainID := new(big.Int).SetUint64(ArbitrumOneChainID)
	tests := []struct {
		name     string
		tx       *types.Transaction
		wantType string
	}{
		{
			name: "access list",
			tx: types.NewTx(&types.AccessListTx{
				ChainID:  chainID,
				Nonce:    8,
				GasPrice: big.NewInt(110),
				Gas:      50000,
				To:       testDestination(),
				Value:    big.NewInt(4),
				Data:     []byte{0x01},
			}),
			wantType: "0x01",
		},
		{
			name: "dynamic fee",
			tx: types.NewTx(&types.DynamicFeeTx{
				ChainID:   chainID,
				Nonce:     9,
				GasTipCap: big.NewInt(2),
				GasFeeCap: big.NewInt(120),
				Gas:       60000,
				To:        testDestination(),
				Value:     big.NewInt(5),
				Data:      []byte{0x02},
			}),
			wantType: "0x02",
		},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			signed := signTestTransaction(t, tt.tx, chainID)
			frame, report := decodeSingleFrame(t, signedTransactionMessage(t, signed))
			if len(report.Unsupported) != 0 || len(report.Malformed) != 0 {
				t.Fatalf("unexpected issues: %+v", report)
			}
			if len(frame.Transactions) != 1 || frame.Transactions[0].Type != tt.wantType {
				t.Fatalf("unexpected typed transaction: %+v", frame.Transactions)
			}
			if frame.Transactions[0].Hash != signed.Hash().Hex() || frame.Transactions[0].From != testSender(t).Hex() {
				t.Fatalf("typed transaction hash/sender mismatch: %+v", frame.Transactions[0])
			}
		})
	}
}

func TestDecodeBroadcastPayloadType03BatchContainsMultipleTransactions(t *testing.T) {
	chainID := new(big.Int).SetUint64(ArbitrumOneChainID)
	first := signTestTransaction(t, types.NewTx(&types.LegacyTx{
		Nonce: 1, GasPrice: big.NewInt(100), Gas: 21000, To: testDestination(), Value: big.NewInt(1),
	}), chainID)
	second := signTestTransaction(t, types.NewTx(&types.DynamicFeeTx{
		ChainID: chainID, Nonce: 2, GasTipCap: big.NewInt(1), GasFeeCap: big.NewInt(100), Gas: 22000, To: testDestination(), Value: big.NewInt(2),
	}), chainID)
	batch := l2BatchMessage(signedTransactionMessage(t, first), signedTransactionMessage(t, second))
	if batch[0] != L2MessageKindBatch {
		t.Fatalf("expected payload type 0x03, got 0x%02x", batch[0])
	}

	frame, report := decodeSingleFrame(t, batch)
	if len(report.Unsupported) != 0 || len(report.Malformed) != 0 {
		t.Fatalf("unexpected batch issues: %+v", report)
	}
	if len(frame.Transactions) != 2 {
		t.Fatalf("expected two transactions in one feed sequence, got %d", len(frame.Transactions))
	}
	if frame.Transactions[0].Hash != first.Hash().Hex() || frame.Transactions[1].Hash != second.Hash().Hex() {
		t.Fatalf("batch transaction order/hash mismatch: %+v", frame.Transactions)
	}
	if frame.Sequence != 460530858 {
		t.Fatalf("transactions lost shared feed sequence: %d", frame.Sequence)
	}
}

func TestDecodeBroadcastBatchPreservesSupportedSiblingsAroundUnsupportedItem(t *testing.T) {
	chainID := new(big.Int).SetUint64(ArbitrumOneChainID)
	first := signTestTransaction(t, types.NewTx(&types.LegacyTx{
		Nonce: 1, GasPrice: big.NewInt(100), Gas: 21000, To: testDestination(), Value: big.NewInt(1),
	}), chainID)
	second := signTestTransaction(t, types.NewTx(&types.LegacyTx{
		Nonce: 2, GasPrice: big.NewInt(100), Gas: 21000, To: testDestination(), Value: big.NewInt(2),
	}), chainID)
	batch := l2BatchMessage(
		signedTransactionMessage(t, first),
		[]byte{0x7f},
		signedTransactionMessage(t, second),
	)

	frame, report := decodeSingleFrame(t, batch)
	if len(frame.Transactions) != 2 {
		t.Fatalf("supported siblings were discarded: %+v", frame)
	}
	if len(report.Unsupported) != 1 || !strings.Contains(report.Unsupported[0], "batch item 1") || !strings.Contains(report.Unsupported[0], "0x7f") {
		t.Fatalf("unsupported child was not observable: %+v", report)
	}
	if len(report.Malformed) != 0 {
		t.Fatalf("unsupported child was misclassified as malformed: %+v", report)
	}
}

func TestDecodeBroadcastRejectsMalformedBatchFraming(t *testing.T) {
	tests := []struct {
		name    string
		payload []byte
		want    string
	}{
		{name: "truncated length", payload: []byte{L2MessageKindBatch, 0, 0, 0}, want: "truncated uint64"},
		{name: "truncated item", payload: batchWithDeclaredLength(10, []byte{L2MessageKindSignedTx}), want: "exceeds remaining"},
		{name: "oversized item", payload: batchWithDeclaredLength(MaxL2MessageSize+1, nil), want: "exceeds 262144"},
		{name: "empty item", payload: batchWithDeclaredLength(0, nil), want: "empty batch item"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			frame, report := decodeSingleFrame(t, tt.payload)
			if len(frame.Transactions) != 0 || len(report.Malformed) != 1 || !strings.Contains(report.Malformed[0], tt.want) {
				t.Fatalf("malformed batch classification mismatch: frame=%+v report=%+v", frame, report)
			}
			if len(report.Unsupported) != 0 {
				t.Fatalf("malformed batch counted as unsupported: %+v", report)
			}
		})
	}
}

func TestDecodeBroadcastRejectsInvalidSignedTransaction(t *testing.T) {
	frame, report := decodeSingleFrame(t, []byte{L2MessageKindSignedTx, 0xc0})
	if len(frame.Transactions) != 0 || len(report.Malformed) != 1 {
		t.Fatalf("invalid signed transaction was not rejected: frame=%+v report=%+v", frame, report)
	}
	if len(report.Unsupported) != 0 {
		t.Fatalf("invalid supported format counted as unsupported: %+v", report)
	}
}

func TestDecodeBroadcastRejectsSignedTransactionForWrongChain(t *testing.T) {
	signed := signTestTransaction(t, types.NewTx(&types.DynamicFeeTx{
		ChainID: big.NewInt(1), Nonce: 1, GasTipCap: big.NewInt(1), GasFeeCap: big.NewInt(100), Gas: 21000, To: testDestination(), Value: big.NewInt(1),
	}), big.NewInt(1))
	frame, report := decodeSingleFrame(t, signedTransactionMessage(t, signed))
	if len(frame.Transactions) != 0 || len(report.Malformed) != 1 || !strings.Contains(report.Malformed[0], "chain id") {
		t.Fatalf("wrong-chain transaction was not rejected: frame=%+v report=%+v", frame, report)
	}
}

func TestDecodeBroadcastAcceptsEmptyBatchWithoutTransactions(t *testing.T) {
	frame, report := decodeSingleFrame(t, []byte{L2MessageKindBatch})
	if len(frame.Transactions) != 0 || len(report.Malformed) != 0 || len(report.Unsupported) != 0 {
		t.Fatalf("empty batch should be a supported empty message: frame=%+v report=%+v", frame, report)
	}
	if len(report.Ignored) != 1 || !strings.Contains(report.Ignored[0], "empty") {
		t.Fatalf("empty batch should remain observable: %+v", report)
	}
}

func TestDecodeBroadcastEnforcesBatchRecursionLimit(t *testing.T) {
	signed := signTestTransaction(t, types.NewTx(&types.LegacyTx{
		Nonce: 1, GasPrice: big.NewInt(100), Gas: 21000, To: testDestination(), Value: big.NewInt(1),
	}), new(big.Int).SetUint64(ArbitrumOneChainID))
	nested := signedTransactionMessage(t, signed)
	for range MaxL2BatchDepth + 1 {
		nested = l2BatchMessage(nested)
	}
	frame, report := decodeSingleFrame(t, nested)
	if len(frame.Transactions) != 0 || len(report.Malformed) != 1 || !strings.Contains(report.Malformed[0], "max depth") {
		t.Fatalf("batch depth limit was not enforced: frame=%+v report=%+v", frame, report)
	}
}

func TestDecodeBroadcastContextPropagatesCancellation(t *testing.T) {
	ctx, cancel := context.WithCancel(context.Background())
	cancel()
	_, _, err := DecodeBroadcastContext(ctx, []byte(`{"version":1}`))
	if !errors.Is(err, context.Canceled) {
		t.Fatalf("expected context cancellation, got %v", err)
	}
}

func TestDecodeL2MessageRejectsOversizedPayload(t *testing.T) {
	result, err := decodeL2Message(context.Background(), make([]byte, MaxL2MessageSize+1), 0)
	if err != nil {
		t.Fatal(err)
	}
	if len(result.transactions) != 0 || len(result.malformed) != 1 || !strings.Contains(result.malformed[0], "exceeds") {
		t.Fatalf("oversized L2 message was not rejected: %+v", result)
	}
}

func TestDecodeBroadcastRejectsUnsupportedVersion(t *testing.T) {
	_, _, err := DecodeBroadcast([]byte(`{"version":2}`))
	if err == nil || !strings.Contains(err.Error(), "unsupported Nitro broadcast version") {
		t.Fatalf("expected unsupported version error, got %v", err)
	}
}

func TestDecodeBroadcastIgnoresKnownNonTransactionMessageKind(t *testing.T) {
	raw := []byte(`{"version":1,"messages":[{"sequenceNumber":1,"message":{"message":{"header":{"kind":6,"timestamp":1700000000},"l2Msg":""}}}]}`)
	frames, report, err := DecodeBroadcast(raw)
	if err != nil {
		t.Fatal(err)
	}
	if len(frames) != 1 || len(frames[0].Transactions) != 0 || len(frames[0].Ignored) != 1 || len(frames[0].Unsupported) != 0 || len(frames[0].Malformed) != 0 {
		t.Fatalf("unexpected ignored frame: frames=%+v report=%+v", frames, report)
	}
}

func TestDecodeBroadcastReportsUnsupportedTransactionLikeMessageKind(t *testing.T) {
	raw := []byte(`{"version":1,"messages":[{"sequenceNumber":1,"message":{"message":{"header":{"kind":9,"timestamp":1700000000},"l2Msg":""}}}]}`)
	frames, report, err := DecodeBroadcast(raw)
	if err != nil {
		t.Fatal(err)
	}
	if len(frames) != 1 || len(frames[0].Transactions) != 0 || len(report.Unsupported) != 1 || len(report.Malformed) != 0 {
		t.Fatalf("unexpected unsupported frame: frames=%+v report=%+v", frames, report)
	}
}

func decodeSingleFrame(t *testing.T, l2Message []byte) (Frame, DecodeReport) {
	t.Helper()
	raw, err := json.Marshal(BroadcastMessage{
		Version: BroadcastVersion,
		Messages: []*BroadcastFeedMessage{
			{
				SequenceNumber: 460530858,
				Message: MessageWithMetadata{
					Message: &L1IncomingMessage{
						Header: &L1IncomingMessageHeader{
							Kind:      L1MessageTypeL2Message,
							Timestamp: 1700000000,
						},
						L2msg: l2Message,
					},
				},
			},
		},
	})
	if err != nil {
		t.Fatal(err)
	}
	frames, report, err := DecodeBroadcast(raw)
	if err != nil {
		t.Fatalf("decode broadcast: %v", err)
	}
	if len(frames) != 1 {
		t.Fatalf("expected one frame, got %d", len(frames))
	}
	return frames[0], report
}

func signedTransactionMessage(t *testing.T, tx *types.Transaction) []byte {
	t.Helper()
	encoded, err := tx.MarshalBinary()
	if err != nil {
		t.Fatal(err)
	}
	return append([]byte{L2MessageKindSignedTx}, encoded...)
}

func l2BatchMessage(messages ...[]byte) []byte {
	batch := []byte{L2MessageKindBatch}
	var length [8]byte
	for _, message := range messages {
		binary.BigEndian.PutUint64(length[:], uint64(len(message)))
		batch = append(batch, length[:]...)
		batch = append(batch, message...)
	}
	return batch
}

func batchWithDeclaredLength(length uint64, payload []byte) []byte {
	batch := []byte{L2MessageKindBatch}
	var encoded [8]byte
	binary.BigEndian.PutUint64(encoded[:], length)
	batch = append(batch, encoded[:]...)
	return append(batch, payload...)
}

func signTestTransaction(t *testing.T, tx *types.Transaction, chainID *big.Int) *types.Transaction {
	t.Helper()
	keyMaterial := make([]byte, 32)
	keyMaterial[len(keyMaterial)-1] = 1
	key, err := crypto.ToECDSA(keyMaterial)
	if err != nil {
		t.Fatal(err)
	}
	signed, err := types.SignTx(tx, types.LatestSignerForChainID(chainID), key)
	if err != nil {
		t.Fatal(err)
	}
	return signed
}

func testSender(t *testing.T) common.Address {
	t.Helper()
	keyMaterial := make([]byte, 32)
	keyMaterial[len(keyMaterial)-1] = 1
	key, err := crypto.ToECDSA(keyMaterial)
	if err != nil {
		t.Fatal(err)
	}
	return crypto.PubkeyToAddress(key.PublicKey)
}

func testDestination() *common.Address {
	address := common.HexToAddress("0x2222222222222222222222222222222222222222")
	return &address
}

func byteRange(count int) []byte {
	out := make([]byte, count)
	for i := range out {
		out[i] = byte(i)
	}
	return out
}
