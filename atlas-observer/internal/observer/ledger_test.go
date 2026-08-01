package observer

import (
	"encoding/json"
	"os"
	"path/filepath"
	"testing"
	"time"
)

func TestLedgerDeduplicatesAndPersistsBoundedState(t *testing.T) {
	dir := t.TempDir()
	start := time.Unix(1_700_000_000, 0).UTC()
	ledger, err := OpenLedger(dir, start, 500, 72*time.Hour)
	if err != nil {
		t.Fatal(err)
	}
	record, err := DecodeAndValidateNotification(validNotification(t, "0x4c76F02E484e8ce9B6C2358CF9624BabC5531E9e"), start.Add(time.Second))
	if err != nil {
		t.Fatal(err)
	}
	added, err := ledger.Append(record)
	if err != nil || !added {
		t.Fatalf("first append failed: added=%t err=%v", added, err)
	}
	added, err = ledger.Append(record)
	if err != nil || added {
		t.Fatalf("duplicate append contract failed: added=%t err=%v", added, err)
	}
	state := ledger.Snapshot(start.Add(2 * time.Second))
	if state.UniqueAuctionCount != 1 || state.RelevantAaveCount != 1 || state.DuplicateCount != 1 || state.PerFeed["LINK"] != 1 {
		t.Fatalf("unexpected state: %#v", state)
	}
	reopened, err := OpenLedger(dir, start.Add(time.Minute), 500, 72*time.Hour)
	if err != nil {
		t.Fatal(err)
	}
	if reopened.Snapshot(start.Add(time.Minute)).UniqueAuctionCount != 1 {
		t.Fatal("durable auction identity was not recovered")
	}
	data, err := os.ReadFile(filepath.Join(dir, "auctions.ndjson"))
	if err != nil {
		t.Fatal(err)
	}
	var persisted LedgerRecord
	if err := json.Unmarshal(data, &persisted); err != nil {
		t.Fatal(err)
	}
	if persisted.AuctionID != validAuctionID {
		t.Fatalf("unexpected persisted auction: %s", persisted.AuctionID)
	}
}

func TestLedgerStopsAtFirstBound(t *testing.T) {
	start := time.Unix(1_700_000_000, 0).UTC()
	ledger, err := OpenLedger(t.TempDir(), start, 1, time.Hour)
	if err != nil {
		t.Fatal(err)
	}
	record, err := DecodeAndValidateNotification(validNotification(t, "0xc1720A8240Dbd992d95D6c865A15e490901879B1"), start.Add(time.Second))
	if err != nil {
		t.Fatal(err)
	}
	if _, err := ledger.Append(record); err != nil {
		t.Fatal(err)
	}
	state := ledger.Snapshot(start.Add(time.Second))
	if !state.Completed || state.CompletionReason != "maximum_auction_count" {
		t.Fatalf("auction bound did not complete ledger: %#v", state)
	}

	durationLedger, err := OpenLedger(t.TempDir(), start, 500, time.Hour)
	if err != nil {
		t.Fatal(err)
	}
	complete, err := durationLedger.Complete(start.Add(time.Hour))
	if err != nil || !complete {
		t.Fatalf("duration bound did not complete ledger: complete=%t err=%v", complete, err)
	}
	if durationLedger.Snapshot(start.Add(time.Hour)).CompletionReason != "maximum_observation_duration" {
		t.Fatal("wrong duration completion reason")
	}
}

func TestLedgerRejectsRelativeDirectory(t *testing.T) {
	if _, err := OpenLedger("relative-ledger", time.Now(), 500, 72*time.Hour); err == nil {
		t.Fatal("relative ledger directory was accepted")
	}
}

func TestOtherChainFilterIsTrackedSeparately(t *testing.T) {
	start := time.Unix(1_700_000_000, 0).UTC()
	ledger, err := OpenLedger(t.TempDir(), start, 500, 72*time.Hour)
	if err != nil {
		t.Fatal(err)
	}
	if err := ledger.RecordFilteredOtherChain(start.Add(time.Second)); err != nil {
		t.Fatal(err)
	}
	state := ledger.Snapshot(start.Add(time.Second))
	if state.FilteredOtherChainCount != 1 || state.InvalidCount != 0 {
		t.Fatalf("other chain polluted invariant failures: %#v", state)
	}
}

func TestSubscriptionIdentityIsDurable(t *testing.T) {
	start := time.Unix(1_700_000_000, 0).UTC()
	ledger, err := OpenLedger(t.TempDir(), start, 500, 72*time.Hour)
	if err != nil {
		t.Fatal(err)
	}
	if err := ledger.RecordSubscription(start.Add(time.Second), "subscription-1"); err != nil {
		t.Fatal(err)
	}
	if ledger.Snapshot(start.Add(time.Second)).LastSubscriptionID != "subscription-1" {
		t.Fatal("subscription identity was not persisted")
	}
}
