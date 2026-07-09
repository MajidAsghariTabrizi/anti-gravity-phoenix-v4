package nitro

import (
	"encoding/base64"
	"encoding/json"
	"strings"
	"testing"
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

func TestDecodeBroadcastExtractsArbitrumUnsignedTx(t *testing.T) {
	rawTx := sampleArbitrumUnsignedTx()
	payload := map[string]any{
		"version": BroadcastVersion,
		"messages": []any{
			map[string]any{
				"sequenceNumber": float64(1),
				"message": map[string]any{
					"message": map[string]any{
						"header": map[string]any{
							"kind":      float64(L1MessageTypeL2Message),
							"timestamp": float64(1700000000),
						},
						"l2Msg": base64.StdEncoding.EncodeToString(rawTx),
					},
					"delayedMessagesRead": float64(0),
				},
			},
		},
	}
	raw, err := json.Marshal(payload)
	if err != nil {
		t.Fatal(err)
	}

	frames, report, err := DecodeBroadcast(raw)
	if err != nil {
		t.Fatalf("decode broadcast: %v", err)
	}
	if len(report.Unsupported) != 0 {
		t.Fatalf("unexpected unsupported report: %+v", report.Unsupported)
	}
	if len(frames) != 1 {
		t.Fatalf("expected one frame, got %d", len(frames))
	}
	frame := frames[0]
	if frame.Sequence != 1 || frame.TimestampUnixMS != 1700000000000 {
		t.Fatalf("unexpected frame metadata: %+v", frame)
	}
	if len(frame.Transactions) != 1 {
		t.Fatalf("expected one transaction, got %d", len(frame.Transactions))
	}
	tx := frame.Transactions[0]
	if tx.Type != "0x65" || tx.ChainID != 42161 || tx.Nonce != 7 {
		t.Fatalf("unexpected tx identity: %+v", tx)
	}
	if tx.From != "0x1111111111111111111111111111111111111111" {
		t.Fatalf("unexpected from: %s", tx.From)
	}
	if tx.To != "0x2222222222222222222222222222222222222222" {
		t.Fatalf("unexpected to: %s", tx.To)
	}
	if tx.Calldata != "0x1234" || tx.Value != "0" || tx.GasLimit != "21000" || tx.MaxFeePerGas != "100" {
		t.Fatalf("unexpected tx economics: %+v", tx)
	}
	if len(tx.Hash) != 66 || !strings.HasPrefix(tx.Hash, "0x") {
		t.Fatalf("unexpected tx hash: %s", tx.Hash)
	}
	if tx.RawTx != "0x"+hexLower(rawTx) {
		t.Fatalf("unexpected raw tx: %s", tx.RawTx)
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
	if len(frames) != 1 || len(frames[0].Transactions) != 0 || len(frames[0].Ignored) != 1 || len(frames[0].Unsupported) != 0 {
		t.Fatalf("unexpected ignored frame: frames=%+v report=%+v", frames, report)
	}
	if len(report.Ignored) != 1 || len(report.Unsupported) != 0 {
		t.Fatalf("expected report to include ignored reason only: %+v", report)
	}
}

func TestDecodeBroadcastReportsUnsupportedTransactionLikeMessageKind(t *testing.T) {
	raw := []byte(`{"version":1,"messages":[{"sequenceNumber":1,"message":{"message":{"header":{"kind":9,"timestamp":1700000000},"l2Msg":""}}}]}`)
	frames, report, err := DecodeBroadcast(raw)
	if err != nil {
		t.Fatal(err)
	}
	if len(frames) != 1 || len(frames[0].Transactions) != 0 || len(frames[0].Unsupported) != 1 {
		t.Fatalf("unexpected unsupported frame: frames=%+v report=%+v", frames, report)
	}
	if len(report.Unsupported) != 1 {
		t.Fatalf("expected report to include unsupported reason: %+v", report)
	}
}

func TestDecodeBroadcastReportsUnsupportedPayloadType(t *testing.T) {
	payload := map[string]any{
		"version": BroadcastVersion,
		"messages": []any{
			map[string]any{
				"sequenceNumber": float64(1),
				"message": map[string]any{
					"message": map[string]any{
						"header": map[string]any{"kind": float64(L1MessageTypeL2Message)},
						"l2Msg":  base64.StdEncoding.EncodeToString([]byte{0x02}),
					},
				},
			},
		},
	}
	raw, err := json.Marshal(payload)
	if err != nil {
		t.Fatal(err)
	}
	frames, report, err := DecodeBroadcast(raw)
	if err != nil {
		t.Fatal(err)
	}
	if len(frames) != 1 || len(frames[0].Transactions) != 0 || len(frames[0].Unsupported) != 1 {
		t.Fatalf("unexpected unsupported payload frame: frames=%+v report=%+v", frames, report)
	}
	if !strings.Contains(frames[0].Unsupported[0], "unsupported L2 transaction payload type") {
		t.Fatalf("unexpected unsupported reason: %+v", frames[0].Unsupported)
	}
}

func sampleArbitrumUnsignedTx() []byte {
	raw := []byte{
		0x65, 0xf6,
		0x82, 0xa4, 0xb1,
		0x94,
	}
	raw = append(raw, repeatByte(0x11, 20)...)
	raw = append(raw,
		0x07,
		0x64,
		0x82, 0x52, 0x08,
		0x94,
	)
	raw = append(raw, repeatByte(0x22, 20)...)
	raw = append(raw,
		0x80,
		0x82, 0x12, 0x34,
	)
	return raw
}

func repeatByte(value byte, count int) []byte {
	out := make([]byte, count)
	for i := range out {
		out[i] = value
	}
	return out
}

func byteRange(count int) []byte {
	out := make([]byte, count)
	for i := range out {
		out[i] = byte(i)
	}
	return out
}
