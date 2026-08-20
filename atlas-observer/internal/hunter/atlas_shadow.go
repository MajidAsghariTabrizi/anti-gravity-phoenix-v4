package hunter

import (
	"context"
	"math/big"
	"strings"
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
)

// Atlas shadow evaluation metric keys. Counts are stored in the durable
// hunter state Counts map; latency uses the same sum/count convention as the
// Exact histograms.
const (
	atlasShadowEvaluatedTotalKey     = "atlas_shadow_evaluated_total"
	atlasShadowEligibleTotalKey      = "atlas_shadow_eligible_total"
	atlasShadowRejectedPrefixKey     = "atlas_shadow_rejected_"
	atlasShadowIngressDecisionSumKey = "atlas_shadow_ingress_decision_millis_sum"
	atlasShadowIngressDecisionCntKey = "atlas_shadow_ingress_decision_millis_count"
)

// Atlas shadow terminal rejection reasons. These classify the independent
// SHADOW path only; they never authorize or gate live execution.
const (
	atlasShadowReasonIdentityInvalid             = "identity_invalid"
	atlasShadowReasonAssetUnknown                = "auction_asset_unknown"
	atlasShadowReasonBoundsInvalid               = "atlas_auction_bounds_invalid"
	atlasShadowReasonEconomicsConfigInvalid      = "atlas_economics_configuration_invalid"
	atlasShadowReasonExpiredBeforeEvaluation     = "auction_expired_before_evaluation"
	atlasShadowReasonSupersededWithoutEvaluation = "superseded_without_evaluation"
	atlasShadowReasonMissingAuction              = "atlas_auction_missing"
	atlasShadowReasonWethDebtRequired            = "atlas_weth_debt_required"
	atlasShadowReasonBidDisabled                 = "atlas_bid_disabled"
	atlasShadowReasonZeroBidBelowFloor           = "atlas_zero_bid_below_floor"
	atlasShadowReasonMaximumBidNonpositive       = "atlas_maximum_bid_nonpositive"
	atlasShadowReasonPostBidBelowFloor           = "atlas_post_bid_below_floor"
	atlasShadowReasonCallbackSimulationFailed    = "atlas_callback_simulation_failed"
	atlasShadowReasonCallbackEvidenceMismatch    = "atlas_callback_evidence_mismatch"
	atlasShadowReasonGrossMismatch               = "atlas_gross_mismatch_or_bid_unaffordable"
	atlasShadowReasonFinalConservativeBelowFloor = "atlas_final_conservative_below_floor"
	atlasShadowReasonCalldataInvalid             = "atlas_calldata_invalid"
)

// AtlasShadowSink persists independent Atlas shadow evaluations. The shadow
// ledger is explicitly NOT part of any live execution authority: it never
// writes execution_requests or atlas_solver_requests.
type AtlasShadowSink interface {
	RecordAtlasShadowEvaluation(context.Context, atlasShadowEvaluation) error
}

// recentAuction holds an in-memory auction awaiting a shadow evaluation for
// its asset. It is consumed (marked evaluated) by the first affected-borrower
// exact evaluation or expired/pruned by the auction handler.
type recentAuction struct {
	record    *observer.LedgerRecord
	evaluated bool
}

func (s *Screener) recentAuctionFor(debtAsset, collateralAsset string) *observer.LedgerRecord {
	s.mu.Lock()
	defer s.mu.Unlock()
	if len(s.recentAuctions) == 0 {
		return nil
	}
	debt := strings.ToLower(debtAsset)
	collateral := strings.ToLower(collateralAsset)
	for asset, entry := range s.recentAuctions {
		if entry.evaluated {
			continue
		}
		if asset == debt || asset == collateral {
			return entry.record
		}
	}
	return nil
}

func (s *Screener) markAuctionEvaluated(auctionID string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	for _, entry := range s.recentAuctions {
		if entry.record.AuctionID == auctionID {
			entry.evaluated = true
			return
		}
	}
}

