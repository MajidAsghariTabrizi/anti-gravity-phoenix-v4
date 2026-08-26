package hunter

import (
	"context"
	"fmt"
	"math/big"
	"strings"
	"testing"
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
)

func newAuctionPriorityTestScreener(now time.Time) *Screener {
	return &Screener{
		config: Config{
			MaximumGasLimit: 100, MaximumFeePerGasWei: "10", MaximumPriorityFeeWei: "10",
			MaximumAtlasBidWei: "500", RetainedProfitFloorWei: "100",
			SignalSink: &recordingSignalSink{},
		},
		state: State{Schema: StateSchema, Counts: map[string]uint64{}, LastBlockNumber: 100},
		now:   func() time.Time { return now },
	}
}

func auctionRecord(id, asset string, observedAt time.Time) *observer.LedgerRecord {
	return &observer.LedgerRecord{
		RelevantAaveAuction: true, ChainID: 42161, AuctionID: id,
		AuctionDeadlineBlock: "3000", SolverGasLimit: 20, OracleGasPriceWei: "10",
		NotificationSHA256: strings.Repeat("f", 64), ObservedAt: observedAt,
		OracleUpdate: &observer.OracleUpdate{Asset: &asset},
	}
}

// Mission §3.1 fix 2: SVR symbols must resolve to reserve addresses through
// the verified alias table so borrower-path attachment works at all.
func TestPendingAuctionForResolvesVerifiedSymbolAlias(t *testing.T) {
	now := time.Date(2026, 8, 14, 0, 0, 0, 0, time.UTC)
	screener := newAuctionPriorityTestScreener(now)
	if err := screener.HandleAtlasAuction(context.Background(), auctionRecord("sym-1", "eth", now)); err != nil {
		t.Fatal(err)
	}
	record := screener.pendingAuctionForReserves(wethAddress, nativeUSDCAddress)
	if record == nil || record.AuctionID != "sym-1" {
		t.Fatalf("symbol-form auction did not resolve to WETH reserve lookup: %+v", record)
	}
	unrelated := screener.pendingAuctionForReserves(arbAddress, "")
	if unrelated != nil {
		t.Fatalf("WETH-symbol auction matched an unrelated asset: %+v", unrelated)
	}
}

func TestPendingAuctionReturnsOldestAcrossSameAsset(t *testing.T) {
	now := time.Date(2026, 8, 14, 1, 0, 0, 0, time.UTC)
	later := now.Add(2 * time.Second)
	screener := newAuctionPriorityTestScreener(now)
	if err := screener.HandleAtlasAuction(context.Background(), auctionRecord("old-1", "eth", now)); err != nil {
		t.Fatal(err)
	}
	if err := screener.HandleAtlasAuction(context.Background(), auctionRecord("new-2", "eth", later)); err != nil {
		t.Fatal(err)
	}
	record := screener.pendingAuctionForReserves(wethAddress, "")
	if record == nil || record.AuctionID != "old-1" {
		t.Fatalf("attachment did not pick the OLDEST pending auction: %+v", record)
	}
}

func TestQueueAuctionTriggerDeduplicatesAndBounds(t *testing.T) {
	now := time.Date(2026, 8, 14, 2, 0, 0, 0, time.UTC)
	screener := newAuctionPriorityTestScreener(now)
	screener.mu.Lock()
	for i := 0; i < maximumAuctionTriggerQueue+4; i++ {
		screener.queueAuctionTriggerLocked(fmt.Sprintf("asset-%d", i))
	}
	// Duplicate signal for an already-queued asset is dropped, not enqueued.
	before := len(screener.pendingAuctionTriggerAssets)
	screener.queueAuctionTriggerLocked(fmt.Sprintf("asset-%d", maximumAuctionTriggerQueue-1))
	after := len(screener.pendingAuctionTriggerAssets)
	drops := screener.state.Counts[auctionTriggerDroppedTotalKey]
	screener.mu.Unlock()
	if after != before {
		t.Fatalf("duplicate trigger was enqueued: %d -> %d", before, after)
	}
	if drops == 0 {
		t.Fatalf("drop counter never incremented")
	}
	if screener.pendingAuctionTriggerCount() > maximumAuctionTriggerQueue {
		t.Fatalf("trigger queue exceeded its bound")
	}
}

