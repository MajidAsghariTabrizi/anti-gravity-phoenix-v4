package hunter

import (
	"context"
	"math/big"
	"sort"
	"strings"
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
)

// Auction-priority evaluation (mission §3.1).
//
// Root cause this module fixes (production evidence 2026-08-25,
// live_canary.atlas_auction_shadow n=26,497): auctions were stored ONE per
// oracle asset and were consumed only when a borrower-driven exact
// evaluation happened to match the asset. 92.8% of all shadow rows died as
// superseded_without_evaluation because a newer auction replaced its
// predecessor before any borrower screen matched, and attachment was
// additionally broken by an identity mismatch: the registry was keyed by
// the SVR oracle SYMBOL ("eth") while lookups passed Aave reserve ADDRESSES
// (0x82af…), which can never compare equal.
//
// Three independent fixes, all shadow-only:
//
//  1. Registry retention becomes per-auction-ID with bounded FIFO order per
//     asset (screen.go); supersede classification now fires only on bounded
//     eviction, never on ordinary arrival.
//  2. This module indexes, per Aave reserve asset, the borrowers whose
//     exact responses proved variable-debt or enabled-collateral exposure
//     (evidence the gateway already returned and the lane previously
//     discarded). No new RPC source, no new gateway route.
//  3. When a valid auction is stored, its asset is queued; the priority
//     window wakes and screens the best-indexed affected borrowers through
//     the UNCHANGED screen() path, so attachment, exact evidence, integer
//     economics, budget admission, fork permits, and provider circuit
//     behavior are byte-for-byte the production path.
//
// Safety invariants: this module never prices anything, never materializes
// a solver or execution request, never relaxes cooldown budgets below the
// configured admission rules, and never invents asset identities. Symbols
// resolve to addresses ONLY through the repo-verified alias table below;
// unresolved symbols leave their auctions registry-pending exactly as an
// unknown asset does today.

const (
	// maximumPendingAuctionsPerAsset bounds FIFO retention per oracle asset.
	// The SVR stream bursts (p95 arrival ≫ evaluation rate); beyond this cap
	// the OLDEST pending auction is evicted with the historical
	// superseded_without_evaluation classification.
	maximumPendingAuctionsPerAsset = 16

	// maximumReserveBorrowerIndex bounds the global by-asset borrower index.
	maximumReserveBorrowerIndex = 4096

	// maximumAuctionTriggerCandidatesPerAsset caps how many indexed borrowers
	// one queued auction asset may admit into a priority screen batch.
	maximumAuctionTriggerCandidatesPerAsset = 3

	// auctionTriggerRecentExactSkip skips borrowers exact-evaluated very
	// recently; it protects provider budget without reintroducing the
	// minute-scale cooldowns that starved auctions originally.
	auctionTriggerRecentExactSkip = 30 * time.Second

	// maximumAuctionTriggerQueue deduplicates queued assets; overflow drops
	// the QUEUED SIGNAL (never the auction itself, which stays registered).
	maximumAuctionTriggerQueue = 32

	// Auction-priority metric keys (hunter state Counts; exported via
	// MetricsText like every other counter).
	auctionTriggerEnqueuedTotalKey  = "atlas_auction_trigger_enqueued_total"
	auctionTriggerDroppedTotalKey   = "atlas_auction_trigger_dropped_total"
	auctionTriggerBatchesTotalKey   = "atlas_auction_trigger_screen_batches_total"
	auctionTriggerBorrowersTotalKey = "atlas_auction_trigger_borrowers_total"
	auctionTriggerNoIndexTotalKey   = "atlas_auction_trigger_no_index_total"
	reserveIndexEntriesTrackedKey   = "atlas_reserve_borrower_index_entries_total"
)

// reserveBorrowerEntry is per-(asset,borrower) exposure evidence captured
// verbatim from an exact response. All quantities stay integer strings; this
// module performs NO pricing and NO bid math.
type reserveBorrowerEntry struct {
	Borrower        string
	Asset           string // lowercase reserve address form, as received
	VariableDebtWei *big.Int
	CollateralWei   *big.Int
	TotalDebtBase   string
	HealthFactorWAD string
	BlockNumber     uint64
	BlockHash       string
	UpdatedAt       time.Time
}