// atlasAuctionBoundsReason validates the auction against the reviewed
// economic configuration. It returns "" when the auction bounds are valid,
// otherwise the shadow terminal rejection reason.
func atlasAuctionBoundsReason(auction *observer.LedgerRecord, config Config) string {
	if auction == nil || auction.AuctionID == "" {
		return atlasShadowReasonIdentityInvalid
	}
	if auction.OracleUpdate == nil || auction.OracleUpdate.Asset == nil || *auction.OracleUpdate.Asset == "" {
		return atlasShadowReasonAssetUnknown
	}
	deadline, deadlineOK := newUint64(auction.AuctionDeadlineBlock)
	oracleGasPrice, gasPriceOK := newBigUint(auction.OracleGasPriceWei)
	maximumFee, maximumFeeOK := newBigUint(config.MaximumFeePerGasWei)
	maximumPriorityFee, maximumPriorityOK := newBigUint(config.MaximumPriorityFeeWei)
	if !deadlineOK || deadline == 0 || auction.SolverGasLimit == 0 ||
		auction.SolverGasLimit > config.MaximumGasLimit ||
		!gasPriceOK || oracleGasPrice.Sign() <= 0 ||
		!maximumFeeOK || !maximumPriorityOK ||
		oracleGasPrice.Cmp(maximumFee) > 0 || oracleGasPrice.Cmp(maximumPriorityFee) > 0 {
		return atlasShadowReasonBoundsInvalid
	}
	return ""
}

// atlasShadowEvaluation is the canonical independent Atlas shadow record. All
// economic fields are integer strings; unknown values remain empty (NULL).
type atlasShadowEvaluation struct {
	Schema                      string    `json:"schema"`
	AuctionID                   string    `json:"auction_id"`
	UserOperationHash           string    `json:"user_operation_hash"`
	Dapp                        string    `json:"dapp"`
	Asset                       string    `json:"asset"`
	EvidenceHash                string    `json:"evidence_hash"`
	ObservedAt                  time.Time `json:"observed_at"`
	EvaluatedAt                 time.Time `json:"evaluated_at"`
	IngressLatencyMillis        uint64    `json:"ingress_latency_ms"`
	IdentityValid               bool      `json:"identity_valid"`
	BoundsValid                 bool      `json:"bounds_valid"`
	Borrower                    string    `json:"borrower,omitempty"`
	BlockNumber                 uint64    `json:"block_number"`
	BlockHash                   string    `json:"block_hash,omitempty"`
	ExactCompleted              bool      `json:"exact_completed"`
	CallbackSimulationAttempted bool      `json:"callback_simulation_attempted"`
	CallbackSimulationPassed    bool      `json:"callback_simulation_passed"`
	EvidenceMode                string    `json:"evidence_mode,omitempty"`
	SimulatedGasLimit           uint64    `json:"simulated_gas_limit"`
	SolverGasSettlementWei      string    `json:"solver_gas_settlement_wei,omitempty"`
	GrossValueWei               string    `json:"gross_value_wei,omitempty"`
	DirectCostWei               string    `json:"direct_cost_wei,omitempty"`
	ZeroBidConservativeWei      string    `json:"zero_bid_conservative_wei,omitempty"`
	MaximumBidWei               string    `json:"maximum_bid_wei,omitempty"`
	SelectedBidWei              string    `json:"selected_bid_wei,omitempty"`
	CompetitiveReserveWei       string    `json:"competitive_reserve_wei,omitempty"`
	ExpectedNetAfterBidWei      string    `json:"expected_net_after_bid_wei,omitempty"`
	ConservativeNetAfterBidWei  string    `json:"conservative_net_after_bid_wei,omitempty"`
	ShadowBidEligible           bool      `json:"shadow_bid_eligible"`
	TerminalRejectionReason     string    `json:"terminal_rejection_reason,omitempty"`
}