func TestIndexReserveEvidenceTracksExposureAndEvictsOldest(t *testing.T) {
	now := time.Date(2026, 8, 14, 3, 0, 0, 0, time.UTC)
	screener := newAuctionPriorityTestScreener(now)
	first := "0x" + strings.Repeat("11", 20)
	for index := 0; index < maximumReserveBorrowerIndex+16; index++ {
		borrower := fmt.Sprintf("0x%04x", index)
		screener.indexReserveEvidence(borrower, uint64(100+index), "0x"+strings.Repeat("a", 64),
			account{Borrower: borrower, TotalDebtBase: "1000000000", HealthFactorWAD: "900000000000000000"},
			[]exactReserve{{Asset: wethAddress, CurrentVariableDebt: "5000000000"}})
	}
	screener.mu.Lock()
	count := screener.reserveIndexEntryCountLocked()
	wethIndex := len(screener.reserveBorrowersByAsset[wethAddress])
	screener.mu.Unlock()
	if count != maximumReserveBorrowerIndex || wethIndex != maximumReserveBorrowerIndex {
		t.Fatalf("reserve index cap violated: total=%d weth=%d", count, wethIndex)
	}
	// A borrower whose exposure disappears leaves the index.
	screener.indexReserveEvidence(first, 9999, "0x"+strings.Repeat("b", 64),
		account{Borrower: first},
		[]exactReserve{{Asset: wethAddress, CurrentVariableDebt: "0"}})
	screener.mu.Lock()
	_, present := screener.reserveBorrowersByAsset[wethAddress][first]
	screener.mu.Unlock()
	if present {
		t.Fatalf("borrower without exposure stayed indexed")
	}
}

func TestAuctionPriorityBorrowersRankAndBoundPerAsset(t *testing.T) {
	now := time.Date(2026, 8, 14, 4, 0, 0, 0, time.UTC)
	screener := newAuctionPriorityTestScreener(now)
	for index := 0; index < 5; index++ {
		borrower := fmt.Sprintf("0xb%d", index)
		hf := fmt.Sprintf("%d000000000000000", 90-index) // descending health factor
		screener.indexReserveEvidence(borrower, uint64(200+index), "0x"+strings.Repeat("c", 64),
			account{Borrower: borrower, TotalDebtBase: "1000000000", HealthFactorWAD: hf},
			[]exactReserve{
				{Asset: arbAddress, CurrentVariableDebt: big.NewInt(1).String()},
				{Asset: wethAddress, CurrentATokenBalance: "7000000000", UsageAsCollateralEnabled: true},
			})
	}
	selected := screener.auctionPriorityBorrowers([]string{"arb"})
	if len(selected) != maximumAuctionTriggerCandidatesPerAsset {
		t.Fatalf("selection ignored the per-asset candidate bound: %+v", selected)
	}
	// liquidationPriorityLess orders by rank then ascending HF: the two
	// lowest-HF borrowers must come first among equal ranks.
	firstHF := firstHFOf(screener, selected[0])
	rankFirst := liquidationPriorityRank("", firstHF)
	if rankFirst > 1 {
		t.Fatalf("selected borrower ranked worse than urgent: %s rank=%d", selected[0], rankFirst)
	}
	// Symbol form resolves through the alias table to the same index.
	bySymbol := screener.auctionPriorityBorrowers([]string{"weth"})
	if len(bySymbol) == 0 {
		t.Fatalf("symbol-form asset did not match the address-keyed index")
	}
}

func TestRecentExactSkipSuppressesHotBorrowers(t *testing.T) {
	now := time.Date(2026, 8, 14, 5, 0, 0, 0, time.UTC)
	screener := newAuctionPriorityTestScreener(now)
	borrower := "0xhot"
	screener.lastExactAt = map[string]time.Time{}
	screener.indexReserveEvidence(borrower, 250, "0x"+strings.Repeat("d", 64),
		account{Borrower: borrower, TotalDebtBase: "1000000000", HealthFactorWAD: "800000000000000000"},
		[]exactReserve{{Asset: wethAddress, CurrentVariableDebt: "9000000000"}})
	screener.lastExactAt[borrower] = now.Add(-auctionTriggerRecentExactSkip / 2)
	if got := screener.auctionPriorityBorrowers([]string{"eth"}); len(got) != 0 {
		t.Fatalf("recently served borrower was re-admitted by the trigger: %+v", got)
	}
}

func firstHFOf(screener *Screener, borrower string) string {
	screener.mu.Lock()
	defer screener.mu.Unlock()
	entry, ok := screener.reserveBorrowersByAsset[arbAddress][borrower]
	if !ok {
		return ""
	}
	return entry.HealthFactorWAD
}
