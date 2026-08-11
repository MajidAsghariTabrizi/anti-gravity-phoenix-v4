package observer

import (
	"encoding/json"
	"strings"
	"testing"
	"time"
)

func TestSubscriptionPayloadIsReadOnlyAndCanonical(t *testing.T) {
	payload := SubscriptionPayload()
	if payload.JSONRPC != "2.0" || payload.ID != 1 || payload.Method != "solver_subscribe" {
		t.Fatalf("unexpected subscription request: %#v", payload)
	}
	if len(payload.Params) != 1 || payload.Params[0] != "userOperations" {
		t.Fatalf("unexpected subscription topic: %#v", payload.Params)
	}
	encoded, err := json.Marshal(payload)
	if err != nil {
		t.Fatal(err)
	}
	var decoded map[string]any
	if err := json.Unmarshal(encoded, &decoded); err != nil {
		t.Fatal(err)
	}
	if len(decoded) != 4 {
		t.Fatalf("subscription request contains unexpected fields: %s", encoded)
	}
}

func TestExactDuplicateEvidenceCanRepairSinkWithoutAdmittingChangedPayload(t *testing.T) {
	ledger, err := OpenLedger(t.TempDir(), time.Now().UTC(), 0, 0)
	if err != nil {
		t.Fatal(err)
	}
	record := &LedgerRecord{
		AuctionID: "auction-repair", NotificationSHA256: strings.Repeat("a", 64),
		ObservedAt: time.Now().UTC(), ChainID: 42161,
	}
	if added, err := ledger.Append(record); err != nil || !added {
		t.Fatalf("initial ledger append failed: added=%t err=%v", added, err)
	}
	if !ledger.hasExactEvidence(record.AuctionID, record.NotificationSHA256) {
		t.Fatal("exact duplicate was not eligible for idempotent sink repair")
	}
	if ledger.hasExactEvidence(record.AuctionID, strings.Repeat("b", 64)) {
		t.Fatal("changed duplicate payload crossed the sink repair boundary")
	}
}
