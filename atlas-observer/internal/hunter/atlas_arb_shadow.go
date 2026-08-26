package hunter

import (
	"context"
	"math/big"
	"strings"
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
)

// Atlas ARB shadow instrumentation — the evidence-selected non-WETH shadow
// pair (Workstream C).
//
// Production evidence (2026-08-20): 11,056 of 34,097 Aave-relevant SVR
// auctions carry the ARB oracle asset (32.4% vs 35.3% WETH; next candidate
// BTC 25.3%). Every one of those auctions is currently classified
// atlas_weth_debt_required: the direct lane's reviewed unwind universe is
// WETH-pool-only, and no reviewed ARB unwind pool exists, so honest ARB bid
// economics cannot clear the retained-profit floor yet.
//
// This module instruments the pair instead of manufacturing economics. The
// gateway ALREADY returns per-reserve evidence for every borrower that ever
// receives an exact evaluation — including ARB variable debt, collateral
// composition and health factor — and that evidence is currently discarded
// after the route-reason decision. We index ARB-debt borrowers from that
// existing data (no new RPC source, no new gateway route) and attach ARB
// auctions to the best liquidatable ARB-debt borrower, recording shadow
// evidence with the terminal reason atlas_arb_unwind_unreviewed.
//
// Bid fields stay empty and shadow_bid_eligible stays false: evidence-only,
// fail-closed, and reversible without schema change. Pricing an ARB unwind
// requires a reviewed ARB pool identity, which is a separate authority
// decision outside this module (see docs/DEPENDENCIES.md).

const (
	// atlasShadowReasonArbUnwindUnreviewed classifies an ARB auction that
	// attached to an indexed ARB-debt borrower but cannot be priced: no
	// reviewed ARB unwind pool exists in the route universe.
	atlasShadowReasonArbUnwindUnreviewed = "atlas_arb_unwind_unreviewed"

	// arbAddress is the Arbitrum One Aave V3 ARB reserve.
	arbAddress = "0x912ce59144191c1204e64559fe8253a0e49e6548"

	// arbAuctionAssetSymbol is the oracle asset symbol SVR emits for ARB
	// auctions. Auctions may also carry the reserve address form; both are
	// matched.
	arbAuctionAssetSymbol = "arb"

	// maximumArbBorrowerIndex bounds the in-memory ARB-debt borrower index.
	// The index is evidence-only and rebuilt from future exact responses.
	maximumArbBorrowerIndex = 512
)

// arbBorrowerEntry is the per-borrower evidence captured from exact
// responses for ARB-debt borrowers. All economic fields remain integer
// strings; this module performs no pricing and no bid math.
type arbBorrowerEntry struct {
	Borrower        string
	ARBVariableDebt *big.Int
	CollateralAsset string
	CollateralWei   *big.Int
	CollateralOK    bool
	TotalDebtBase   string
	HealthFactorWAD string
	BlockNumber     uint64
	BlockHash       string
	UpdatedAt       time.Time
}

// isArbAuctionAsset reports whether an auction oracle asset string is the
// evidence-selected ARB pair (symbol or reserve address form).
func isArbAuctionAsset(asset *string) bool {
	if asset == nil {
		return false
	}
	normalized := strings.ToLower(strings.TrimSpace(*asset))
	return normalized == arbAuctionAssetSymbol || normalized == arbAddress
}