// atlasShadowTerminal builds the shadow record for an auction that reaches a
// terminal classification without a borrower evaluation.
func atlasShadowTerminal(auction *observer.LedgerRecord, identityValid, boundsValid bool, reason string) atlasShadowEvaluation {
	evaluation := atlasShadowEvaluation{
		Schema:                  "phoenix.atlas-shadow-evaluation.v1",
		EvaluatedAt:             time.Now().UTC(),
		IdentityValid:           identityValid,
		BoundsValid:             boundsValid,
		TerminalRejectionReason: reason,
	}
	if auction == nil {
		return evaluation
	}
	evaluation.ObservedAt = auction.ObservedAt
	evaluation.AuctionID = auction.AuctionID
	evaluation.UserOperationHash = auction.UserOpHash
	evaluation.Dapp = auction.Dapp
	evaluation.EvidenceHash = auction.NotificationSHA256
	if auction.OracleUpdate != nil && auction.OracleUpdate.Asset != nil {
		evaluation.Asset = *auction.OracleUpdate.Asset
	}
	if !evaluation.ObservedAt.IsZero() && !evaluation.EvaluatedAt.Before(evaluation.ObservedAt) {
		evaluation.IngressLatencyMillis = uint64(evaluation.EvaluatedAt.Sub(evaluation.ObservedAt).Milliseconds())
	}
	return evaluation
}

// recordAtlasShadow persists the shadow evaluation through the sink and
// updates the in-memory counters. It never touches candidate materialization.
func (s *Screener) recordAtlasShadow(ctx context.Context, evaluation atlasShadowEvaluation) error {
	s.mu.Lock()
	if s.state.Counts == nil {
		s.state.Counts = make(map[string]uint64)
	}
	s.state.Counts[atlasShadowEvaluatedTotalKey]++
	s.state.Counts[atlasShadowIngressDecisionSumKey] += evaluation.IngressLatencyMillis
	s.state.Counts[atlasShadowIngressDecisionCntKey]++
	if evaluation.ShadowBidEligible {
		s.state.Counts[atlasShadowEligibleTotalKey]++
	}
	if evaluation.TerminalRejectionReason != "" {
		s.state.Counts[atlasShadowRejectedPrefixKey+evaluation.TerminalRejectionReason+"_total"]++
	}
	s.mu.Unlock()
	if sink, ok := s.config.SignalSink.(AtlasShadowSink); ok {
		return sink.RecordAtlasShadowEvaluation(ctx, evaluation)
	}
	return nil
}

// atlasShadowFromCandidate maps a successfully built Atlas candidate into the
// shadow ledger. The live lane remains untouched: this never materializes
// atlas_solver_requests or execution_requests.
func (s *Screener) atlasShadowFromCandidate(auction *observer.LedgerRecord, record signal, selected *liquidationEvaluation, candidate *atlasCandidate) atlasShadowEvaluation {
	evaluation := atlasShadowTerminal(auction, true, atlasAuctionBoundsReason(auction, s.config) == "", "")
	evaluation.Borrower = record.Borrower
	evaluation.BlockNumber = record.Block
	evaluation.BlockHash = record.BlockHash
	evaluation.ExactCompleted = true
	evaluation.CallbackSimulationAttempted = true
	evaluation.CallbackSimulationPassed = candidate != nil
	evaluation.ShadowBidEligible = candidate != nil
	oracleGasPrice, gasPriceOK := newBigUint(auction.OracleGasPriceWei)
	if gasPriceOK {
		evaluation.SolverGasSettlementWei = new(big.Int).Mul(new(big.Int).SetUint64(auction.SolverGasLimit), oracleGasPrice).String()
	}
	if selected != nil && selected.Simulation != nil {
		evaluation.GrossValueWei = selected.Simulation.RealizedProfit
	}
	if selected != nil {
		evaluation.DirectCostWei = selected.ExecutionCost.String()
	}
	if candidate == nil {
		return evaluation
	}
	evaluation.EvidenceMode = candidate.EvidenceMode
	evaluation.SimulatedGasLimit = candidate.SimulatedGasLimit
	evaluation.ZeroBidConservativeWei = candidate.ZeroBidConservative
	evaluation.MaximumBidWei = candidate.MaximumBid
	evaluation.SelectedBidWei = candidate.SelectedBid
	maximumBid, maximumOK := newBigUint(candidate.MaximumBid)
	selectedBid, selectedOK := newBigUint(candidate.SelectedBid)
	if maximumOK && selectedOK {
		evaluation.CompetitiveReserveWei = new(big.Int).Sub(new(big.Int).Set(maximumBid), selectedBid).String()
	}
	evaluation.ExpectedNetAfterBidWei = candidate.ExpectedNetPnL
	evaluation.ConservativeNetAfterBidWei = candidate.ConservativeNetPnL
	return evaluation
}