// verifiedAuctionAssetAliases maps lowercased SVR oracle-asset symbols to
// Aave V3 Arbitrum reserve addresses that are ALREADY verified elsewhere in
// this repository. Sources (do not extend without a repo-verifiable source —
// AGENTS.md forbids guessing protocol addresses):
//   - WETH  docs/DEPENDENCIES.md L52, contracts/script/DeployPhoenixExecutor.s.sol L20
//   - ARB   atlas_arb_shadow.go arbAddress (Workstream C)
//   - USDC  nativeUSDCAddress / usdcEAddress (hunter constants, reviewed unwind universe)
//   - USDT  scripts/tests/test_atlas_liquidation_ground_truth.py L39 (production
//     ground-truth fixture: debt_asset of reconciled receipt-status-1 liquidations)
var verifiedAuctionAssetAliases = func() map[string]string {
	return map[string]string{
		"eth":   wethAddress,
		"weth":  wethAddress,
		"arb":   arbAddress,
		"usdc":  nativeUSDCAddress,
		"usdce": usdcEAddress,
		"usdt":  "0xfd086bc7cd5c481dcc9c85ebe478a1c0b69fcbb9",
	}
}()

// normalizeAuctionAsset lowercases and trims an SVR oracle-asset identifier.
func normalizeAuctionAsset(asset string) string {
	return strings.ToLower(strings.TrimSpace(asset))
}

// auctionAssetMatches reports whether a registry asset key refers to the
// given Aave reserve address, either directly (address-form auctions) or via
// the verified alias table (symbol-form auctions).
func auctionAssetMatches(registryAsset, reserveAddress string) bool {
	if registryAsset == "" || reserveAddress == "" {
		return false
	}
	if registryAsset == reserveAddress {
		return true
	}
	alias, ok := verifiedAuctionAssetAliases[registryAsset]
	return ok && alias == reserveAddress
}

// indexReserveEvidence records one borrower's per-reserve exposure evidence
// into the by-asset index. It mirrors indexArbDebtEvidence conventions: it
// never fails the exact evaluation and evicts the globally oldest entry when
// the index exceeds maximumReserveBorrowerIndex. An asset qualifies when the
// borrower carries variable debt OR enabled collateral balance in it.
func (s *Screener) indexReserveEvidence(
	borrower string,
	blockNumber uint64,
	blockHash string,
	responseAccount account,
	reserves []exactReserve,
) {
	normalizedBorrower := strings.ToLower(borrower)
	if normalizedBorrower == "" || len(reserves) == 0 {
		return
	}
	accountIdentityValid := strings.EqualFold(normalizedBorrower, responseAccount.Borrower)

	s.mu.Lock()
	defer s.mu.Unlock()
	if s.reserveBorrowersByAsset == nil {
		s.reserveBorrowersByAsset = make(map[string]map[string]*reserveBorrowerEntry)
	}
	now := s.nowUTC()
	touched := false
	for _, reserve := range reserves {
		asset := strings.ToLower(reserve.Asset)
		if asset == "" {
			continue
		}
		var debt *big.Int
		if variable, ok := newBigUint(reserve.CurrentVariableDebt); ok && variable.Sign() > 0 {
			debt = variable
		}
		var collateral *big.Int
		if balance, ok := newBigUint(reserve.CurrentATokenBalance); ok && balance.Sign() > 0 && reserve.UsageAsCollateralEnabled {
			collateral = balance
		}
		byAsset := s.reserveBorrowersByAsset[asset]
		if debt == nil && collateral == nil {
			if byAsset != nil {
				if _, present := byAsset[normalizedBorrower]; present {
					delete(byAsset, normalizedBorrower)
					touched = true
					s.state.Counts[reserveIndexEntriesTrackedKey]--
				}
				if len(byAsset) == 0 {
					delete(s.reserveBorrowersByAsset, asset)
				}
			}
			continue
		}
		if byAsset == nil {
			byAsset = make(map[string]*reserveBorrowerEntry)
			s.reserveBorrowersByAsset[asset] = byAsset
		}
		totalDebtBase := ""
		healthFactor := ""
		if accountIdentityValid {
			totalDebtBase = responseAccount.TotalDebtBase
			healthFactor = responseAccount.HealthFactorWAD
		} else {
			// Preserve prior ranking evidence when the response account
			// identity does not match the requested borrower (mirrors the
			// ARB index convention).
			if existing, present := byAsset[normalizedBorrower]; present {
				totalDebtBase = existing.TotalDebtBase
				healthFactor = existing.HealthFactorWAD
			}
		}
		_, existed := byAsset[normalizedBorrower]
		byAsset[normalizedBorrower] = &reserveBorrowerEntry{
			Borrower:        normalizedBorrower,
			Asset:           asset,
			VariableDebtWei: debt,
			CollateralWei:   collateral,
			TotalDebtBase:   totalDebtBase,
			HealthFactorWAD: healthFactor,
			BlockNumber:     blockNumber,
			BlockHash:       blockHash,
			UpdatedAt:       now,
		}
		if !existed {
			s.state.Counts[reserveIndexEntriesTrackedKey]++
			touched = true
		}
	}
	if touched {
		s.evictReserveIndexOverflowLocked()
	}
}

