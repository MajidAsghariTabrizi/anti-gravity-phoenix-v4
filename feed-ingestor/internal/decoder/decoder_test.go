package decoder

import (
	"testing"
	"time"
)

var validFrameOne = []byte(`{"sequence":1,"timestamp_unix_ms":1700000000000,"transactions":[{"hash":"0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","type":"0x2","chain_id":42161,"from":"0x1111111111111111111111111111111111111111","to":"0x2222222222222222222222222222222222222222","nonce":7,"value":"0","calldata":"0x1234","gas_limit":"21000","max_fee_per_gas":"100","max_priority_fee_per_gas":"1","raw_tx":"0x0102"}]}`)

func fixedNow() time.Time {
	return time.Unix(1700000000, 123)
}

func TestDecodeFrameNormalizesTransaction(t *testing.T) {
	d := NewOrderedDecoder(fixedNow)
	result, err := d.DecodeJSONFrame(validFrameOne)
	if err != nil {
		t.Fatalf("decode frame: %v", err)
	}
	if len(result.Transactions) != 1 {
		t.Fatalf("expected one transaction, got %d", len(result.Transactions))
	}
	tx := result.Transactions[0]
	if tx.Sequence != 1 {
		t.Fatalf("sequence mismatch: %d", tx.Sequence)
	}
	if tx.ChainID != 42161 {
		t.Fatalf("chain mismatch: %d", tx.ChainID)
	}
	if tx.TxHash != "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa" {
		t.Fatalf("hash mismatch: %s", tx.TxHash)
	}
	if len(tx.RawTx) != 2 {
		t.Fatalf("raw tx length mismatch: %d", len(tx.RawTx))
	}
}

func TestDuplicateSequenceIsReportedAndNotReplayed(t *testing.T) {
	d := NewOrderedDecoder(fixedNow)
	if _, err := d.DecodeJSONFrame(validFrameOne); err != nil {
		t.Fatal(err)
	}
	result, err := d.DecodeJSONFrame(validFrameOne)
	if err != nil {
		t.Fatal(err)
	}
	if !result.Duplicate {
		t.Fatal("expected duplicate")
	}
	if len(result.Transactions) != 0 {
		t.Fatal("duplicate should not return transactions")
	}
}

func TestSequenceGapIsReported(t *testing.T) {
	d := NewOrderedDecoder(fixedNow)
	if _, err := d.DecodeJSONFrame(validFrameOne); err != nil {
		t.Fatal(err)
	}
	frameThree := []byte(`{"sequence":3,"timestamp_unix_ms":1700000000001,"transactions":[]}`)
	result, err := d.DecodeJSONFrame(frameThree)
	if err != nil {
		t.Fatal(err)
	}
	if !result.Gap {
		t.Fatal("expected gap")
	}
	if result.GapFrom != 2 || result.GapTo != 2 {
		t.Fatalf("unexpected gap range %d..%d", result.GapFrom, result.GapTo)
	}
}

func TestMalformedFrameFails(t *testing.T) {
	d := NewOrderedDecoder(fixedNow)
	if _, err := d.DecodeJSONFrame([]byte(`{"sequence":1,"transactions":[{"chain_id":1}]}`)); err == nil {
		t.Fatal("expected malformed frame error")
	}
}