// indexArbDebtEvidence indexes the ARB-debt evidence present in one exact
// response. It removes the borrower's entry when they no longer carry ARB
// variable debt and caps the index at maximumArbBorrowerIndex by evicting
// the oldest entry. It never fails the exact evaluation: the index is
// evidence-only.
func (s *Screener) indexArbDebtEvidence(
	borrower string,
	blockNumber uint64,
	blockHash string,
	responseAccount account,
	reserves []exactReserve,
) {
	normalizedBorrower := strings.ToLower(borrower)
	// The exact response account is identity-bound to the requested borrower
	// elsewhere in the response contract; for the evidence-only index, use
	// its debt/health fields only when the identity matches, otherwise fall
	// back to the empty account (no ranking signal).
	if !strings.EqualFold(normalizedBorrower, responseAccount.Borrower) {
		responseAccount = account{}
	}
	var arbDebt *big.Int
	var collateralAsset string
	var collateralWei *big.Int
	collateralOK := false
	for _, reserve := range reserves {
		asset := strings.ToLower(reserve.Asset)
		if asset == arbAddress {
			if variable, ok := newBigUint(reserve.CurrentVariableDebt); ok && variable.Sign() > 0 {
				arbDebt = variable
			}
			continue
		}
		switch asset {
		case wethAddress, nativeUSDCAddress, usdcEAddress:
			balance, ok := newBigUint(reserve.CurrentATokenBalance)
			if !ok || balance.Sign() <= 0 || !reserve.UsageAsCollateralEnabled {
				continue
			}
			if !collateralOK {
				// Prefer WETH collateral over the stablecoins; first enabled
				// collateral wins.
				collateralAsset, collateralWei, collateralOK = asset, balance, true
			}
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.arbBorrowers == nil {
		s.arbBorrowers = make(map[string]arbBorrowerEntry)
	}
	if arbDebt == nil {
		delete(s.arbBorrowers, normalizedBorrower)
		return
	}
	s.arbBorrowers[normalizedBorrower] = arbBorrowerEntry{
		Borrower:        normalizedBorrower,
		ARBVariableDebt: arbDebt,
		CollateralAsset: collateralAsset,
		CollateralWei:   collateralWei,
		CollateralOK:    collateralOK,
		TotalDebtBase:   responseAccount.TotalDebtBase,
		HealthFactorWAD: responseAccount.HealthFactorWAD,
		BlockNumber:     blockNumber,
		BlockHash:       blockHash,
		UpdatedAt:       s.nowUTC(),
	}
	if len(s.arbBorrowers) > maximumArbBorrowerIndex {
		var oldestBorrower string
		var oldest time.Time
		for candidate, entry := range s.arbBorrowers {
			if oldestBorrower == "" || entry.UpdatedAt.Before(oldest) {
				oldestBorrower, oldest = candidate, entry.UpdatedAt
			}
		}
		delete(s.arbBorrowers, oldestBorrower)
	}
}

// bestArbBorrower returns the best attach target for an ARB auction: the
// most liquidation-prioritized ARB-debt borrower (liquidatable first, then
// urgent/watch by health factor, mirroring the direct lane's priority
// order). The index is evidence-only, so an empty result simply means the
// auction stays in the registry until expiry.
func (s *Screener) bestArbBorrower() (arbBorrowerEntry, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	best := arbBorrowerEntry{}
	found := false
	for _, entry := range s.arbBorrowers {
		if !found {
			best, found = entry, true
			continue
		}
		bestRank := liquidationPriorityRank(best.TotalDebtBase, best.HealthFactorWAD)
		entryRank := liquidationPriorityRank(entry.TotalDebtBase, entry.HealthFactorWAD)
		if entryRank < bestRank {
			best = entry
			continue
		}
		if entryRank > bestRank {
			continue
		}
		entryHF, entryHFOK := newBigUint(entry.HealthFactorWAD)
		bestHF, bestHFOK := newBigUint(best.HealthFactorWAD)
		if entryHFOK != bestHFOK {
			if entryHFOK {
				best = entry
			}
			continue
		}
		if entryHFOK && bestHFOK && entryHF.Cmp(bestHF) != 0 {
			if entryHF.Cmp(bestHF) < 0 {
				best = entry
			}
			continue
		}
		if entry.Borrower < best.Borrower {
			best = entry
		}
	}
	return best, found
}

// claimPendingArbAuction atomically marks the OLDEST pending ARB auction
// evaluated and returns it, or nil when none is pending. The registry is
// keyed by AuctionID with bounded FIFO per asset (mission §3.1); claiming
// before recording prevents double records from concurrent exact workers.
func (s *Screener) claimPendingArbAuction() *observer.LedgerRecord {
	s.mu.Lock()
	defer s.mu.Unlock()
	var oldest *recentAuction
	for _, entry := range s.recentAuctions {
		if entry.evaluated || entry.record == nil {
			continue
		}
		if entry.record.OracleUpdate == nil || entry.record.OracleUpdate.Asset == nil {
			continue
		}
		key := strings.ToLower(*entry.record.OracleUpdate.Asset)
		if key != arbAuctionAssetSymbol && key != arbAddress {
			continue
		}
		if oldest == nil || entry.record.ObservedAt.Before(oldest.record.ObservedAt) {
			oldest = entry
		}
	}
	if oldest == nil {
		return nil
	}
	oldest.evaluated = true
	return oldest.record
}

// arbAuctionShadowEvaluation builds the evidence-only shadow record for an
// ARB auction attached to an indexed ARB-debt borrower. Economics stay
// unknown (empty) and the row terminates with
// atlas_arb_unwind_unreviewed.
func arbAuctionShadowEvaluation(auction *observer.LedgerRecord, entry arbBorrowerEntry) atlasShadowEvaluation {
	evaluation := atlasShadowTerminal(auction, true, true, atlasShadowReasonArbUnwindUnreviewed)
	evaluation.Borrower = entry.Borrower
	evaluation.BlockNumber = entry.BlockNumber
	evaluation.BlockHash = entry.BlockHash
	return evaluation
}

// attachPendingArbAuction attaches a pending ARB auction to the best
// indexed ARB-debt borrower. It is invoked after fresh ARB-debt evidence is
// indexed; without a borrower or a pending auction it is a no-op.
func (s *Screener) attachPendingArbAuction(ctx context.Context) error {
	entry, ok := s.bestArbBorrower()
	if !ok {
		return nil
	}
	claimed := s.claimPendingArbAuction()
	if claimed == nil {
		return nil
	}
	return s.recordAtlasShadow(ctx, arbAuctionShadowEvaluation(claimed, entry))
}