func (s *Screener) reserveIndexEntryCountLocked() uint64 {
	total := uint64(0)
	for _, byAsset := range s.reserveBorrowersByAsset {
		total += uint64(len(byAsset))
	}
	return total
}

func (s *Screener) evictReserveIndexOverflowLocked() {
	for s.reserveIndexEntryCountLocked() > maximumReserveBorrowerIndex {
		var oldestAsset, oldestBorrower string
		var oldest time.Time
		for asset, byAsset := range s.reserveBorrowersByAsset {
			for borrower, entry := range byAsset {
				if oldestAsset == "" || entry.UpdatedAt.Before(oldest) {
					oldestAsset, oldestBorrower, oldest = asset, borrower, entry.UpdatedAt
				}
			}
		}
		if oldestAsset == "" {
			return
		}
		delete(s.reserveBorrowersByAsset[oldestAsset], oldestBorrower)
		if len(s.reserveBorrowersByAsset[oldestAsset]) == 0 {
			delete(s.reserveBorrowersByAsset, oldestAsset)
		}
		s.state.Counts[reserveIndexEntriesTrackedKey]--
	}
}

// queueAuctionTrigger records that a freshly stored pending auction needs
// affected-borrower screening. Deduplicated per asset; bounded queue drops
// the duplicate SIGNAL only (counted), never the auction registration.
func (s *Screener) queueAuctionTriggerLocked(asset string) {
	for _, queued := range s.pendingAuctionTriggerAssets {
		if queued == asset {
			s.state.Counts[auctionTriggerDroppedTotalKey]++
			return
		}
	}
	if len(s.pendingAuctionTriggerAssets) >= maximumAuctionTriggerQueue {
		// Drop the OLDEST queued signal; the newest asset is most likely to
		// still be within its deadline when the priority window wakes.
		s.pendingAuctionTriggerAssets = s.pendingAuctionTriggerAssets[1:]
		s.state.Counts[auctionTriggerDroppedTotalKey]++
	}
	s.pendingAuctionTriggerAssets = append(s.pendingAuctionTriggerAssets, asset)
	s.state.Counts[auctionTriggerEnqueuedTotalKey]++
}

// pendingAuctionTriggerCount reports whether a priority wake is due.
func (s *Screener) pendingAuctionTriggerCount() int {
	s.mu.Lock()
	defer s.mu.Unlock()
	return len(s.pendingAuctionTriggerAssets)
}

func (s *Screener) popAuctionTriggerAssetsLocked(maximum int) []string {
	if len(s.pendingAuctionTriggerAssets) == 0 {
		return nil
	}
	count := len(s.pendingAuctionTriggerAssets)
	if count > maximum {
		count = maximum
	}
	popped := append([]string(nil), s.pendingAuctionTriggerAssets[:count]...)
	s.pendingAuctionTriggerAssets = s.pendingAuctionTriggerAssets[count:]
	return popped
}

