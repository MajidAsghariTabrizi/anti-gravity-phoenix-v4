package normalizer

import (
	"testing"
	"time"
)

func TestNormalizePreservesSourceFeedOrderPosition(t *testing.T) {
	tx, err := Normalize(7, 1_700_000_000_000, RelayTx{
		Hash:                    "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
		SourceFeedOrderPosition: 3,
		Type:                    "0x02",
		ChainID:                 42161,
		From:                    "0x1111111111111111111111111111111111111111",
		To:                      "0x2222222222222222222222222222222222222222",
		Nonce:                   1,
		Value:                   "0",
		Calldata:                "0x1234",
		GasLimit:                "21000",
		MaxFeePerGas:            "100",
		MaxPriorityFeePerGas:    "1",
		RawTx:                   "0x0102",
	}, time.Unix(1_700_000_000, 0))
	if err != nil {
		t.Fatal(err)
	}
	if tx.SourceFeedOrderPosition != 3 {
		t.Fatalf("source order position was lost: got %d", tx.SourceFeedOrderPosition)
	}
}