// auctionPriorityBorrowers selects, for the given queued assets, up to
// maximumAuctionTriggerCandidatesPerAsset indexed borrowers each, ranked by
// the shared liquidation-priority order, excluding in-flight and very
// recently served borrowers. Selection is pure bookkeeping over evidence
// already held; it issues no requests.
func (s *Screener) auctionPriorityBorrowers(assets []string) []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := s.nowUTC()
	selected := make([]string, 0, len(assets)*maximumAuctionTriggerCandidatesPerAsset)
	seen := make(map[string]bool, len(selected))
	for _, asset := range assets {
		matches := make([]*reserveBorrowerEntry, 0, 8)
		for candidateAsset, byAsset := range s.reserveBorrowersByAsset {
			// Index keys are lowercase reserve addresses; the queued asset may
			// be an SVR symbol resolved through verifiedAuctionAssetAliases.
			if !auctionAssetMatches(asset, candidateAsset) {
				continue
			}
			for _, entry := range byAsset {
				if s.exactInFlightBorrowers[entry.Borrower] || seen[entry.Borrower] {
					continue
				}
				if completedAt, served := s.lastExactAt[entry.Borrower]; served &&
					now.Sub(completedAt) < auctionTriggerRecentExactSkip {
					continue
				}
				matches = append(matches, entry)
			}
		}
		sort.Slice(matches, func(i, j int) bool {
			return liquidationPriorityLess(
				matches[i].TotalDebtBase, matches[i].HealthFactorWAD, matches[i].Borrower,
				matches[j].TotalDebtBase, matches[j].HealthFactorWAD, matches[j].Borrower,
			)
		})
		admitted := 0
		for _, entry := range matches {
			if admitted >= maximumAuctionTriggerCandidatesPerAsset {
				break
			}
			seen[entry.Borrower] = true
			selected = append(selected, entry.Borrower)
			admitted++
		}
	}
	return selected
}

// runAuctionPriority drains up to two queued auction assets and screens
// their best-indexed affected borrowers through the unchanged screen()
// pipeline (which performs prefilter, exact evidence, and Atlas shadow
// attachment with fresh state). It returns whether any work ran.
func (s *Screener) runAuctionPriority(ctx context.Context) (bool, error) {
	s.mu.Lock()
	assets := s.popAuctionTriggerAssetsLocked(2)
	s.mu.Unlock()
	if len(assets) == 0 {
		return false, nil
	}
	borrowers := s.auctionPriorityBorrowers(assets)
	if len(borrowers) == 0 {
		s.mu.Lock()
		s.state.Counts[auctionTriggerNoIndexTotalKey] += uint64(len(assets))
		s.mu.Unlock()
		return true, nil
	}
	s.mu.Lock()
	s.state.Counts[auctionTriggerBatchesTotalKey]++
	s.state.Counts[auctionTriggerBorrowersTotalKey] += uint64(len(borrowers))
	s.mu.Unlock()
	if err := s.screen(ctx, borrowers, false, nil); err != nil {
		return true, err
	}
	return true, nil
}

// pendingAuctionForReserves returns the oldest pending auction whose asset
// matches either reserve address (symbol-aware). Used by tests and by the
// registry helpers in screen.go/atlas_arb_shadow.go.
func (s *Screener) pendingAuctionForReserves(debtAsset, collateralAsset string) *observer.LedgerRecord {
	s.mu.Lock()
	defer s.mu.Unlock()
	debt := normalizeAuctionAsset(debtAsset)
	collateral := normalizeAuctionAsset(collateralAsset)
	if debt == "" && collateral == "" {
		return nil
	}
	var oldest *observer.LedgerRecord
	var oldestObserved time.Time
	for _, entry := range s.recentAuctions {
		if entry.evaluated || entry.record == nil {
			continue
		}
		if entry.record.OracleUpdate == nil || entry.record.OracleUpdate.Asset == nil {
			continue
		}
		key := normalizeAuctionAsset(*entry.record.OracleUpdate.Asset)
		if !auctionAssetMatches(key, debt) && !auctionAssetMatches(key, collateral) &&
			key != debt && key != collateral {
			continue
		}
		// Same deterministic total order as claimPendingArbAuction:
		// ObservedAt, then AuctionID ascending for timestamp ties.
		if oldest == nil ||
			entry.record.ObservedAt.Before(oldestObserved) ||
			(entry.record.ObservedAt.Equal(oldestObserved) && entry.record.AuctionID < oldest.AuctionID) {
			oldest = entry.record
			oldestObserved = entry.record.ObservedAt
		}
	}
	return oldest
}
