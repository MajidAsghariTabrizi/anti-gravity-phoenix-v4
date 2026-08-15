package hunter

import (
	"bufio"
	"bytes"
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/big"
	"net"
	"net/http"
	"os"
	"path/filepath"
	"reflect"
	"regexp"
	"sort"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
)

const (
	StateSchema                      = "phoenix.atlas-aave-hunter-state.v1"
	RequestSchema                    = "phoenix.rpc.aave-screen-request.v1"
	ResponseSchema                   = "phoenix.rpc.aave-screen-response.v2"
	PrimaryScreenResponseSchema      = "phoenix.rpc.aave-primary-screen-response.v1"
	DefaultBatch                     = 100
	MaximumBatch                     = 100
	watchHF                          = uint64(1_100_000_000_000_000_000)
	urgentHF                         = uint64(1_020_000_000_000_000_000)
	liquidatableHF                   = uint64(1_000_000_000_000_000_000)
	maximumResponse                  = 2 << 20
	gatewayReadyTimeout              = 90 * time.Second
	gatewayReadyPoll                 = 5 * time.Second
	initialScreenOffset              = 10 * time.Second
	startupRetryTimeout              = 90 * time.Second
	maximumStartupRetries            = 3
	providerDegradationTotalKey      = "provider_retryable_degradation_total"
	providerDisagreementTotalKey     = "provider_disagreement_degradation_total"
	providerUnavailableTotalKey      = "provider_unavailable_degradation_total"
	providerTimeoutTotalKey          = "provider_timeout_degradation_total"
	providerRateLimitedTotalKey      = "provider_rate_limited_degradation_total"
	providerRecoveryAttemptTotalKey  = "provider_recovery_attempt_total"
	providerRecoverySuccessTotalKey  = "provider_recovery_success_total"
	providerDegradedSinceMillisKey   = "provider_degraded_since_unix_millis"
	providerLastDegradedAtMillisKey  = "provider_last_degraded_at_unix_millis"
	providerLastRecoveryAtMillisKey  = "provider_last_recovery_at_unix_millis"
	providerLastDegradedDurationKey  = "provider_last_degraded_duration_millis"
	providerCurrentFailureStreakKey  = "provider_current_class_failure_streak"
	providerCircuitOpenTotalKey      = "provider_circuit_open_total"
	providerCircuitSkippedTotalKey   = "provider_circuit_skipped_total"
	providerCircuitCooldown          = 5 * time.Minute
	inMemoryProviderRecoveryWindow   = 2 * time.Minute
	gatewayBudgetCircuitCooldown     = 30 * time.Second
	hotRevisitCadence                = 10 * time.Second
	aaveSimulationBatchTimeout       = 55 * time.Second
	exactBorrowerCooldown            = 2 * time.Minute
	defaultExactStateBudgetPerMinute = uint64(12)
	defaultExactDiscoveryReserve     = uint64(7)
	defaultExactRequestEstimateMilli = uint64(5_000)
	defaultExactWorkers              = 12
	defaultExactForkWorkers          = 2
	directForkEvidenceMode           = "SINGLE_PRIMARY_FORK_VERIFIED"
	counterfactualForkEvidenceMode   = "SINGLE_PRIMARY_COUNTERFACTUAL_FORK_VERIFIED"
	atlasCallbackEvidenceMode        = "SINGLE_PRIMARY_ATLAS_CALLBACK_FORK_VERIFIED"
	primaryProviderID                = "production-nownodes-arbitrum"
	maximumReviewedInputWei          = "10000000000000000"
	fixedReviewedSizeClassification  = "fixed_reviewed_size"
	terminalSizeClassification       = "terminal_size_required"
	belowMinReviewedSizeReason       = "below_min_reviewed_size"
	dustPartialInvalidReason         = "dust_partial_invalid"
	exactDeferredCooldownKey         = "exact_deferred_cooldown_total"
	exactDeferredRouteIneligibleKey  = "exact_deferred_route_ineligible_total"
	exactDeferredSchedulerKey        = "exact_deferred_scheduler_capacity_total"
	exactRouteIneligibleObservedKey  = "exact_route_ineligible_observed_total"
	atlasCallbackUnavailableKey      = "atlas_callback_evidence_unavailable_total"
	hotRecheckTotalKey               = "hot_recheck_total"
	hotRecheckDeferredBudgetKey      = "hot_recheck_deferred_budget_total"
	exactEvalStartedKey              = "exact_eval_started_total"
	exactEvalCompletedKey            = "exact_eval_completed_total"
	exactEvalLatencySumKey           = "exact_eval_latency_millis_sum"
	exactEvalLatencyCountKey         = "exact_eval_latency_millis_count"
	exactQueueLatencySumKey          = "exact_queue_latency_millis_sum"
	exactQueueLatencyCountKey        = "exact_queue_latency_millis_count"
	exactEligibilityLatencySumKey    = "exact_eligibility_latency_millis_sum"
	exactEligibilityLatencyCountKey  = "exact_eligibility_latency_millis_count"
	exactDispatchLatencySumKey       = "exact_dispatch_latency_millis_sum"
	exactDispatchLatencyCountKey     = "exact_dispatch_latency_millis_count"
	exactInitialLatencySumKey        = "exact_initial_response_latency_millis_sum"
	exactInitialLatencyCountKey      = "exact_initial_response_latency_millis_count"
	signalPrefilterLatencySumKey     = "signal_prefilter_latency_millis_sum"
	signalPrefilterLatencyCountKey   = "signal_prefilter_latency_millis_count"
	exactFirstRPCLatencySumKey       = "exact_first_rpc_latency_millis_sum"
	exactFirstRPCLatencyCountKey     = "exact_first_rpc_latency_millis_count"
	exactComputeLatencySumKey        = "exact_compute_latency_millis_sum"
	exactComputeLatencyCountKey      = "exact_compute_latency_millis_count"
	exactForkRuntimeSumKey           = "exact_fork_runtime_millis_sum"
	exactForkRuntimeCountKey         = "exact_fork_runtime_millis_count"
	exactForkQueueSumKey             = "exact_fork_queue_millis_sum"
	exactForkQueueCountKey           = "exact_fork_queue_millis_count"
	exactCoalescedKey                = "exact_coalesced_total"
	exactStaleInvalidatedKey         = "exact_stale_invalidated_total"
	exactDuplicateSuppressedKey      = "exact_duplicate_suppressed_total"
	liquidatableToExactSumKey        = "liquidatable_to_exact_millis_sum"
	liquidatableToExactCountKey      = "liquidatable_to_exact_millis_count"
	routeIneligibleRechecksKey       = "route_ineligible_rechecks_total"
	aavePoolAddress                  = "0x794a61358d6845594f94dc1db02a252b5b4814ad"
	wethAddress                      = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1"
	nativeUSDCAddress                = "0xaf88d065e77c8cc2239327c5edb3a432268e5831"
	usdcEAddress                     = "0xff970a61a04b1ca14834a43f5de4533ebddb5cc8"
	wethNativeUSDCPool100Address     = "0x6f38e884725a116c9c7fbf208e79fe8828a2595f"
	wethNativeUSDCPool500Address     = "0xc6962004f452be9203591991d15f6b388e09e8d0"
	wethNativeUSDCPool3000Address    = "0xc473e2aee3441bf9240be85eb122abb059a3b57c"
	wethUSDCePool500Address          = "0xc31e54c7a869b9fcbecc14363cf510d1c41fa443"
	zeroAddress                      = "0x0000000000000000000000000000000000000000"
	uniswapFactoryAddress            = "0x1f98431c8ad98523631ae4a59f267346ea31f984"
)

const (
	revenueLaneAuthorityDivergedKey   = "revenue_lane_authority_diverged_total"
	revenueLaneAuthorityDivergedClass = "revenue_lane_authority_diverged"
)

var addressPattern = regexp.MustCompile(`^0x[0-9a-f]{40}$`)
var releaseSHAPattern = regexp.MustCompile(`^[0-9a-f]{40}$`)
var errorClassPattern = regexp.MustCompile(`^[a-z][a-z0-9_]{0,63}$`)
var errPriorityWindowStopped = errors.New("priority recheck window stopped")

type Config struct {
	DiscoveryPath          string
	DiscoverySHA256        string
	StateDir               string
	GatewayURL             string
	StartingCursor         uint64
	BatchSize              int
	Pace                   time.Duration
	RetainedProfitFloorWei string
	MaximumInputAmountWei  string
	MaximumGasLimit        uint64
	MaximumFeePerGasWei    string
	MaximumAtlasBidWei     string
	FlashPremiumBPS        uint64
	EconomicReserveBPS     uint64
	ExecutorAddress        string
	ExecutorCodeHash       string
	CallerAddress          string
	ReleaseSHA             string
	MaximumPriorityFeeWei  string
	// PrimaryDiscovery uses the discovery-only gateway endpoint in Production.
	// Both endpoints are bound to the same reviewed single primary.
	PrimaryDiscovery               bool
	ExactStateBudgetPerMinute      uint64
	ExactDiscoveryReservePerMinute uint64
	ExactWorkers                   int
	SignalSink                     SignalSink
}

type SignalSink interface {
	RecordAaveSignal(context.Context, signal) (signal, error)
}

type ProviderAuthoritySink interface {
	RecordProviderFailure(context.Context, string, time.Time) error
	ResetProviderRecoveryEvidence(context.Context, string, time.Time) error
}

type ProviderRecoverySample struct {
	ObservedAt           time.Time `json:"observed_at"`
	PrimaryProvider      string    `json:"primary_provider"`
	Confirmation         *string   `json:"confirmation"`
	Quorum               uint8     `json:"quorum"`
	ConfirmationProvider string    `json:"-"`
}

type AtlasAuctionDispositionSink interface {
	RecordAtlasCallbackUnavailable(context.Context, string, string) error
}

// LiveSizeAuthority is implemented by the durable Production sink. It keeps
// shadow evaluation bounded by the reviewed ladder while ensuring that live
// Candidate authority comes from the current economic/revenue-lane controls,
// not merely from the executor-wide reviewed ceiling.
type LiveSizeAuthority interface {
	CurrentAaveLiveMaximumInputAmount(context.Context) (string, error)
}

type State struct {
	Schema                             string                   `json:"schema"`
	DiscoverySHA256                    string                   `json:"discovery_sha256"`
	SourceAddressCount                 uint64                   `json:"source_address_count"`
	Cursor                             uint64                   `json:"cursor"`
	LastBlockNumber                    uint64                   `json:"last_block_number"`
	LastBlockHash                      string                   `json:"last_block_hash"`
	LastProviderPrimary                string                   `json:"last_provider_primary"`
	LastProviderSecond                 string                   `json:"last_provider_secondary,omitempty"`
	ProviderRecoverySamples            []ProviderRecoverySample `json:"provider_recovery_samples,omitempty"`
	LastBatchAt                        *time.Time               `json:"last_batch_at"`
	LastTailAt                         *time.Time               `json:"last_tail_at"`
	LastPrimaryExactAt                 *time.Time               `json:"last_primary_exact_at,omitempty"`
	LastDualAgreementAt                *time.Time               `json:"-"`
	TailNextBlock                      uint64                   `json:"tail_next_block"`
	DebtBearingCount                   uint64                   `json:"debt_bearing_count"`
	Counts                             map[string]uint64        `json:"counts"`
	RouteIneligible                    map[string]string        `json:"route_ineligible,omitempty"`
	TailInvalidatedBlock               map[string]uint64        `json:"tail_invalidated_block,omitempty"`
	ExactQueueCount                    uint64                   `json:"exact_queue_count"`
	LastExactAdmissionAt               *time.Time               `json:"last_exact_admission_at,omitempty"`
	ExactBudgetTokensMilli             uint64                   `json:"exact_budget_tokens_milli,omitempty"`
	ExactBudgetUpdatedAt               *time.Time               `json:"exact_budget_updated_at,omitempty"`
	ExactAverageStateRequestsMilli     uint64                   `json:"exact_average_state_requests_milli,omitempty"`
	IncompleteCount                    uint64                   `json:"incomplete_count"`
	LastErrorClass                     string                   `json:"last_error_class,omitempty"`
	LastAttemptAt                      *time.Time               `json:"last_attempt_at,omitempty"`
	StartupRetryCount                  uint64                   `json:"startup_retry_count,omitempty"`
	ProviderCircuitOpenTotal           uint64                   `json:"provider_circuit_open_total"`
	ProviderCircuitSkippedTotal        uint64                   `json:"provider_circuit_skipped_total"`
	ProviderCircuitOpenUntilUnixMillis int64                    `json:"provider_circuit_open_until_unix_millis"`
	HotQueueSize                       uint64                   `json:"hot_queue_size"`
	LiquidatableHotCount               uint64                   `json:"liquidatable_hot_count"`
	UrgentHotCount                     uint64                   `json:"urgent_hot_count"`
	ExactEligibleNowCount              uint64                   `json:"-"`
	SchedulerBlockedCount              uint64                   `json:"-"`
	CooldownBlockedCount               uint64                   `json:"-"`
	RouteIneligibleCount               uint64                   `json:"-"`
	ProviderBlockedCount               uint64                   `json:"-"`
	AuthorityBlockedCount              uint64                   `json:"-"`
	ExactEvaluationsInFlight           uint64                   `json:"-"`
	ExactWorkersRunning                uint64                   `json:"-"`
	ExactWorkerQueueDepth              uint64                   `json:"-"`
	OldestExactEligibleAgeMillis       uint64                   `json:"-"`
	ActiveForkPendingCount             uint64                   `json:"-"`
}

type Screener struct {
	config                 Config
	client                 *http.Client
	batchClient            *http.Client
	operationMu            sync.Mutex
	mu                     sync.Mutex
	state                  State
	debtBearing            map[string]bool
	refreshKnown           map[string]bool
	refreshOrder           []string
	refreshCursor          int
	hotBorrowers           map[string]string
	hotDebtBase            map[string]string
	hotUpperPositive       map[string]bool
	latestOutcome          map[string]string
	lastExactAt            map[string]time.Time
	firstLiquidatableAt    map[string]time.Time
	firstLiquidatableMono  map[string]time.Time
	lastExactAdmissionAt   time.Time
	hasDurableAdmission    bool
	exactInFlight          uint64
	exactInFlightBorrowers map[string]bool
	exactWorkerSlots       chan struct{}
	exactForkSlots         chan struct{}
	exactForkSlotsOnce     sync.Once
	exactWorkerStarts      atomic.Uint64
	wait                   func(context.Context, time.Duration) bool
	now                    func() time.Time
}

type exactEvaluationTracker struct {
	requests                 atomic.Uint64
	workerStartedUnixNanos   atomic.Int64
	firstRequestUnixNanos    atomic.Int64
	initialResponseUnixNanos atomic.Int64
	workerStartedMonotonic   time.Time
	firstRequestMonotonic    time.Time
	initialResponseMonotonic time.Time
	forkRuntimeNanos         atomic.Uint64
	forkQueueNanos           atomic.Uint64
}

type exactEvaluationTrackerKey struct{}

func exactTrackerFromContext(ctx context.Context) *exactEvaluationTracker {
	tracker, _ := ctx.Value(exactEvaluationTrackerKey{}).(*exactEvaluationTracker)
	return tracker
}

func (s *Screener) exactForkPermits() chan struct{} {
	s.exactForkSlotsOnce.Do(func() {
		s.exactForkSlots = make(chan struct{}, defaultExactForkWorkers)
	})
	return s.exactForkSlots
}

type pendingExactEvaluation struct {
	order                  int
	primary                account
	bucket                 string
	record                 signal
	admittedAt             time.Time
	admittedMonotonic      time.Time
	firstObserved          time.Time
	firstObservedMonotonic time.Time
	reservedMilli          uint64
	signalToPrefilter      time.Duration
	tracker                *exactEvaluationTracker
	result                 chan exactEvaluationResult
}

type completedScreenSignal struct {
	order             int
	record            signal
	borrower          string
	bucket            string
	signalToPrefilter time.Duration
}

type exactEvaluationResult struct {
	record             signal
	err                error
	completedAt        time.Time
	completedMonotonic time.Time
}

type gatewayErrorContract struct {
	ErrorClass       string  `json:"error_class"`
	Retryable        bool    `json:"retryable"`
	RetryAfterSecond *uint64 `json:"retry_after_seconds,omitempty"`
}

type gatewayResponseError struct {
	statusCode int
	class      string
	retryable  bool
	retryAfter time.Duration
}

func (e *gatewayResponseError) Error() string {
	return fmt.Sprintf("RPC Gateway rejected request: %s", e.class)
}

type screenRequest struct {
	SchemaVersion string   `json:"schema_version"`
	ChainID       uint64   `json:"chain_id"`
	RequestID     string   `json:"request_id"`
	Borrowers     []string `json:"borrowers"`
}

type account struct {
	Borrower                    string `json:"borrower"`
	TotalCollateralBase         string `json:"total_collateral_base"`
	TotalDebtBase               string `json:"total_debt_base"`
	AvailableBorrowsBase        string `json:"available_borrows_base"`
	CurrentLiquidationThreshold string `json:"current_liquidation_threshold_bps"`
	LoanToValueBPS              string `json:"loan_to_value_bps"`
	HealthFactorWAD             string `json:"health_factor_wad"`
}

type providerScreen struct {
	ProviderID    string    `json:"provider_id"`
	WETHPriceBase string    `json:"weth_price_base"`
	Accounts      []account `json:"accounts"`
}

type screenResponse struct {
	SchemaVersion string          `json:"schema_version"`
	ChainID       uint64          `json:"chain_id"`
	RequestID     string          `json:"request_id"`
	BlockNumber   uint64          `json:"block_number"`
	BlockHash     string          `json:"block_hash"`
	Primary       providerScreen  `json:"primary"`
	Confirmation  *providerScreen `json:"confirmation"`
	Quorum        uint8           `json:"quorum"`
	Secondary     providerScreen  `json:"-"`
}

// primaryScreenResponse is discovery-only.  A primary screen can enqueue an
// Exact evaluation but it can never provide Candidate authority; resolveExact
// always obtains fresh independent provider evidence first.
type primaryScreenResponse struct {
	SchemaVersion string         `json:"schema_version"`
	ChainID       uint64         `json:"chain_id"`
	RequestID     string         `json:"request_id"`
	BlockNumber   uint64         `json:"block_number"`
	BlockHash     string         `json:"block_hash"`
	Primary       providerScreen `json:"primary"`
}

type exactRequest struct {
	SchemaVersion      string `json:"schema_version"`
	ChainID            uint64 `json:"chain_id"`
	RequestID          string `json:"request_id"`
	Borrower           string `json:"borrower"`
	MaximumInputAmount string `json:"maximum_input_amount"`
}

type tailRequest struct {
	SchemaVersion string `json:"schema_version"`
	ChainID       uint64 `json:"chain_id"`
	RequestID     string `json:"request_id"`
	FromBlock     uint64 `json:"from_block"`
}

type tailResponse struct {
	SchemaVersion        string   `json:"schema_version"`
	ChainID              uint64   `json:"chain_id"`
	RequestID            string   `json:"request_id"`
	FinalizedBlockNumber uint64   `json:"finalized_block_number"`
	FinalizedBlockHash   string   `json:"finalized_block_hash"`
	FromBlock            uint64   `json:"from_block"`
	ToBlock              uint64   `json:"to_block"`
	NextBlock            uint64   `json:"next_block"`
	PrimaryProviderID    string   `json:"primary_provider_id"`
	ConfirmationProvider *string  `json:"confirmation_provider_id"`
	Quorum               uint8    `json:"quorum"`
	SecondaryProviderID  string   `json:"-"`
	Borrowers            []string `json:"borrowers"`
}

type borrowerActivity struct {
	Borrower string `json:"borrower"`
	Active   bool   `json:"active"`
}

type exactLiquidation struct {
	DebtAsset              string             `json:"debt_asset"`
	CollateralAsset        string             `json:"collateral_asset"`
	DebtAssetDecimals      uint8              `json:"debt_asset_decimals"`
	DebtAssetPriceBase     string             `json:"debt_asset_price_base"`
	WETHPriceBase          string             `json:"weth_price_base"`
	MaximumRepayAmount     string             `json:"maximum_repay_amount"`
	ReviewedSizeWETHWei    string             `json:"reviewed_size_weth_wei"`
	DebtAssetReview        string             `json:"debt_asset_review"`
	SizeClassification     string             `json:"size_classification"`
	TerminalSizeReason     string             `json:"terminal_size_reason"`
	RequestedRepayAmount   string             `json:"requested_repay_amount"`
	ActualRepayAmount      string             `json:"actual_repay_amount"`
	RepayAmount            string             `json:"repay_amount"`
	FlashPremiumAmount     string             `json:"flash_premium_amount"`
	SeizedCollateral       string             `json:"seized_collateral"`
	ProtocolFeeCollateral  string             `json:"protocol_fee_collateral"`
	LiquidatorCollateral   string             `json:"liquidator_collateral"`
	OracleUnwindOutputWETH string             `json:"oracle_unwind_output_weth"`
	OracleUnwindOutputDebt string             `json:"oracle_unwind_output_debt_asset"`
	UnwindQuotes           []exactUnwindQuote `json:"unwind_quotes"`
}

type exactUnwindQuote struct {
	Pool       string `json:"pool"`
	Factory    string `json:"factory"`
	Token0     string `json:"token0"`
	Token1     string `json:"token1"`
	Fee        uint32 `json:"fee"`
	ZeroForOne bool   `json:"zero_for_one"`
	OutputWETH string `json:"output_weth"`
	OutputDebt string `json:"output_debt_asset"`
}

type exactReserve struct {
	Asset                    string `json:"asset"`
	ReserveID                uint16 `json:"reserve_id"`
	Decimals                 uint8  `json:"decimals"`
	CurrentATokenBalance     string `json:"current_a_token_balance"`
	CurrentStableDebt        string `json:"current_stable_debt"`
	CurrentVariableDebt      string `json:"current_variable_debt"`
	UsageAsCollateralEnabled bool   `json:"usage_as_collateral_enabled"`
	ConfigurationData        string `json:"configuration_data"`
	AToken                   string `json:"a_token"`
	StableDebtToken          string `json:"stable_debt_token"`
	VariableDebtToken        string `json:"variable_debt_token"`
	OraclePriceBase          string `json:"oracle_price_base"`
	LiquidationGraceUntil    uint64 `json:"liquidation_grace_period_until"`
}

type exactProvider struct {
	ProviderID                 string             `json:"provider_id"`
	PoolCodeHash               string             `json:"pool_code_hash"`
	PoolImplementation         string             `json:"pool_implementation"`
	PoolImplementationCodeHash string             `json:"pool_implementation_code_hash"`
	UserConfiguration          string             `json:"user_configuration"`
	UserEModeCategory          uint8              `json:"user_emode_category"`
	EModeCollateralBitmap      string             `json:"emode_collateral_bitmap"`
	EModeLiquidationBonusBPS   uint16             `json:"emode_liquidation_bonus_bps"`
	Account                    account            `json:"account"`
	Reserves                   []exactReserve     `json:"reserves"`
	FlashPremiumBPS            uint64             `json:"flash_premium_bps"`
	Liquidations               []exactLiquidation `json:"liquidations"`
}

type exactResponse struct {
	SchemaVersion string         `json:"schema_version"`
	ChainID       uint64         `json:"chain_id"`
	RequestID     string         `json:"request_id"`
	BlockNumber   uint64         `json:"block_number"`
	BlockHash     string         `json:"block_hash"`
	StateRoot     string         `json:"state_root"`
	Primary       exactProvider  `json:"primary"`
	Confirmation  *exactProvider `json:"confirmation"`
	Quorum        uint8          `json:"quorum"`
	Secondary     exactProvider  `json:"-"`
}

type signal struct {
	Schema                      string                  `json:"schema"`
	ObservedAt                  time.Time               `json:"observed_at"`
	ExactCompletedAt            *time.Time              `json:"exact_completed_at,omitempty"`
	Cursor                      uint64                  `json:"cursor"`
	Block                       uint64                  `json:"block_number"`
	BlockHash                   string                  `json:"block_hash"`
	Borrower                    string                  `json:"borrower"`
	DebtBase                    string                  `json:"total_debt_base"`
	HF                          string                  `json:"health_factor_wad"`
	Bucket                      string                  `json:"bucket"`
	Authority                   bool                    `json:"candidate_authority"`
	ExactDeferredReason         string                  `json:"exact_deferred_reason,omitempty"`
	ExactRouteIneligibleReason  string                  `json:"exact_route_ineligible_reason,omitempty"`
	ZeroCostProfitUpperBoundWei string                  `json:"zero_cost_profit_upper_bound_wei,omitempty"`
	ExpectedNetPnLWei           string                  `json:"expected_net_pnl_wei,omitempty"`
	ConservativeNetPnLWei       string                  `json:"conservative_net_pnl_wei,omitempty"`
	RiskReserveAmountWei        string                  `json:"risk_reserve_amount_wei,omitempty"`
	ExecutionCostWei            string                  `json:"execution_cost_wei,omitempty"`
	EstimatedL1CostWei          string                  `json:"estimated_l1_cost_wei,omitempty"`
	FlashPremiumWei             string                  `json:"flash_premium_wei,omitempty"`
	StateRoot                   string                  `json:"state_root,omitempty"`
	SelectedRoute               string                  `json:"selected_route,omitempty"`
	TerminalOutcome             string                  `json:"terminal_outcome"`
	AuthorityRejectionReason    string                  `json:"authority_rejection_reason,omitempty"`
	SizeDiagnostics             []sizeDiagnostic        `json:"reviewed_size_diagnostics,omitempty"`
	ExactDiagnostics            *exactDiagnosticSummary `json:"exact_diagnostics,omitempty"`
	ExactPrimaryProvider        string                  `json:"exact_primary_provider,omitempty"`
	ExactConfirmationProvider   *string                 `json:"exact_confirmation_provider"`
	ExactSecondaryProvider      string                  `json:"-"`
	ExecutionCandidate          *executionCandidate     `json:"-"`
	AtlasCandidate              *atlasCandidate         `json:"-"`
	PrefilterCompletedAt        time.Time               `json:"-"`
	LiquidatableClassifiedAt    time.Time               `json:"-"`
}

type sizeDiagnostic struct {
	ReviewedSize             string `json:"reviewed_size"`
	DebtAsset                string `json:"debt_asset"`
	CollateralAsset          string `json:"collateral_asset"`
	DebtAssetReview          string `json:"debt_asset_review"`
	SizeClassification       string `json:"size_classification"`
	TerminalSizeReason       string `json:"terminal_size_reason,omitempty"`
	TerminalSizeUnprofitable bool   `json:"terminal_size_unprofitable"`
	Route                    string `json:"route"`
	GrossLiquidationEdgeWei  string `json:"gross_liquidation_edge_wei"`
	FlashPremiumWei          string `json:"flash_premium_wei"`
	DEXUnwindLossWei         string `json:"dex_unwind_loss_wei"`
	PriceImpactBPS           string `json:"price_impact_bps"`
	GasLimit                 uint64 `json:"gas_limit"`
	GasPriceWei              string `json:"gas_price_wei"`
	L1CostWei                string `json:"l1_cost_wei"`
	ExecutionCostWei         string `json:"execution_cost_wei"`
	AtlasExposureWei         string `json:"atlas_exposure_wei"`
	AtlasBidWei              string `json:"atlas_bid_wei"`
	RiskReserveWei           string `json:"risk_reserve_wei"`
	ExpectedNetPnLWei        string `json:"expected_net_pnl_wei"`
	ConservativeNetPnLWei    string `json:"conservative_net_pnl_wei"`
	MarginToRetainedFloorWei string `json:"margin_to_retained_profit_gate_wei"`
	LiveAuthorized           bool   `json:"live_authorized"`
	FinalRejectionReason     string `json:"final_rejection_reason,omitempty"`
	EvidenceMode             string `json:"evidence_mode,omitempty"`
	Selected                 bool   `json:"selected,omitempty"`
}

type exactDiagnosticSummary struct {
	Schema                           string            `json:"schema"`
	EvaluationStage                  string            `json:"evaluation_stage"`
	RouteEligibility                 string            `json:"route_eligibility"`
	ReviewedCombinationCount         uint64            `json:"reviewed_combination_count"`
	RejectionCounts                  map[string]uint64 `json:"rejection_counts"`
	TopDiagnostics                   []sizeDiagnostic  `json:"top_diagnostics,omitempty"`
	BestDiagnostic                   *sizeDiagnostic   `json:"best_diagnostic,omitempty"`
	BestLiveAuthorizedDiagnostic     *sizeDiagnostic   `json:"best_live_authorized_diagnostic,omitempty"`
	SelectedDiagnostic               *sizeDiagnostic   `json:"selected_diagnostic,omitempty"`
	ClosestMarginToRetainedFloorWei  string            `json:"closest_margin_to_retained_profit_gate_wei,omitempty"`
	AnyCounterfactualPositive        bool              `json:"any_counterfactual_positive"`
	AnyLiveAuthorizedPositive        bool              `json:"any_live_authorized_positive"`
	ForkAttempted                    bool              `json:"fork_attempted"`
	ForkPassed                       bool              `json:"fork_passed"`
	ForkEvidenceMode                 string            `json:"fork_evidence_mode,omitempty"`
	FailureClass                     string            `json:"failure_class,omitempty"`
	LiquidatableToExactLatencyMillis uint64            `json:"liquidatable_to_exact_latency_ms"`
	ExactForkLatencyMillis           uint64            `json:"exact_fork_latency_ms"`
	QueueToWorkerLatencyMillis       uint64            `json:"queue_to_worker_latency_ms"`
	EligibilityToAdmissionMillis     uint64            `json:"eligibility_to_admission_latency_ms"`
	WorkerToGatewayLatencyMillis     uint64            `json:"worker_to_gateway_latency_ms"`
	InitialGatewayLatencyMillis      uint64            `json:"initial_gateway_response_latency_ms"`
	ExactStateRequestCount           uint64            `json:"exact_state_request_count"`
	SignalReceivedAt                 string            `json:"signal_received_at"`
	PrefilterCompletedAt             string            `json:"prefilter_completed_at"`
	LiquidatableClassifiedAt         string            `json:"liquidatable_classified_at"`
	ExactEnqueuedAt                  string            `json:"exact_enqueued_at"`
	ExactWorkerStartedAt             string            `json:"exact_worker_started_at"`
	FirstGatewayRequestAt            string            `json:"first_gateway_request_at"`
	InitialGatewayResponseAt         string            `json:"initial_gateway_response_at"`
	ExactEvaluationCompletedAt       string            `json:"exact_evaluation_completed_at"`
}

type atlasPreparedOperation struct {
	From         string  `json:"from"`
	To           string  `json:"to"`
	Value        string  `json:"value"`
	Gas          uint64  `json:"gas"`
	MaxFeePerGas string  `json:"max_fee_per_gas"`
	Deadline     uint64  `json:"deadline"`
	Solver       string  `json:"solver"`
	Control      string  `json:"control"`
	UserOpHash   string  `json:"user_op_hash"`
	BidToken     *string `json:"bid_token"`
	BidAmount    string  `json:"bid_amount"`
	Data         string  `json:"data"`
}

type atlasCandidate struct {
	AuctionID            string
	SelectedSize         string
	MaximumInputAmount   string
	MaximumBid           string
	SelectedBid          string
	ExpectedNetPnL       string
	ConservativeNetPnL   string
	EvidenceMode         string
	SimulationResultHash string
	OperationHash        string
	Operation            atlasPreparedOperation
	ObservedAt           time.Time
}

type simulationRequest struct {
	SchemaVersion             string `json:"schema_version"`
	ChainID                   uint64 `json:"chain_id"`
	RequestID                 string `json:"request_id"`
	BlockNumber               uint64 `json:"block_number"`
	BlockHash                 string `json:"block_hash"`
	StateRoot                 string `json:"state_root"`
	ExecutorAddress           string `json:"executor_address"`
	ExecutorCodeHash          string `json:"executor_code_hash"`
	CallerAddress             string `json:"caller_address"`
	ReleaseSHA                string `json:"release_sha"`
	Borrower                  string `json:"borrower"`
	DebtAsset                 string `json:"debt_asset"`
	CollateralAsset           string `json:"collateral_asset"`
	DebtAssetDecimals         uint8  `json:"debt_asset_decimals"`
	DebtAssetPriceBase        string `json:"debt_asset_price_base"`
	WETHPriceBase             string `json:"weth_price_base"`
	RepayAmount               string `json:"repay_amount"`
	MaximumInputAmount        string `json:"maximum_input_amount"`
	LiveMaximumInputAmount    string `json:"live_maximum_input_amount"`
	MaximumInputWETHWei       string `json:"maximum_input_weth_wei"`
	LiveMaximumInputWETHWei   string `json:"live_maximum_input_weth_wei"`
	Counterfactual            bool   `json:"counterfactual"`
	MinimumCollateralReceived string `json:"minimum_collateral_received"`
	MinimumUnwindOutput       string `json:"minimum_unwind_output"`
	MinimumProfit             string `json:"minimum_profit"`
	MinimumProfitWETHWei      string `json:"minimum_profit_weth_wei"`
	ExpectedProfit            string `json:"expected_profit"`
	RetainedProfitFloor       string `json:"retained_profit_floor"`
	SelectedPool              string `json:"selected_pool"`
	SelectedFactory           string `json:"selected_factory"`
	SelectedFee               uint32 `json:"selected_fee"`
	ZeroForOne                bool   `json:"zero_for_one"`
	GasLimit                  uint64 `json:"gas_limit"`
	MaxFeePerGas              string `json:"max_fee_per_gas"`
	MaxPriorityFeePerGas      string `json:"max_priority_fee_per_gas"`
	DeadlineUnixSeconds       uint64 `json:"deadline_unix_seconds"`
	AtlasMode                 bool   `json:"atlas_mode"`
	AtlasBid                  string `json:"atlas_bid"`
}

type simulationResponse struct {
	SchemaVersion             string  `json:"schema_version"`
	ChainID                   uint64  `json:"chain_id"`
	RequestID                 string  `json:"request_id"`
	BlockNumber               uint64  `json:"block_number"`
	BlockHash                 string  `json:"block_hash"`
	StateRoot                 string  `json:"state_root"`
	PrimaryProviderID         string  `json:"primary_provider_id"`
	ConfirmationProviderID    *string `json:"confirmation_provider_id"`
	Quorum                    uint8   `json:"quorum"`
	SecondaryProviderID       string  `json:"-"`
	EvidenceMode              string  `json:"evidence_mode"`
	RouteID                   string  `json:"route_id"`
	CalldataHex               string  `json:"calldata_hex"`
	CalldataHash              string  `json:"calldata_hash"`
	SimulationResultHash      string  `json:"simulation_result_hash"`
	RealizedProfit            string  `json:"realized_profit"`
	RealizedProfitDebtAsset   string  `json:"realized_profit_debt_asset"`
	ConservativeNetPnL        string  `json:"conservative_net_pnl"`
	EstimatedGasLimit         uint64  `json:"estimated_gas_limit"`
	EstimatedMaxFeePerGasWei  string  `json:"estimated_max_fee_per_gas_wei"`
	EstimatedExecutionCostWei string  `json:"estimated_execution_cost_wei"`
	EstimatedL1CostWei        string  `json:"estimated_l1_cost_wei"`
	FlashPremiumWei           string  `json:"flash_premium_wei"`
	FlashPremiumDebtAsset     string  `json:"flash_premium_debt_asset"`
	DeadlineUnixSeconds       uint64  `json:"deadline_unix_seconds"`
}

type simulationBatchRequest struct {
	SchemaVersion string              `json:"schema_version"`
	ChainID       uint64              `json:"chain_id"`
	RequestID     string              `json:"request_id"`
	Simulations   []simulationRequest `json:"simulations"`
}

type simulationBatchResult struct {
	RequestID string                `json:"request_id"`
	Response  *simulationResponse   `json:"response"`
	Error     *gatewayErrorContract `json:"error"`
}

type simulationBatchResponse struct {
	SchemaVersion          string                  `json:"schema_version"`
	ChainID                uint64                  `json:"chain_id"`
	RequestID              string                  `json:"request_id"`
	BlockNumber            uint64                  `json:"block_number"`
	BlockHash              string                  `json:"block_hash"`
	StateRoot              string                  `json:"state_root"`
	PrimaryProviderID      string                  `json:"primary_provider_id"`
	ConfirmationProviderID *string                 `json:"confirmation_provider_id"`
	Quorum                 uint8                   `json:"quorum"`
	SecondaryProviderID    string                  `json:"-"`
	EvidenceMode           string                  `json:"evidence_mode"`
	Results                []simulationBatchResult `json:"results"`
}

type simulationBatchOutcome struct {
	Response *simulationResponse
	Err      error
}

type executionLeg struct {
	Pool         string `json:"pool"`
	Factory      string `json:"factory"`
	TokenIn      string `json:"token_in"`
	TokenOut     string `json:"token_out"`
	Fee          uint32 `json:"fee"`
	ZeroForOne   bool   `json:"zero_for_one"`
	MinAmountOut string `json:"min_amount_out"`
}

type liquidationRoute struct {
	Name         string
	Output       *big.Int
	OutputDebt   *big.Int
	SelectedPool string
	Factory      string
	SelectedFee  uint32
	ZeroForOne   bool
	TokenPath    []string
	TokenIn      string
	TokenOut     string
}

type liquidationEvaluation struct {
	Liquidation          *exactLiquidation
	Route                liquidationRoute
	Simulation           *simulationResponse
	Expected             *big.Int
	Conservative         *big.Int
	RiskReserve          *big.Int
	ExecutionCost        *big.Int
	EstimatedL1Cost      *big.Int
	MinimumCollateral    string
	MinimumUnwind        string
	MinimumProfit        string
	MinimumProfitWETH    string
	LiveMaximumInput     string
	LiveMaximumInputWETH string
}

type liquidationProbe struct {
	Liquidation       *exactLiquidation
	Route             liquidationRoute
	ExactEdge         *big.Int
	MinimumCollateral string
}

type aaveRoutePayload struct {
	Borrower                  string `json:"borrower"`
	DebtAsset                 string `json:"debt_asset"`
	CollateralAsset           string `json:"collateral_asset"`
	DebtAssetDecimals         uint8  `json:"debt_asset_decimals"`
	DebtAssetPriceBase        string `json:"debt_asset_price_base"`
	WETHPriceBase             string `json:"weth_price_base"`
	MaximumInputWETHWei       string `json:"maximum_input_weth_wei"`
	MinimumProfitWETHWei      string `json:"minimum_profit_weth_wei"`
	ReceiveAToken             bool   `json:"receive_a_token"`
	MinimumCollateralReceived string `json:"minimum_collateral_received"`
	MinimumUnwindOutput       string `json:"minimum_unwind_output"`
	MaximumAtlasBid           string `json:"maximum_atlas_bid"`
	EvidenceMode              string `json:"evidence_mode"`
	StateRoot                 string `json:"state_root"`
	ReleaseSHA                string `json:"release_sha"`
}

type executionCandidate struct {
	RequestID            string
	OpportunityID        string
	RouteID              string
	RoutePayload         aaveRoutePayload
	SelectedSize         string
	TokenPath            []string
	OriginRouter         string
	ExecutorAddress      string
	ExecutorCodeHash     string
	CalldataHash         string
	SimulationResultHash string
	PlanHash             string
	PinnedBlockNumber    uint64
	PinnedBlockHash      string
	FlashAsset           string
	FlashAmount          string
	MaximumInputAmount   string
	MinimumProfit        string
	ExpectedProfit       string
	Deadline             time.Time
	Legs                 []executionLeg
	GasLimit             uint64
	MaxFeePerGas         string
	MaxPriorityFeePerGas string
	ApprovedBy           string
	ApprovedAt           time.Time
	ApprovalDeadline     time.Time
	PolicyVersion        string
	ApprovalDigest       string
}

type approvalBody struct {
	SchemaVersion        string           `json:"schema_version"`
	RequestID            string           `json:"request_id"`
	OpportunityID        string           `json:"opportunity_id"`
	ChainID              uint64           `json:"chain_id"`
	RouteID              string           `json:"route_id"`
	RouteFingerprint     string           `json:"route_fingerprint"`
	RouteType            string           `json:"route_type"`
	RoutePayload         aaveRoutePayload `json:"route_payload"`
	SelectedSize         string           `json:"selected_size"`
	TokenPath            []string         `json:"token_path"`
	OriginRouter         string           `json:"origin_router"`
	ExecutorAddress      string           `json:"executor_address"`
	ExecutorCodeHash     string           `json:"executor_code_hash"`
	CalldataHash         string           `json:"calldata_hash"`
	SimulationResultHash string           `json:"simulation_result_hash"`
	PlanHash             string           `json:"plan_hash"`
	PinnedBlockNumber    uint64           `json:"pinned_block_number"`
	PinnedBlockHash      string           `json:"pinned_block_hash"`
	FlashAsset           string           `json:"flash_asset"`
	FlashAmount          string           `json:"flash_amount"`
	MaximumInputAmount   string           `json:"maximum_input_amount"`
	MinimumProfit        string           `json:"minimum_profit"`
	ExpectedProfit       string           `json:"expected_profit"`
	DeadlineUnixSeconds  int64            `json:"deadline_unix_seconds"`
	Legs                 []executionLeg   `json:"legs"`
	GasLimit             uint64           `json:"gas_limit"`
	MaxFeePerGas         string           `json:"max_fee_per_gas"`
	MaxPriorityFeePerGas string           `json:"max_priority_fee_per_gas"`
	ApprovedBy           string           `json:"approved_by"`
	ApprovedAt           string           `json:"approved_at"`
	ApprovalDeadline     string           `json:"approval_deadline"`
	PolicyVersion        string           `json:"policy_version"`
}

func New(config Config) (*Screener, error) {
	if config.ExactStateBudgetPerMinute == 0 {
		config.ExactStateBudgetPerMinute = defaultExactStateBudgetPerMinute
	}
	if config.ExactDiscoveryReservePerMinute == 0 {
		config.ExactDiscoveryReservePerMinute = defaultExactDiscoveryReserve
	}
	if config.ExactDiscoveryReservePerMinute >= config.ExactStateBudgetPerMinute {
		return nil, errors.New("hunter Exact RPC budget is invalid")
	}
	if config.ExactWorkers == 0 {
		config.ExactWorkers = defaultExactWorkers
	}
	if config.ExactWorkers < 1 || config.ExactWorkers > 16 {
		return nil, errors.New("hunter Exact worker count is invalid")
	}
	if !filepath.IsAbs(config.DiscoveryPath) || !filepath.IsAbs(config.StateDir) {
		return nil, errors.New("hunter paths must be absolute")
	}
	if len(config.DiscoverySHA256) != 64 || config.BatchSize < 1 || config.BatchSize > MaximumBatch || config.Pace < time.Second {
		return nil, errors.New("hunter configuration is invalid")
	}
	if value, ok := newBigUint(config.RetainedProfitFloorWei); !ok || value.Sign() <= 0 {
		return nil, errors.New("hunter retained-profit floor is invalid")
	}
	if value, ok := newBigUint(config.MaximumInputAmountWei); !ok || value.Sign() <= 0 {
		return nil, errors.New("hunter maximum input is invalid")
	}
	if _, ok := newBigUint(config.MaximumAtlasBidWei); !ok {
		return nil, errors.New("hunter maximum Atlas bid is invalid")
	}
	if value, ok := newBigUint(config.MaximumFeePerGasWei); !ok || value.Sign() <= 0 || config.MaximumGasLimit == 0 || config.FlashPremiumBPS == 0 || config.FlashPremiumBPS > 100 || config.EconomicReserveBPS == 0 || config.EconomicReserveBPS > 5_000 {
		return nil, errors.New("hunter economic limits are invalid")
	}
	if !addressPattern.MatchString(config.ExecutorAddress) || !addressPattern.MatchString(config.CallerAddress) || len(config.ExecutorCodeHash) != 64 || !releaseSHAPattern.MatchString(config.ReleaseSHA) {
		return nil, errors.New("hunter execution identity is invalid")
	}
	if value, ok := newBigUint(config.MaximumPriorityFeeWei); !ok || value.Sign() <= 0 {
		return nil, errors.New("hunter priority fee is invalid")
	}
	if !strings.HasPrefix(config.GatewayURL, "http://rpc-gateway:") {
		return nil, errors.New("hunter gateway must use the internal RPC Gateway")
	}
	if err := os.MkdirAll(config.StateDir, 0o700); err != nil {
		return nil, err
	}
	if digest, err := fileSHA256(config.DiscoveryPath); err != nil || digest != config.DiscoverySHA256 {
		return nil, errors.New("immutable discovery seed hash mismatch")
	}
	state := State{Schema: StateSchema, DiscoverySHA256: config.DiscoverySHA256, Cursor: config.StartingCursor, Counts: map[string]uint64{}}
	s := &Screener{
		config: config, client: &http.Client{Timeout: 35 * time.Second},
		batchClient: &http.Client{Timeout: aaveSimulationBatchTimeout}, state: state,
		debtBearing: make(map[string]bool), refreshKnown: make(map[string]bool),
		hotBorrowers: make(map[string]string), hotDebtBase: make(map[string]string),
		hotUpperPositive:       make(map[string]bool),
		latestOutcome:          make(map[string]string),
		lastExactAt:            make(map[string]time.Time),
		firstLiquidatableAt:    make(map[string]time.Time),
		firstLiquidatableMono:  make(map[string]time.Time),
		exactInFlightBorrowers: make(map[string]bool),
		exactWorkerSlots:       make(chan struct{}, config.ExactWorkers),
		wait:                   waitContext,
		now:                    func() time.Time { return time.Now().UTC() },
	}
	if err := s.loadState(); err != nil {
		return nil, err
	}
	if err := s.loadBorrowerIndex(); err != nil {
		return nil, err
	}
	if err := s.loadHotSignals(); err != nil {
		return nil, err
	}
	s.mu.Lock()
	s.initializeExactBudgetLocked(s.nowUTC())
	s.mu.Unlock()
	return s, nil
}

func (s *Screener) Run(ctx context.Context) error {
	addresses, err := streamBorrowers(s.config.DiscoveryPath)
	if err != nil {
		return err
	}
	defer addresses.Close()
	for index := uint64(0); index < s.state.Cursor; index++ {
		if _, err := addresses.Next(); err != nil {
			return fmt.Errorf("resume cursor exceeds discovery seed: %w", err)
		}
	}
	if s.Snapshot().LastBatchAt == nil {
		if err := s.waitForGatewayStartup(ctx); err != nil {
			if gateErr := s.recordError("rpc_gateway_not_ready"); gateErr != nil {
				return gateErr
			}
			return err
		}
	}
	seedComplete := false
	var pendingBatch []string
	pendingAdvanceSeed := false
	for {
		if wait := s.ProviderCircuitWaitDuration(); wait > 0 {
			if err := s.recordProviderCircuitSkip(); err != nil {
				return err
			}
			if !s.waitDuration(ctx, wait) {
				return nil
			}
			continue
		}
		if len(pendingBatch) == 0 {
			pendingBatch = make([]string, 0, s.config.BatchSize)
			for !seedComplete && len(pendingBatch) < s.config.BatchSize {
				address, nextErr := addresses.Next()
				if errors.Is(nextErr, io.EOF) {
					seedComplete = true
					break
				}
				if nextErr != nil {
					return nextErr
				}
				pendingBatch = append(pendingBatch, address)
			}
			pendingAdvanceSeed = len(pendingBatch) > 0
			if seedComplete && len(pendingBatch) == 0 {
				pendingBatch = s.nextRefreshBatch()
			}
		}
		if len(pendingBatch) > 0 {
			attempt := func() error { return s.screen(ctx, pendingBatch, pendingAdvanceSeed, nil) }
			var screenErr error
			if s.Snapshot().LastBatchAt == nil {
				screenErr = s.screenWithStartupRetry(ctx, attempt)
			} else {
				screenErr = attempt()
			}
			if screenErr != nil {
				if ctx.Err() != nil {
					return nil
				}
				retryable, recordErr := s.RecordRetryableGatewayError(screenErr)
				if recordErr != nil {
					return recordErr
				}
				if !retryable {
					if gateErr := s.recordError(gatewayErrorClass(screenErr, "rpc_gateway_screen_failure")); gateErr != nil {
						return gateErr
					}
					return screenErr
				}
				continue
			}
			pendingBatch = nil
			pendingAdvanceSeed = false
		}
		if err := s.runPriorityRecheckWindow(ctx, s.config.Pace); err != nil {
			if errors.Is(err, errPriorityWindowStopped) {
				return nil
			}
			if ctx.Err() != nil {
				return nil
			}
			retryable, recordErr := s.RecordRetryableGatewayError(err)
			if recordErr != nil {
				return recordErr
			}
			if !retryable {
				if gateErr := s.recordError(gatewayErrorClass(err, "rpc_gateway_priority_failure")); gateErr != nil {
					return gateErr
				}
				return err
			}
			continue
		}
	}
}

func (s *Screener) runPriorityRecheckWindow(ctx context.Context, window time.Duration) error {
	if window <= 0 {
		return nil
	}
	if _, err := s.runTailPriority(ctx); err != nil {
		return err
	}
	elapsed := time.Duration(0)
	nextTailAt := hotRevisitCadence
	nextHotAt := hotRevisitCadence
	for elapsed < window {
		waitFor := window - elapsed
		if nextTailAt < window && nextTailAt-elapsed < waitFor {
			waitFor = nextTailAt - elapsed
		}
		if nextHotAt < window && nextHotAt-elapsed < waitFor {
			waitFor = nextHotAt - elapsed
		}
		if eligibilityDelay, present := s.nextExactEligibilityWakeDelay(); present && eligibilityDelay < waitFor {
			waitFor = eligibilityDelay
		}
		if waitFor > 0 && !s.waitDuration(ctx, waitFor) {
			return errPriorityWindowStopped
		}
		elapsed += waitFor
		if elapsed >= window {
			break
		}

		tailDue := elapsed >= nextTailAt
		if tailDue {
			if _, err := s.runTailPriority(ctx); err != nil {
				return err
			}
			for nextTailAt <= elapsed {
				nextTailAt += hotRevisitCadence
			}
		}

		periodicHotDue := elapsed >= nextHotAt
		hotDue := periodicHotDue
		if !hotDue {
			eligibilityDelay, present := s.nextExactEligibilityWakeDelay()
			hotDue = present && eligibilityDelay <= 0
		}
		if hotDue {
			if _, err := s.runHotPriority(ctx); err != nil {
				return err
			}
			for nextHotAt <= elapsed {
				nextHotAt += hotRevisitCadence
			}
			if !periodicHotDue {
				// An eligibility wake consumes the next ordinary hot poll rather
				// than adding a provider request to the priority window.
				nextHotAt += hotRevisitCadence
			}
		}
	}
	return nil
}

// nextExactEligibilityWakeDelay returns the earliest local cooldown boundary
// for work that can use an Exact worker. It deliberately does not poll a
// provider or widen the durable state-request budget; it only wakes the
// existing hot screen at the point where local eligibility changes.
func (s *Screener) nextExactEligibilityWakeDelay() (time.Duration, bool) {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := s.nowUTC()
	providerBlocked := (s.state.LastErrorClass != "" && s.state.LastErrorClass != revenueLaneAuthorityDivergedClass) ||
		s.providerCircuitIsOpenLocked(now)
	if providerBlocked || s.state.LastErrorClass == revenueLaneAuthorityDivergedClass {
		return 0, false
	}
	budgetBlocked := s.exactAdmissionBlockedLocked(now)
	earliest := time.Time{}
	for borrower, hf := range s.hotBorrowers {
		if s.exactInFlightBorrowers[borrower] || classify(s.hotDebtBase[borrower], hf) != "liquidatable" ||
			!s.hotUpperPositive[borrower] || s.state.RouteIneligible[borrower] == "no_weth_debt" {
			continue
		}
		completedAt, served := s.lastExactAt[borrower]
		eligibleAt := exactEligibleSince(s.firstLiquidatableAt[borrower], completedAt, served)
		if served && now.Before(completedAt) {
			// Match screen(): a future completion timestamp does not establish
			// a valid elapsed cooldown after clock rollback.
			eligibleAt = time.Time{}
		}
		if eligibleAt.IsZero() || !now.Before(eligibleAt) {
			if !budgetBlocked {
				return 0, true
			}
			continue
		}
		if earliest.IsZero() || eligibleAt.Before(earliest) {
			earliest = eligibleAt
		}
	}
	if earliest.IsZero() {
		return 0, false
	}
	return earliest.Sub(now), true
}

func (s *Screener) runTailPriority(ctx context.Context) (bool, error) {
	tailBorrowers, err := s.pollTail(ctx)
	if err != nil {
		return false, err
	}
	if len(tailBorrowers) > 0 {
		for offset := 0; offset < len(tailBorrowers); offset += MaximumBatch {
			end := offset + MaximumBatch
			if end > len(tailBorrowers) {
				end = len(tailBorrowers)
			}
			if err := s.screen(ctx, tailBorrowers[offset:end], false, nil); err != nil {
				return false, err
			}
		}
		return true, nil
	}
	return false, nil
}

func (s *Screener) runHotPriority(ctx context.Context) (bool, error) {
	hot := s.nextHotBatch()
	if len(hot) == 0 {
		return false, nil
	}
	s.mu.Lock()
	if s.state.Counts == nil {
		s.state.Counts = make(map[string]uint64)
	}
	s.state.Counts[hotRecheckTotalKey]++
	s.mu.Unlock()
	if err := s.screen(ctx, hot, false, nil); err != nil {
		class := gatewayErrorClass(err, "")
		if strings.Contains(class, "budget") || strings.Contains(class, "rate_limited") {
			s.mu.Lock()
			s.state.Counts[hotRecheckDeferredBudgetKey]++
			s.mu.Unlock()
		}
		return false, err
	}
	return true, nil
}

func (s *Screener) Snapshot() State {
	s.mu.Lock()
	defer s.mu.Unlock()
	copy := s.state
	now := s.nowUTC()
	providerBlocked := (copy.LastErrorClass != "" && copy.LastErrorClass != revenueLaneAuthorityDivergedClass) ||
		(copy.ProviderCircuitOpenUntilUnixMillis > 0 && copy.ProviderCircuitOpenUntilUnixMillis > now.UnixMilli())
	authorityBlocked := copy.LastErrorClass == revenueLaneAuthorityDivergedClass
	copy.HotQueueSize = uint64(len(s.hotBorrowers))
	copy.LiquidatableHotCount = 0
	copy.UrgentHotCount = 0
	copy.ExactEvaluationsInFlight = s.exactInFlight
	copy.ExactWorkersRunning = uint64(len(s.exactWorkerSlots))
	copy.ExactWorkerQueueDepth = copy.ExactEvaluationsInFlight - min(copy.ExactEvaluationsInFlight, copy.ExactWorkersRunning)
	for borrower, hf := range s.hotBorrowers {
		if s.state.RouteIneligible[borrower] != "" {
			copy.RouteIneligibleCount++
		}
		switch classify(s.hotDebtBase[borrower], hf) {
		case "liquidatable":
			copy.LiquidatableHotCount++
			if s.latestOutcome[borrower] == "fork_pending" {
				copy.ActiveForkPendingCount++
			}
			if !s.hotUpperPositive[borrower] {
				continue
			}
			if s.exactInFlightBorrowers[borrower] {
				continue
			}
			routeReason := s.state.RouteIneligible[borrower]
			if routeReason == "no_weth_debt" {
				continue
			}
			if completedAt, present := s.lastExactAt[borrower]; present &&
				!now.Before(completedAt) && now.Sub(completedAt) < exactBorrowerCooldown {
				copy.CooldownBlockedCount++
				continue
			}
			if providerBlocked {
				copy.ProviderBlockedCount++
				continue
			}
			if authorityBlocked {
				copy.AuthorityBlockedCount++
				continue
			}
			if s.exactAdmissionBlockedLocked(now) {
				copy.SchedulerBlockedCount++
				continue
			}
			copy.ExactEligibleNowCount++
			completedAt, served := s.lastExactAt[borrower]
			eligibleSince := exactEligibleSince(s.firstLiquidatableAt[borrower], completedAt, served)
			if !eligibleSince.IsZero() && !now.Before(eligibleSince) {
				age := uint64(now.Sub(eligibleSince) / time.Millisecond)
				if age > copy.OldestExactEligibleAgeMillis {
					copy.OldestExactEligibleAgeMillis = age
				}
			}
		case "urgent":
			copy.UrgentHotCount++
		}
	}
	copy.Counts = make(map[string]uint64, len(s.state.Counts))
	for key, value := range s.state.Counts {
		copy.Counts[key] = value
	}
	copy.RouteIneligible = make(map[string]string, len(s.state.RouteIneligible))
	for borrower, reason := range s.state.RouteIneligible {
		copy.RouteIneligible[borrower] = reason
	}
	return copy
}

var exactLatencyBucketsMillis = [...]uint64{1, 5, 10, 25, 50, 100, 250, 500, 1_000, 2_000, 2_500, 5_000, 10_000, 15_000, 30_000, 60_000, 120_000, 300_000}

func (s *Screener) observeDurationLocked(sumKey, countKey, bucketPrefix string, duration time.Duration) {
	if s.state.Counts == nil {
		s.state.Counts = make(map[string]uint64)
	}
	millis := uint64(0)
	if duration > 0 {
		millis = uint64(duration / time.Millisecond)
	}
	s.state.Counts[sumKey] += millis
	s.state.Counts[countKey]++
	for _, boundary := range exactLatencyBucketsMillis {
		if millis <= boundary {
			s.state.Counts[fmt.Sprintf("%s%d", bucketPrefix, boundary)]++
		}
	}
}

func (s *Screener) MetricsText() string {
	state := s.Snapshot()
	workerCapacity := s.config.ExactWorkers
	if workerCapacity == 0 {
		workerCapacity = defaultExactWorkers
	}
	workerPermitsAvailable := max(workerCapacity-int(state.ExactWorkersRunning), 0)
	lines := []string{
		"# TYPE phoenix_aave_hot_queue_size gauge",
		fmt.Sprintf("phoenix_aave_hot_queue_size %d", state.HotQueueSize),
		"# TYPE phoenix_aave_liquidatable_hot_count gauge",
		fmt.Sprintf("phoenix_aave_liquidatable_hot_count %d", state.LiquidatableHotCount),
		"# TYPE phoenix_aave_urgent_hot_count gauge",
		fmt.Sprintf("phoenix_aave_urgent_hot_count %d", state.UrgentHotCount),
		"# TYPE phoenix_aave_exact_eligible_now gauge",
		fmt.Sprintf("phoenix_aave_exact_eligible_now %d", state.ExactEligibleNowCount),
		"# TYPE phoenix_aave_exact_scheduler_blocked gauge",
		fmt.Sprintf("phoenix_aave_exact_scheduler_blocked %d", state.SchedulerBlockedCount),
		"# TYPE phoenix_aave_exact_cooldown_blocked gauge",
		fmt.Sprintf("phoenix_aave_exact_cooldown_blocked %d", state.CooldownBlockedCount),
		"# TYPE phoenix_aave_route_ineligible_current gauge",
		fmt.Sprintf("phoenix_aave_route_ineligible_current %d", state.RouteIneligibleCount),
		"# TYPE phoenix_aave_exact_provider_blocked gauge",
		fmt.Sprintf("phoenix_aave_exact_provider_blocked %d", state.ProviderBlockedCount),
		"# TYPE phoenix_aave_exact_authority_blocked gauge",
		fmt.Sprintf("phoenix_aave_exact_authority_blocked %d", state.AuthorityBlockedCount),
		"# TYPE phoenix_aave_exact_evaluations_in_flight gauge",
		fmt.Sprintf("phoenix_aave_exact_evaluations_in_flight %d", state.ExactEvaluationsInFlight),
		"# TYPE phoenix_aave_exact_worker_capacity gauge",
		fmt.Sprintf("phoenix_aave_exact_worker_capacity %d", workerCapacity),
		"# TYPE phoenix_aave_exact_workers_running gauge",
		fmt.Sprintf("phoenix_aave_exact_workers_running %d", state.ExactWorkersRunning),
		"# TYPE phoenix_exact_workers_in_flight gauge",
		fmt.Sprintf("phoenix_exact_workers_in_flight %d", state.ExactWorkersRunning),
		"# TYPE phoenix_exact_worker_permits_available gauge",
		fmt.Sprintf("phoenix_exact_worker_permits_available %d", workerPermitsAvailable),
		"# TYPE phoenix_aave_exact_worker_queue_depth gauge",
		fmt.Sprintf("phoenix_aave_exact_worker_queue_depth %d", state.ExactWorkerQueueDepth),
		"# TYPE phoenix_exact_queue_depth gauge",
		fmt.Sprintf("phoenix_exact_queue_depth %d", state.ExactWorkerQueueDepth),
		"# TYPE phoenix_aave_oldest_exact_eligible_age_ms gauge",
		fmt.Sprintf("phoenix_aave_oldest_exact_eligible_age_ms %d", state.OldestExactEligibleAgeMillis),
		"# TYPE phoenix_exact_oldest_actionable_age_seconds gauge",
		fmt.Sprintf("phoenix_exact_oldest_actionable_age_seconds %g", float64(state.OldestExactEligibleAgeMillis)/1_000),
		"# TYPE phoenix_aave_active_fork_pending gauge",
		fmt.Sprintf("phoenix_aave_active_fork_pending %d", state.ActiveForkPendingCount),
		"# TYPE phoenix_aave_exact_queue_ledger_entries_total counter",
		fmt.Sprintf("phoenix_aave_exact_queue_ledger_entries_total %d", state.ExactQueueCount),
		"# TYPE phoenix_aave_hot_recheck_total counter",
		fmt.Sprintf("phoenix_aave_hot_recheck_total %d", state.Counts[hotRecheckTotalKey]),
		"# TYPE phoenix_aave_hot_recheck_deferred_budget_total counter",
		fmt.Sprintf("phoenix_aave_hot_recheck_deferred_budget_total %d", state.Counts[hotRecheckDeferredBudgetKey]),
		"# TYPE phoenix_aave_exact_eval_started_total counter",
		fmt.Sprintf("phoenix_aave_exact_eval_started_total %d", state.Counts[exactEvalStartedKey]),
		"# TYPE phoenix_aave_exact_worker_started_total counter",
		fmt.Sprintf("phoenix_aave_exact_worker_started_total %d", s.exactWorkerStarts.Load()),
		"# TYPE phoenix_aave_exact_eval_completed_total counter",
		fmt.Sprintf("phoenix_aave_exact_eval_completed_total %d", state.Counts[exactEvalCompletedKey]),
		"# TYPE phoenix_aave_route_ineligible_rechecks_total counter",
		fmt.Sprintf("phoenix_aave_route_ineligible_rechecks_total %d", state.Counts[routeIneligibleRechecksKey]),
		"# TYPE phoenix_aave_provider_circuit_deferrals_total counter",
		fmt.Sprintf("phoenix_aave_provider_circuit_deferrals_total %d", state.ProviderCircuitSkippedTotal),
		"# TYPE phoenix_atlas_callback_evidence_unavailable_total counter",
		fmt.Sprintf("phoenix_atlas_callback_evidence_unavailable_total %d", state.Counts[atlasCallbackUnavailableKey]),
		"# TYPE phoenix_exact_coalesced_total counter",
		fmt.Sprintf("phoenix_exact_coalesced_total %d", state.Counts[exactCoalescedKey]),
		"# TYPE phoenix_exact_stale_invalidated_total counter",
		fmt.Sprintf("phoenix_exact_stale_invalidated_total %d", state.Counts[exactStaleInvalidatedKey]),
		"# TYPE phoenix_exact_duplicate_suppressed_total counter",
		fmt.Sprintf("phoenix_exact_duplicate_suppressed_total %d", state.Counts[exactDuplicateSuppressedKey]),
		"# TYPE phoenix_exact_provider_blocked_total counter",
		fmt.Sprintf("phoenix_exact_provider_blocked_total %d", state.ProviderCircuitSkippedTotal),
		"# TYPE phoenix_exact_scheduler_blocked_total counter",
		fmt.Sprintf("phoenix_exact_scheduler_blocked_total %d", state.Counts[exactDeferredSchedulerKey]),
		"# TYPE phoenix_exact_overload_total counter",
		fmt.Sprintf("phoenix_exact_overload_total %d", state.Counts[hotRecheckDeferredBudgetKey]+state.Counts[exactDeferredSchedulerKey]),
	}
	appendHistogram := func(name, sumKey, countKey, bucketPrefix string) {
		lines = append(lines, fmt.Sprintf("# TYPE %s histogram", name))
		for _, boundary := range exactLatencyBucketsMillis {
			lines = append(lines, fmt.Sprintf(
				"%s_bucket{le=\"%d\"} %d",
				name,
				boundary,
				state.Counts[fmt.Sprintf("%s%d", bucketPrefix, boundary)],
			))
		}
		lines = append(lines,
			fmt.Sprintf("%s_bucket{le=\"+Inf\"} %d", name, state.Counts[countKey]),
			fmt.Sprintf("%s_sum %d", name, state.Counts[sumKey]),
			fmt.Sprintf("%s_count %d", name, state.Counts[countKey]),
		)
	}
	appendHistogram("phoenix_aave_exact_eval_latency_ms", exactEvalLatencySumKey, exactEvalLatencyCountKey, "exact_eval_latency_millis_bucket_le_")
	appendHistogram("phoenix_aave_first_liquidatable_to_exact_eval_ms", liquidatableToExactSumKey, liquidatableToExactCountKey, "liquidatable_to_exact_millis_bucket_le_")
	appendSecondsHistogram := func(name, sumKey, countKey, bucketPrefix string) {
		lines = append(lines, fmt.Sprintf("# TYPE %s histogram", name))
		for _, boundary := range exactLatencyBucketsMillis {
			lines = append(lines, fmt.Sprintf(
				"%s_bucket{le=\"%g\"} %d",
				name,
				float64(boundary)/1_000,
				state.Counts[fmt.Sprintf("%s%d", bucketPrefix, boundary)],
			))
		}
		lines = append(lines,
			fmt.Sprintf("%s_bucket{le=\"+Inf\"} %d", name, state.Counts[countKey]),
			fmt.Sprintf("%s_sum %g", name, float64(state.Counts[sumKey])/1_000),
			fmt.Sprintf("%s_count %d", name, state.Counts[countKey]),
		)
	}
	appendSecondsHistogram("phoenix_aave_liquidatable_to_exact_admission_seconds", exactEligibilityLatencySumKey, exactEligibilityLatencyCountKey, "exact_eligibility_latency_millis_bucket_le_")
	appendSecondsHistogram("phoenix_aave_exact_worker_queue_seconds", exactQueueLatencySumKey, exactQueueLatencyCountKey, "exact_queue_latency_millis_bucket_le_")
	appendSecondsHistogram("phoenix_aave_exact_worker_to_gateway_seconds", exactDispatchLatencySumKey, exactDispatchLatencyCountKey, "exact_dispatch_latency_millis_bucket_le_")
	appendSecondsHistogram("phoenix_aave_exact_initial_gateway_response_seconds", exactInitialLatencySumKey, exactInitialLatencyCountKey, "exact_initial_response_latency_millis_bucket_le_")
	appendSecondsHistogram("phoenix_aave_exact_end_to_end_seconds", liquidatableToExactSumKey, liquidatableToExactCountKey, "liquidatable_to_exact_millis_bucket_le_")
	appendSecondsHistogram("phoenix_signal_to_prefilter_seconds", signalPrefilterLatencySumKey, signalPrefilterLatencyCountKey, "signal_prefilter_latency_millis_bucket_le_")
	appendSecondsHistogram("phoenix_liquidatable_to_exact_enqueue_seconds", exactEligibilityLatencySumKey, exactEligibilityLatencyCountKey, "exact_eligibility_latency_millis_bucket_le_")
	appendSecondsHistogram("phoenix_exact_queue_wait_seconds", exactQueueLatencySumKey, exactQueueLatencyCountKey, "exact_queue_latency_millis_bucket_le_")
	appendSecondsHistogram("phoenix_exact_worker_dispatch_seconds", exactDispatchLatencySumKey, exactDispatchLatencyCountKey, "exact_dispatch_latency_millis_bucket_le_")
	appendSecondsHistogram("phoenix_exact_first_rpc_dispatch_seconds", exactFirstRPCLatencySumKey, exactFirstRPCLatencyCountKey, "exact_first_rpc_latency_millis_bucket_le_")
	appendSecondsHistogram("phoenix_exact_rpc_state_fetch_seconds", exactInitialLatencySumKey, exactInitialLatencyCountKey, "exact_initial_response_latency_millis_bucket_le_")
	appendSecondsHistogram("phoenix_exact_compute_seconds", exactComputeLatencySumKey, exactComputeLatencyCountKey, "exact_compute_latency_millis_bucket_le_")
	appendSecondsHistogram("phoenix_exact_end_to_end_seconds", liquidatableToExactSumKey, liquidatableToExactCountKey, "liquidatable_to_exact_millis_bucket_le_")
	appendSecondsHistogram("phoenix_fork_queue_wait_seconds", exactForkQueueSumKey, exactForkQueueCountKey, "exact_fork_queue_millis_bucket_le_")
	appendSecondsHistogram("phoenix_fork_runtime_seconds", exactForkRuntimeSumKey, exactForkRuntimeCountKey, "exact_fork_runtime_millis_bucket_le_")
	return strings.Join(lines, "\n") + "\n"
}

func waitContext(ctx context.Context, delay time.Duration) bool {
	timer := time.NewTimer(delay)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C:
		return true
	}
}

func (s *Screener) waitDuration(ctx context.Context, delay time.Duration) bool {
	if s.wait != nil {
		return s.wait(ctx, delay)
	}
	return waitContext(ctx, delay)
}

func (s *Screener) waitForGatewayStartup(ctx context.Context) error {
	startupCtx, cancel := context.WithTimeout(ctx, gatewayReadyTimeout)
	defer cancel()
	for {
		req, err := http.NewRequestWithContext(startupCtx, http.MethodGet, s.config.GatewayURL+"/readyz", nil)
		if err != nil {
			return err
		}
		response, err := s.client.Do(req)
		if err == nil {
			response.Body.Close()
			if response.StatusCode == http.StatusOK {
				if !s.waitDuration(startupCtx, initialScreenOffset) {
					return startupCtx.Err()
				}
				return nil
			}
		}
		if !s.waitDuration(startupCtx, gatewayReadyPoll) {
			return startupCtx.Err()
		}
	}
}

func (s *Screener) screenWithStartupRetry(ctx context.Context, attempt func() error) error {
	retryCtx, cancel := context.WithTimeout(ctx, startupRetryTimeout)
	defer cancel()
	backoff := [...]time.Duration{10 * time.Second, 20 * time.Second, 30 * time.Second}
	var lastErr error
	for retry := 0; retry <= maximumStartupRetries; retry++ {
		s.recordStartupAttempt(uint64(retry), "")
		lastErr = attempt()
		if lastErr == nil {
			s.clearStartupError()
			return nil
		}
		class := gatewayErrorClass(lastErr, "rpc_gateway_screen_failure")
		s.recordStartupAttempt(uint64(retry), class)
		gatewayErr, retryable := retryableStartupError(lastErr)
		if !retryable || retry == maximumStartupRetries {
			return lastErr
		}
		delay := backoff[retry]
		if gatewayErr.retryAfter > delay {
			delay = gatewayErr.retryAfter
		}
		if delay > 30*time.Second {
			delay = 30 * time.Second
		}
		if !s.waitDuration(retryCtx, delay) {
			if retryCtx.Err() != nil {
				return retryCtx.Err()
			}
			return lastErr
		}
	}
	return lastErr
}

func retryableStartupError(err error) (*gatewayResponseError, bool) {
	var gatewayErr *gatewayResponseError
	if !errors.As(err, &gatewayErr) || !gatewayErr.retryable {
		return nil, false
	}
	retryable := gatewayErr.statusCode == http.StatusTooManyRequests && gatewayErr.class == "upstream_call_budget_exhausted" ||
		gatewayErr.statusCode == http.StatusServiceUnavailable && gatewayErr.class == "provider_unavailable"
	return gatewayErr, retryable
}

func (s *Screener) nowUTC() time.Time {
	if s.now != nil {
		return s.now()
	}
	return time.Now().UTC()
}

func (s *Screener) providerCircuitIsOpenLocked(now time.Time) bool {
	return s.state.ProviderCircuitOpenUntilUnixMillis > 0 && s.state.ProviderCircuitOpenUntilUnixMillis > now.UnixMilli()
}

func (s *Screener) ProviderCircuitWaitDuration() time.Duration {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := s.nowUTC()
	if !s.providerCircuitIsOpenLocked(now) {
		return 0
	}
	return time.Duration(s.state.ProviderCircuitOpenUntilUnixMillis-now.UnixMilli()) * time.Millisecond
}

func (s *Screener) IsProviderCircuitOpen() bool {
	return s.ProviderCircuitWaitDuration() > 0
}

func (s *Screener) recordProviderCircuitSkip() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.state.ProviderCircuitSkippedTotal++
	return s.persistStateLocked()
}

func (s *Screener) openProviderCircuit(class string, cooldown time.Duration) error {
	now := s.nowUTC()
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.openProviderCircuitLocked(now, class, cooldown)
}

func (s *Screener) openProviderCircuitLocked(now time.Time, class string, cooldown time.Duration) error {
	if cooldown <= 0 {
		cooldown = providerCircuitCooldown
	}
	if s.state.Counts == nil {
		s.state.Counts = make(map[string]uint64)
	}
	circuitAlreadyOpen := s.providerCircuitIsOpenLocked(now)
	if !circuitAlreadyOpen {
		s.state.ProviderCircuitOpenTotal++
	}
	if !circuitAlreadyOpen || s.state.LastErrorClass != class {
		if s.state.LastErrorClass == class {
			s.state.Counts[providerCurrentFailureStreakKey]++
		} else {
			s.state.Counts[providerCurrentFailureStreakKey] = 1
		}
	}
	s.state.ProviderCircuitOpenUntilUnixMillis = now.Add(cooldown).UnixMilli()
	s.state.ProviderRecoverySamples = nil
	s.state.LastPrimaryExactAt = nil
	s.state.LastProviderSecond = ""
	return s.recordProviderDegradationLocked(now, class)
}

// RecordRetryableGatewayError keeps exact authority fail-closed while applying
// different recovery windows to external provider failures and local Gateway
// budget pressure. Local token-bucket exhaustion is bounded by the Gateway
// itself and must not turn a seconds-long refill into a five-minute outage.
func (s *Screener) RecordRetryableGatewayError(err error) (bool, error) {
	class, cooldown, retryable := retryableProviderError(err)
	if !retryable {
		return false, nil
	}
	if err := s.openProviderCircuit(class, cooldown); err != nil {
		return false, err
	}
	if sink, ok := s.config.SignalSink.(ProviderAuthoritySink); ok {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		if err := sink.RecordProviderFailure(ctx, class, s.nowUTC()); err != nil {
			return false, err
		}
	}
	return true, nil
}

// ResetProviderRecoveryEvidence is called on every process start. Exact
// samples from a previous process can never satisfy a new recovery window.
func (s *Screener) ResetProviderRecoveryEvidence(ctx context.Context) error {
	now := s.nowUTC()
	s.mu.Lock()
	s.state.ProviderRecoverySamples = nil
	s.state.LastPrimaryExactAt = nil
	s.state.LastProviderSecond = ""
	if s.state.LastErrorClass == "" {
		s.state.LastErrorClass = "observer_restart"
	}
	s.state.LastAttemptAt = &now
	err := s.persistStateLocked()
	s.mu.Unlock()
	if err != nil {
		return err
	}
	if sink, ok := s.config.SignalSink.(ProviderAuthoritySink); ok {
		return sink.ResetProviderRecoveryEvidence(ctx, "observer_restart", now)
	}
	return nil
}

func retryableProviderError(err error) (string, time.Duration, bool) {
	if errors.Is(err, context.Canceled) {
		return "", 0, false
	}
	var gatewayErr *gatewayResponseError
	if errors.As(err, &gatewayErr) {
		switch gatewayErr.class {
		case "provider_disagreement":
			return "provider_disagreement", providerCircuitCooldown,
				gatewayErr.statusCode == http.StatusConflict || gatewayErr.statusCode == http.StatusBadGateway
		case "provider_unavailable":
			return "provider_unavailable", providerCircuitCooldown,
				gatewayErr.statusCode == http.StatusServiceUnavailable
		case "provider_timeout", "secondary_timeout":
			return "provider_timeout", providerCircuitCooldown,
				gatewayErr.statusCode == http.StatusRequestTimeout ||
					gatewayErr.statusCode == http.StatusBadGateway ||
					gatewayErr.statusCode == http.StatusServiceUnavailable ||
					gatewayErr.statusCode == http.StatusGatewayTimeout
		case "provider_rate_limited", "secondary_rate_limited":
			return "provider_rate_limited", providerCircuitCooldown,
				gatewayErr.statusCode == http.StatusTooManyRequests ||
					gatewayErr.statusCode == http.StatusServiceUnavailable
		case "state_request_budget_exhausted", "upstream_call_budget_exhausted":
			return "gateway_budget_exhausted", gatewayBudgetCircuitCooldown,
				gatewayErr.statusCode == http.StatusTooManyRequests && gatewayErr.retryable
		default:
			return "", 0, false
		}
	}
	var networkErr net.Error
	if errors.As(err, &networkErr) && networkErr.Timeout() {
		return "provider_timeout", providerCircuitCooldown, true
	}
	return "", 0, false
}
func gatewayErrorClass(err error, fallback string) string {
	var gatewayErr *gatewayResponseError
	if errors.As(err, &gatewayErr) && errorClassPattern.MatchString(gatewayErr.class) {
		return gatewayErr.class
	}
	return fallback
}

func decodeGatewayError(response *http.Response) error {
	var contract gatewayErrorContract
	if err := json.NewDecoder(io.LimitReader(response.Body, maximumResponse)).Decode(&contract); err != nil ||
		!errorClassPattern.MatchString(contract.ErrorClass) {
		return fmt.Errorf("RPC Gateway status %d", response.StatusCode)
	}
	retryAfter := time.Duration(0)
	if contract.RetryAfterSecond != nil && *contract.RetryAfterSecond <= 30 {
		retryAfter = time.Duration(*contract.RetryAfterSecond) * time.Second
	}
	return &gatewayResponseError{
		statusCode: response.StatusCode,
		class:      contract.ErrorClass,
		retryable:  contract.Retryable,
		retryAfter: retryAfter,
	}
}

func (s *Screener) nextRefreshBatch() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	if len(s.refreshOrder) == 0 || len(s.debtBearing) == 0 {
		return nil
	}
	batch := make([]string, 0, s.config.BatchSize)
	visited := 0
	for len(batch) < s.config.BatchSize && visited < len(s.refreshOrder) {
		if s.refreshCursor >= len(s.refreshOrder) {
			s.refreshCursor = 0
		}
		borrower := s.refreshOrder[s.refreshCursor]
		s.refreshCursor++
		visited++
		if s.debtBearing[borrower] {
			batch = append(batch, borrower)
		}
	}
	return batch
}

func (s *Screener) pollTail(ctx context.Context) ([]string, error) {
	s.mu.Lock()
	fromBlock := s.state.TailNextBlock
	s.mu.Unlock()
	requestID := fmt.Sprintf("aave-tail-%d-%d", fromBlock, time.Now().UnixMilli())
	body, _ := json.Marshal(tailRequest{
		SchemaVersion: "phoenix.rpc.aave-tail-request.v1",
		ChainID:       42161,
		RequestID:     requestID,
		FromBlock:     fromBlock,
	})
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, s.config.GatewayURL+"/v1/aave/tail", bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")
	response, err := s.client.Do(req)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return nil, decodeGatewayError(response)
	}
	var result tailResponse
	if err := json.NewDecoder(io.LimitReader(response.Body, maximumResponse)).Decode(&result); err != nil {
		return nil, err
	}
	if result.SchemaVersion != "phoenix.rpc.aave-tail-response.v2" || result.ChainID != 42161 || result.RequestID != requestID || result.FinalizedBlockNumber == 0 || len(result.FinalizedBlockHash) != 66 || result.PrimaryProviderID != primaryProviderID || result.ConfirmationProvider != nil || result.Quorum != 1 || result.NextBlock != result.ToBlock+1 || len(result.Borrowers) > 1024 {
		return nil, errors.New("Aave tail evidence is incomplete")
	}
	if fromBlock == 0 {
		if result.FromBlock != result.FinalizedBlockNumber+1 || result.ToBlock != result.FinalizedBlockNumber || len(result.Borrowers) != 0 {
			return nil, errors.New("Aave tail checkpoint is invalid")
		}
	} else if result.FromBlock != fromBlock || result.ToBlock > result.FinalizedBlockNumber {
		return nil, errors.New("Aave tail range is invalid")
	}
	for index, borrower := range result.Borrowers {
		if !addressPattern.MatchString(borrower) || index > 0 && result.Borrowers[index-1] >= borrower {
			return nil, errors.New("Aave tail borrower identity is invalid")
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.ensureHotMapsLocked()
	if s.state.Counts == nil {
		s.state.Counts = make(map[string]uint64)
	}
	for _, borrower := range result.Borrowers {
		if _, wasHot := s.hotBorrowers[borrower]; wasHot || s.exactInFlightBorrowers[borrower] {
			s.state.Counts[exactStaleInvalidatedKey]++
		}
		if err := s.updateBorrowerActivityLocked(borrower, true); err != nil {
			return nil, err
		}
		// A new Borrow/Repay/Liquidation event is a material state change and
		// immediately invalidates both the short Exact cooldown and any
		// route-ineligibility learned from a prior Exact reserve snapshot.
		delete(s.lastExactAt, borrower)
		delete(s.firstLiquidatableAt, borrower)
		delete(s.firstLiquidatableMono, borrower)
		delete(s.latestOutcome, borrower)
		delete(s.hotUpperPositive, borrower)
		delete(s.state.RouteIneligible, borrower)
		if s.state.TailInvalidatedBlock == nil {
			s.state.TailInvalidatedBlock = make(map[string]uint64)
		}
		s.state.TailInvalidatedBlock[borrower] = result.ToBlock
	}
	now := time.Now().UTC()
	s.state.TailNextBlock = result.NextBlock
	s.state.LastTailAt = &now
	if err := s.persistStateLocked(); err != nil {
		return nil, err
	}
	return result.Borrowers, nil
}

func (s *Screener) HandleAtlasAuction(ctx context.Context, auction *observer.LedgerRecord) error {
	if auction == nil || !auction.RelevantAaveAuction || auction.ChainID != 42161 {
		return nil
	}
	// The gateway currently proves only the direct executeAaveLiquidation
	// wrapper, never Atlas' caller/bid/reconcile callback path. Screening the
	// entire hot cohort for each auction therefore cannot produce authority and
	// only repeats Exact/fork work. Persist that capability rejection directly.
	if sink, ok := s.config.SignalSink.(AtlasAuctionDispositionSink); ok {
		if err := sink.RecordAtlasCallbackUnavailable(ctx, auction.AuctionID, auction.NotificationSHA256); err != nil {
			return err
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.state.Counts == nil {
		s.state.Counts = make(map[string]uint64)
	}
	s.state.Counts[atlasCallbackUnavailableKey]++
	return nil
}

func liquidationPriorityRank(debt, hf string) int {
	switch classify(debt, hf) {
	case "liquidatable":
		return 0
	case "urgent":
		return 1
	case "watch":
		return 2
	default:
		return 3
	}
}

func liquidationPriorityLess(
	leftDebt, leftHF, leftBorrower string,
	rightDebt, rightHF, rightBorrower string,
) bool {
	leftRank := liquidationPriorityRank(leftDebt, leftHF)
	rightRank := liquidationPriorityRank(rightDebt, rightHF)
	if leftRank != rightRank {
		return leftRank < rightRank
	}

	leftHFValue, leftHFOK := newBigUint(leftHF)
	rightHFValue, rightHFOK := newBigUint(rightHF)
	if leftHFOK != rightHFOK {
		return leftHFOK
	}
	if leftHFOK && rightHFOK && leftHFValue.Cmp(rightHFValue) != 0 {
		return leftHFValue.Cmp(rightHFValue) < 0
	}
	return leftBorrower < rightBorrower
}

func prioritizedAccountOrder(accounts []account) []int {
	order := make([]int, len(accounts))
	for index := range accounts {
		order[index] = index
	}
	sort.SliceStable(order, func(i, j int) bool {
		left := accounts[order[i]]
		right := accounts[order[j]]
		return liquidationPriorityLess(
			left.TotalDebtBase,
			left.HealthFactorWAD,
			left.Borrower,
			right.TotalDebtBase,
			right.HealthFactorWAD,
			right.Borrower,
		)
	})
	return order
}

func exactEligibleSince(firstLiquidatableAt, lastExactAt time.Time, served bool) time.Time {
	eligibleSince := firstLiquidatableAt
	if served {
		cooldownEligibleAt := lastExactAt.Add(exactBorrowerCooldown)
		if eligibleSince.Before(cooldownEligibleAt) {
			eligibleSince = cooldownEligibleAt
		}
	}
	return eligibleSince
}

func (s *Screener) initializeExactBudgetLocked(now time.Time) {
	if s.state.ExactAverageStateRequestsMilli == 0 {
		s.state.ExactAverageStateRequestsMilli = defaultExactRequestEstimateMilli
	}
	if s.state.ExactBudgetUpdatedAt == nil || now.Before(s.state.ExactBudgetUpdatedAt.UTC()) {
		s.state.ExactBudgetUpdatedAt = &now
		s.state.ExactBudgetTokensMilli = s.exactBudgetCapacityMilliLocked()
	}
	s.refillExactBudgetLocked(now)
}

func (s *Screener) refillExactBudgetLocked(now time.Time) {
	if s.state.ExactAverageStateRequestsMilli == 0 {
		s.state.ExactAverageStateRequestsMilli = defaultExactRequestEstimateMilli
	}
	if s.state.ExactBudgetUpdatedAt == nil {
		s.state.ExactBudgetUpdatedAt = &now
		s.state.ExactBudgetTokensMilli = s.exactBudgetCapacityMilliLocked()
		return
	}
	last := s.state.ExactBudgetUpdatedAt.UTC()
	if now.Before(last) {
		s.state.ExactBudgetTokensMilli = 0
		s.state.ExactBudgetUpdatedAt = &now
		return
	}
	total := s.config.ExactStateBudgetPerMinute
	reserve := s.config.ExactDiscoveryReservePerMinute
	if total == 0 {
		total = defaultExactStateBudgetPerMinute
	}
	if reserve == 0 {
		reserve = defaultExactDiscoveryReserve
	}
	if reserve >= total {
		reserve = total - 1
	}
	effective := total - reserve
	elapsedMillis := uint64(now.Sub(last) / time.Millisecond)
	added := elapsedMillis * effective * 1_000 / 60_000
	capacity := max(effective*1_000, s.state.ExactAverageStateRequestsMilli)
	s.state.ExactBudgetTokensMilli = min(s.state.ExactBudgetTokensMilli+added, capacity)
	s.state.ExactBudgetUpdatedAt = &now
}

func (s *Screener) exactBudgetCapacityMilliLocked() uint64 {
	total := s.config.ExactStateBudgetPerMinute
	reserve := s.config.ExactDiscoveryReservePerMinute
	if total == 0 {
		total = defaultExactStateBudgetPerMinute
	}
	if reserve == 0 {
		reserve = defaultExactDiscoveryReserve
	}
	if reserve >= total {
		reserve = total - 1
	}
	return max((total-reserve)*1_000, s.state.ExactAverageStateRequestsMilli)
}

func (s *Screener) exactAdmissionBlockedLocked(now time.Time) bool {
	s.refillExactBudgetLocked(now)
	return s.state.ExactBudgetTokensMilli < s.exactReservationMilliLocked()
}

func (s *Screener) admitExactLocked(now time.Time) (uint64, bool) {
	if s.exactAdmissionBlockedLocked(now) {
		return 0, false
	}
	reserved := s.exactReservationMilliLocked()
	s.state.ExactBudgetTokensMilli -= reserved
	s.lastExactAdmissionAt = now
	s.hasDurableAdmission = true
	s.state.LastExactAdmissionAt = &now
	return reserved, true
}

func (s *Screener) exactReservationMilliLocked() uint64 {
	return max(s.state.ExactAverageStateRequestsMilli, defaultExactRequestEstimateMilli)
}

func (s *Screener) settleExactBudgetLocked(reservedMilli, actualRequests uint64) {
	actualMilli := max(actualRequests, 1) * 1_000
	if actualMilli > reservedMilli {
		s.state.ExactBudgetTokensMilli = s.state.ExactBudgetTokensMilli - min(s.state.ExactBudgetTokensMilli, actualMilli-reservedMilli)
	}
	s.state.ExactAverageStateRequestsMilli = (3*s.state.ExactAverageStateRequestsMilli + actualMilli) / 4
}

func (s *Screener) recordExactStateRequest(ctx context.Context) {
	tracker := exactTrackerFromContext(ctx)
	if tracker == nil {
		return
	}
	tracker.requests.Add(1)
	if tracker.firstRequestUnixNanos.CompareAndSwap(0, s.nowUTC().UnixNano()) {
		tracker.firstRequestMonotonic = time.Now()
	}
}

func (s *Screener) schedulerAccountOrder(accounts []account) []int {
	order := prioritizedAccountOrder(accounts)
	sort.SliceStable(order, func(i, j int) bool {
		left := accounts[order[i]]
		right := accounts[order[j]]
		leftRank := liquidationPriorityRank(left.TotalDebtBase, left.HealthFactorWAD)
		rightRank := liquidationPriorityRank(right.TotalDebtBase, right.HealthFactorWAD)
		if leftRank != rightRank || leftRank != 0 {
			return false
		}
		leftCompleted, leftServed := s.lastExactAt[left.Borrower]
		rightCompleted, rightServed := s.lastExactAt[right.Borrower]
		if leftServed != rightServed {
			return !leftServed
		}
		leftEligibleSince := exactEligibleSince(s.firstLiquidatableAt[left.Borrower], leftCompleted, leftServed)
		rightEligibleSince := exactEligibleSince(s.firstLiquidatableAt[right.Borrower], rightCompleted, rightServed)
		if leftEligibleSince.IsZero() != rightEligibleSince.IsZero() {
			// A new account has no epoch until this response is processed. Keep
			// an already-waiting never-served borrower ahead of that arrival.
			return !leftEligibleSince.IsZero()
		}
		if !leftEligibleSince.Equal(rightEligibleSince) {
			return leftEligibleSince.Before(rightEligibleSince)
		}
		return false
	})
	return order
}

func (s *Screener) nextHotBatch() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	type entry struct {
		borrower      string
		hf            string
		debt          string
		rank          int
		served        bool
		eligibleSince time.Time
	}
	now := s.nowUTC()
	entries := make([]entry, 0, len(s.hotBorrowers))
	for borrower, hf := range s.hotBorrowers {
		bucket := classify(s.hotDebtBase[borrower], hf)
		routeReason := s.state.RouteIneligible[borrower]
		rank := 4
		switch bucket {
		case "liquidatable":
			rank = 1
			completedAt, recentlyResolved := s.lastExactAt[borrower]
			cooldownBlocked := recentlyResolved && !now.Before(completedAt) &&
				now.Sub(completedAt) < exactBorrowerCooldown
			if s.hotUpperPositive[borrower] && routeReason != "no_weth_debt" && !cooldownBlocked {
				rank = 0
			} else if routeReason != "" {
				rank = 4
			}
		case "urgent":
			rank = 2
		case "watch":
			rank = 3
		}
		if routeReason == "no_weth_debt" {
			rank = 5
		}
		completedAt, served := s.lastExactAt[borrower]
		eligibleSince := exactEligibleSince(s.firstLiquidatableAt[borrower], completedAt, served)
		entries = append(entries, entry{
			borrower:      borrower,
			hf:            hf,
			debt:          s.hotDebtBase[borrower],
			rank:          rank,
			served:        served,
			eligibleSince: eligibleSince,
		})
	}
	sort.Slice(entries, func(i, j int) bool {
		if entries[i].rank != entries[j].rank {
			return entries[i].rank < entries[j].rank
		}
		if entries[i].rank == 0 {
			if entries[i].served != entries[j].served {
				return !entries[i].served
			}
			if !entries[i].eligibleSince.Equal(entries[j].eligibleSince) {
				if entries[i].eligibleSince.IsZero() {
					// A newly arrived borrower has no durable wait epoch yet.
					// Never let it jump an already-waiting never-served borrower.
					return false
				}
				if entries[j].eligibleSince.IsZero() {
					return true
				}
				return entries[i].eligibleSince.Before(entries[j].eligibleSince)
			}
		}
		return liquidationPriorityLess(
			entries[i].debt,
			entries[i].hf,
			entries[i].borrower,
			entries[j].debt,
			entries[j].hf,
			entries[j].borrower,
		)
	})
	if len(entries) > MaximumBatch {
		entries = entries[:MaximumBatch]
	}
	result := make([]string, len(entries))
	for index := range entries {
		result[index] = entries[index].borrower
	}
	return result
}

func (s *Screener) persistScreenSignalLocked(
	ctx context.Context,
	record signal,
	borrower string,
	bucket string,
	authorityDiverged *bool,
) error {
	if *authorityDiverged && record.TerminalOutcome == "exact_pending" {
		record.ExactDeferredReason = revenueLaneAuthorityDivergedClass
	}
	if *authorityDiverged && (record.ExecutionCandidate != nil || record.AtlasCandidate != nil) {
		record = withoutCandidateAuthority(record, revenueLaneAuthorityDivergedClass)
	}
	sinkRecorded := false
	if s.config.SignalSink != nil && (record.ExecutionCandidate != nil || record.AtlasCandidate != nil) {
		s.mu.Unlock()
		normalized, sinkErr := s.config.SignalSink.RecordAaveSignal(ctx, record)
		s.mu.Lock()
		if sinkErr != nil {
			return sinkErr
		}
		record = normalized
		sinkRecorded = true
		if record.ExactDeferredReason == revenueLaneAuthorityDivergedClass {
			if !*authorityDiverged {
				s.state.Counts[revenueLaneAuthorityDivergedKey]++
			}
			*authorityDiverged = true
		}
	}
	if _, hot := s.hotBorrowers[borrower]; hot {
		if record.ExactDeferredReason == "" || s.latestOutcome[borrower] == "" {
			s.latestOutcome[borrower] = record.TerminalOutcome
		}
	} else {
		delete(s.latestOutcome, borrower)
	}
	if err := appendJSON(filepath.Join(s.config.StateDir, "signals.ndjson"), record); err != nil {
		return err
	}
	if invalidatedBlock := s.state.TailInvalidatedBlock[borrower]; invalidatedBlock != 0 && record.Block >= invalidatedBlock {
		delete(s.state.TailInvalidatedBlock, borrower)
	}
	if s.config.SignalSink != nil && !sinkRecorded {
		s.mu.Unlock()
		_, sinkErr := s.config.SignalSink.RecordAaveSignal(ctx, record)
		s.mu.Lock()
		if sinkErr != nil {
			return sinkErr
		}
	}
	if bucket == "urgent" || record.TerminalOutcome == "fork_pending" {
		if err := appendJSON(filepath.Join(s.config.StateDir, "exact-queue.ndjson"), record); err != nil {
			return err
		}
		s.state.ExactQueueCount++
	}
	s.state.Counts[bucket]++
	return nil
}

func (s *Screener) screen(ctx context.Context, borrowers []string, advanceSeed bool, auction *observer.LedgerRecord) error {
	s.operationMu.Lock()
	defer s.operationMu.Unlock()
	cursor := s.Snapshot().Cursor
	requestID := fmt.Sprintf("aave-%d-%d", cursor, time.Now().UnixMilli())
	body, _ := json.Marshal(screenRequest{SchemaVersion: RequestSchema, ChainID: 42161, RequestID: requestID, Borrowers: borrowers})
	discoveryPath := "/v1/aave/screen"
	if s.config.PrimaryDiscovery {
		discoveryPath = "/v1/aave/screen-primary"
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, s.config.GatewayURL+discoveryPath, bytes.NewReader(body))
	if err != nil {
		return err
	}
	req.Header.Set("Content-Type", "application/json")
	response, err := s.client.Do(req)
	if err != nil {
		return err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return decodeGatewayError(response)
	}
	var screenBlockNumber uint64
	var screenBlockHash string
	var primaryEvidence providerScreen
	if s.config.PrimaryDiscovery {
		var result primaryScreenResponse
		if err := json.NewDecoder(io.LimitReader(response.Body, maximumResponse)).Decode(&result); err != nil {
			return err
		}
		if result.SchemaVersion != PrimaryScreenResponseSchema || result.ChainID != 42161 || result.RequestID != requestID || result.BlockNumber == 0 || len(result.BlockHash) != 66 || result.Primary.ProviderID != primaryProviderID || result.Primary.WETHPriceBase == "0" || len(result.Primary.Accounts) != len(borrowers) {
			return errors.New("gateway Aave primary discovery evidence is incomplete")
		}
		screenBlockNumber, screenBlockHash, primaryEvidence = result.BlockNumber, result.BlockHash, result.Primary
	} else {
		var result screenResponse
		if err := json.NewDecoder(io.LimitReader(response.Body, maximumResponse)).Decode(&result); err != nil {
			return err
		}
		if result.SchemaVersion != ResponseSchema || result.ChainID != 42161 || result.RequestID != requestID || result.BlockNumber == 0 || len(result.BlockHash) != 66 || result.Primary.ProviderID != primaryProviderID || result.Primary.WETHPriceBase == "0" || len(result.Primary.Accounts) != len(borrowers) || result.Confirmation != nil || result.Quorum != 1 {
			return errors.New("gateway Aave evidence is incomplete")
		}
		screenBlockNumber, screenBlockHash, primaryEvidence = result.BlockNumber, result.BlockHash, result.Primary
	}
	previousAuthorityDiverged := s.Snapshot().LastErrorClass == revenueLaneAuthorityDivergedClass
	authorityDiverged := false
	if _, hasDurableAuthority := s.config.SignalSink.(LiveSizeAuthority); hasDurableAuthority {
		if _, authorityErr := s.currentAaveLiveMaximumInputAmount(ctx); authorityErr != nil {
			if !errors.Is(authorityErr, errRevenueLaneAuthorityDiverged) {
				return authorityErr
			}
			authorityDiverged = true
		}
	}
	if authorityDiverged {
		if sink, ok := s.config.SignalSink.(ProviderAuthoritySink); ok {
			if err := sink.RecordProviderFailure(ctx, revenueLaneAuthorityDivergedClass, s.nowUTC()); err != nil {
				return err
			}
		}
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	if s.config.ExactWorkers == 0 {
		s.config.ExactWorkers = defaultExactWorkers
	}
	if s.exactWorkerSlots == nil {
		s.exactWorkerSlots = make(chan struct{}, s.config.ExactWorkers)
	}
	if s.exactInFlightBorrowers == nil {
		s.exactInFlightBorrowers = make(map[string]bool)
	}
	if s.firstLiquidatableMono == nil {
		s.firstLiquidatableMono = make(map[string]time.Time)
	}
	s.ensureHotMapsLocked()
	if previousAuthorityDiverged && !authorityDiverged {
		// The control pair is coherent again, so discovery can be healthy, but
		// only three subsequent authority-bearing Exact samples may reopen
		// execution authority.
		s.state.LastErrorClass = "provider_recovery_requires_exact"
	}
	pendingExact := make([]pendingExactEvaluation, 0, s.config.ExactWorkers)
	completedSignals := make([]completedScreenSignal, 0, len(primaryEvidence.Accounts))
	for _, borrower := range borrowers {
		if invalidatedBlock := s.state.TailInvalidatedBlock[borrower]; invalidatedBlock > screenBlockNumber {
			return errors.New("gateway Aave screen predates tail invalidation")
		}
	}
	exactAuthorityWasDegraded := s.state.LastErrorClass != "" &&
		s.state.LastErrorClass != revenueLaneAuthorityDivergedClass
	if authorityDiverged && !previousAuthorityDiverged {
		s.state.Counts[revenueLaneAuthorityDivergedKey]++
	}
	for order, index := range s.schedulerAccountOrder(primaryEvidence.Accounts) {
		signalReceivedMonotonic := time.Now()
		primary := primaryEvidence.Accounts[index]
		if primary.Borrower != borrowers[index] {
			return errors.New("gateway Aave primary discovery is incomplete")
		}
		bucket := classify(primary.TotalDebtBase, primary.HealthFactorWAD)
		if bucket == "liquidatable" || bucket == "urgent" || bucket == "watch" {
			s.hotBorrowers[primary.Borrower] = primary.HealthFactorWAD
			s.hotDebtBase[primary.Borrower] = primary.TotalDebtBase
			if bucket == "liquidatable" {
				if _, present := s.firstLiquidatableAt[primary.Borrower]; !present {
					s.firstLiquidatableAt[primary.Borrower] = s.nowUTC()
					s.firstLiquidatableMono[primary.Borrower] = time.Now()
				}
			} else {
				delete(s.firstLiquidatableAt, primary.Borrower)
				delete(s.firstLiquidatableMono, primary.Borrower)
			}
		} else {
			delete(s.hotBorrowers, primary.Borrower)
			delete(s.hotDebtBase, primary.Borrower)
			delete(s.hotUpperPositive, primary.Borrower)
			delete(s.latestOutcome, primary.Borrower)
			delete(s.lastExactAt, primary.Borrower)
			delete(s.firstLiquidatableAt, primary.Borrower)
			delete(s.firstLiquidatableMono, primary.Borrower)
			delete(s.state.RouteIneligible, primary.Borrower)
		}
		if err := s.updateBorrowerActivityLocked(primary.Borrower, primary.TotalDebtBase != "0"); err != nil {
			return err
		}
		wasActionable := s.hotUpperPositive[primary.Borrower]
		record := signal{Schema: "phoenix.atlas-aave-hunting-signal.v1", ObservedAt: s.nowUTC(), Cursor: cursor + uint64(index), Block: screenBlockNumber, BlockHash: screenBlockHash, Borrower: primary.Borrower, DebtBase: primary.TotalDebtBase, HF: primary.HealthFactorWAD, Bucket: bucket, Authority: false, TerminalOutcome: "prefiltered"}
		if bucket != "liquidatable" {
			delete(s.hotUpperPositive, primary.Borrower)
		}
		if bucket == "liquidatable" {
			upper, upperErr := generousUpperBound(primary, primaryEvidence.WETHPriceBase)
			if upperErr != nil {
				s.hotUpperPositive[primary.Borrower] = false
				bucket = "incomplete"
				record.Bucket = bucket
				record.TerminalOutcome = "incomplete"
			} else {
				record.ZeroCostProfitUpperBoundWei = upper.String()
				floor, _ := newBigUint(s.config.RetainedProfitFloorWei)
				if upper.Cmp(floor) <= 0 {
					s.hotUpperPositive[primary.Borrower] = false
					record.TerminalOutcome = "economic_rejection"
				} else {
					s.hotUpperPositive[primary.Borrower] = true
					record.TerminalOutcome = "exact_pending"
				}
			}
		}
		record.PrefilterCompletedAt = s.nowUTC()
		record.LiquidatableClassifiedAt = s.firstLiquidatableAt[primary.Borrower]
		signalToPrefilter := time.Since(signalReceivedMonotonic)
		if record.TerminalOutcome == "exact_pending" && wasActionable {
			s.state.Counts[exactCoalescedKey]++
		}
		if record.TerminalOutcome == "exact_pending" && authorityDiverged {
			record.ExactDeferredReason = revenueLaneAuthorityDivergedClass
		} else if record.TerminalOutcome == "exact_pending" && exactAuthorityWasDegraded && !s.config.PrimaryDiscovery {
			record.ExactDeferredReason = "provider_recovery_requires_fresh_screen"
		}
		if record.TerminalOutcome == "exact_pending" && !authorityDiverged && (!exactAuthorityWasDegraded || s.config.PrimaryDiscovery) {
			now := s.nowUTC()
			routeReason, knownRouteIneligible := s.state.RouteIneligible[primary.Borrower]
			lastExact, recentlyResolved := s.lastExactAt[primary.Borrower]
			if knownRouteIneligible && routeReason == "no_weth_debt" {
				record.ExactDeferredReason = "route_ineligible_until_tail"
				record.ExactRouteIneligibleReason = routeReason
				if s.state.Counts == nil {
					s.state.Counts = make(map[string]uint64)
				}
				s.state.Counts[exactDeferredRouteIneligibleKey]++
			} else if recentlyResolved && !now.Before(lastExact) && now.Sub(lastExact) < exactBorrowerCooldown {
				record.ExactDeferredReason = "borrower_cooldown"
				if knownRouteIneligible {
					record.ExactRouteIneligibleReason = routeReason
				}
				if s.state.Counts == nil {
					s.state.Counts = make(map[string]uint64)
				}
				s.state.Counts[exactDeferredCooldownKey]++
			} else if s.exactAdmissionBlockedLocked(now) {
				record.ExactDeferredReason = "scheduler_capacity"
				s.state.Counts[exactDeferredSchedulerKey]++
			} else {
				// Collateral supply/withdraw and collateral-enable changes are not
				// present in the debt tail. Re-probe those route reasons after the
				// normal exact cooldown instead of deferring them forever.
				if s.exactInFlightBorrowers[primary.Borrower] {
					record.ExactDeferredReason = "scheduler_capacity"
					s.state.Counts[exactDeferredSchedulerKey]++
					s.state.Counts[exactDuplicateSuppressedKey]++
				} else {
					if knownRouteIneligible {
						delete(s.state.RouteIneligible, primary.Borrower)
						s.state.Counts[routeIneligibleRechecksKey]++
					}
					exactAdmittedAt := s.nowUTC()
					exactAdmittedMonotonic := time.Now()
					reservedMilli, admitted := s.admitExactLocked(exactAdmittedAt)
					if !admitted {
						record.ExactDeferredReason = "scheduler_capacity"
						s.state.Counts[exactDeferredSchedulerKey]++
					} else {
						tracker := &exactEvaluationTracker{}
						s.state.Counts[exactEvalStartedKey]++
						s.exactInFlight++
						s.exactInFlightBorrowers[primary.Borrower] = true
						result := make(chan exactEvaluationResult, 1)
						work := pendingExactEvaluation{
							order: order, primary: primary, bucket: bucket, record: record,
							admittedAt: exactAdmittedAt, admittedMonotonic: exactAdmittedMonotonic,
							firstObserved:          s.firstLiquidatableAt[primary.Borrower],
							firstObservedMonotonic: s.firstLiquidatableMono[primary.Borrower],
							reservedMilli:          reservedMilli, tracker: tracker, result: result,
							signalToPrefilter: signalToPrefilter,
						}
						pendingExact = append(pendingExact, work)
						continue
					}
				}
			}
		}
		completedSignals = append(completedSignals, completedScreenSignal{
			order: order, record: record, borrower: primary.Borrower, bucket: bucket,
			signalToPrefilter: signalToPrefilter,
		})
	}
	if len(pendingExact) > 0 {
		// Persist the complete bounded admission batch once before issuing any
		// Exact RPC. This preserves restart-safe token accounting without making
		// every independent borrower wait behind its own state-file fsync.
		if err := s.persistStateLocked(); err != nil {
			s.state.Counts[exactEvalStartedKey] -= uint64(len(pendingExact))
			return err
		}
		for index := range pendingExact {
			work := &pendingExact[index]
			exactCtx := context.WithValue(ctx, exactEvaluationTrackerKey{}, work.tracker)
			go func(
				workerCtx context.Context,
				evaluationTracker *exactEvaluationTracker,
				input signal,
				output chan<- exactEvaluationResult,
			) {
				select {
				case s.exactWorkerSlots <- struct{}{}:
				case <-workerCtx.Done():
					output <- exactEvaluationResult{record: input, err: workerCtx.Err(), completedAt: s.nowUTC(), completedMonotonic: time.Now()}
					return
				}
				s.exactWorkerStarts.Add(1)
				evaluationTracker.workerStartedUnixNanos.Store(s.nowUTC().UnixNano())
				evaluationTracker.workerStartedMonotonic = time.Now()
				defer func() { <-s.exactWorkerSlots }()
				exactRecord, exactErr := s.resolveExact(workerCtx, input, auction)
				output <- exactEvaluationResult{record: exactRecord, err: exactErr, completedAt: s.nowUTC(), completedMonotonic: time.Now()}
			}(exactCtx, work.tracker, work.record, work.result)
		}
	}
	exactResults := make([]exactEvaluationResult, len(pendingExact))
	var exactBatchErr error
	s.mu.Unlock()
	for index := range pendingExact {
		exactResults[index] = <-pendingExact[index].result
	}
	s.mu.Lock()
	for index := range pendingExact {
		work := &pendingExact[index]
		s.exactInFlight--
		delete(s.exactInFlightBorrowers, work.primary.Borrower)
		s.settleExactBudgetLocked(work.reservedMilli, work.tracker.requests.Load())
		if exactResults[index].err != nil &&
			!errors.Is(exactResults[index].err, errRevenueLaneAuthorityDiverged) && exactBatchErr == nil {
			exactBatchErr = exactResults[index].err
		}
	}
	if exactBatchErr != nil {
		return exactBatchErr
	}
	recoveryExactTimes := make([]time.Time, 0, len(pendingExact))
	for index, work := range pendingExact {
		result := exactResults[index]
		record := work.record
		if result.err != nil {
			record = withoutCandidateAuthority(record, revenueLaneAuthorityDivergedClass)
			record.TerminalOutcome = "exact_pending"
			record.ExactDeferredReason = revenueLaneAuthorityDivergedClass
			if !authorityDiverged {
				s.state.Counts[revenueLaneAuthorityDivergedKey]++
			}
			authorityDiverged = true
		} else {
			exactCompletedAt := result.completedAt
			workerStartedAt := time.Unix(0, work.tracker.workerStartedUnixNanos.Load()).UTC()
			exactDuration := result.completedMonotonic.Sub(work.tracker.workerStartedMonotonic)
			s.state.Counts[exactEvalCompletedKey]++
			s.observeDurationLocked(
				exactEvalLatencySumKey,
				exactEvalLatencyCountKey,
				"exact_eval_latency_millis_bucket_le_",
				exactDuration,
			)
			liquidatableToExact := time.Duration(0)
			if !work.firstObservedMonotonic.IsZero() {
				liquidatableToExact = result.completedMonotonic.Sub(work.firstObservedMonotonic)
			} else if !work.firstObserved.IsZero() {
				liquidatableToExact = exactCompletedAt.Sub(work.firstObserved)
			}
			if !work.firstObserved.IsZero() {
				s.observeDurationLocked(
					liquidatableToExactSumKey,
					liquidatableToExactCountKey,
					"liquidatable_to_exact_millis_bucket_le_",
					liquidatableToExact,
				)
				s.observeDurationLocked(
					exactEligibilityLatencySumKey,
					exactEligibilityLatencyCountKey,
					"exact_eligibility_latency_millis_bucket_le_",
					func() time.Duration {
						if !work.firstObservedMonotonic.IsZero() {
							return work.admittedMonotonic.Sub(work.firstObservedMonotonic)
						}
						return work.admittedAt.Sub(work.firstObserved)
					}(),
				)
				delete(s.firstLiquidatableAt, work.primary.Borrower)
				delete(s.firstLiquidatableMono, work.primary.Borrower)
			}
			s.observeDurationLocked(
				exactQueueLatencySumKey,
				exactQueueLatencyCountKey,
				"exact_queue_latency_millis_bucket_le_",
				work.tracker.workerStartedMonotonic.Sub(work.admittedMonotonic),
			)
			firstRequestAt := time.Unix(0, work.tracker.firstRequestUnixNanos.Load()).UTC()
			initialResponseAt := time.Unix(0, work.tracker.initialResponseUnixNanos.Load()).UTC()
			if work.tracker.firstRequestUnixNanos.Load() > 0 {
				s.observeDurationLocked(
					exactDispatchLatencySumKey,
					exactDispatchLatencyCountKey,
					"exact_dispatch_latency_millis_bucket_le_",
					work.tracker.firstRequestMonotonic.Sub(work.tracker.workerStartedMonotonic),
				)
				if !work.firstObservedMonotonic.IsZero() && work.tracker.firstRequestMonotonic.After(work.firstObservedMonotonic) {
					s.observeDurationLocked(
						exactFirstRPCLatencySumKey,
						exactFirstRPCLatencyCountKey,
						"exact_first_rpc_latency_millis_bucket_le_",
						work.tracker.firstRequestMonotonic.Sub(work.firstObservedMonotonic),
					)
				}
			}
			if work.tracker.initialResponseUnixNanos.Load() > 0 && !firstRequestAt.IsZero() {
				s.observeDurationLocked(
					exactInitialLatencySumKey,
					exactInitialLatencyCountKey,
					"exact_initial_response_latency_millis_bucket_le_",
					work.tracker.initialResponseMonotonic.Sub(work.tracker.firstRequestMonotonic),
				)
				if result.completedMonotonic.After(work.tracker.initialResponseMonotonic) {
					s.observeDurationLocked(
						exactComputeLatencySumKey,
						exactComputeLatencyCountKey,
						"exact_compute_latency_millis_bucket_le_",
						result.completedMonotonic.Sub(work.tracker.initialResponseMonotonic),
					)
				}
			}
			if forkRuntime := time.Duration(work.tracker.forkRuntimeNanos.Load()); forkRuntime > 0 {
				s.observeDurationLocked(
					exactForkRuntimeSumKey,
					exactForkRuntimeCountKey,
					"exact_fork_runtime_millis_bucket_le_",
					forkRuntime,
				)
			}
			if forkQueue := time.Duration(work.tracker.forkQueueNanos.Load()); forkQueue > 0 {
				s.observeDurationLocked(
					exactForkQueueSumKey,
					exactForkQueueCountKey,
					"exact_fork_queue_millis_bucket_le_",
					forkQueue,
				)
			}
			record = result.record
			if exactAuthorityWasDegraded || authorityDiverged {
				record = withoutCandidateAuthority(record, "provider_recovery_sample")
			}
			record.ExactCompletedAt = &exactCompletedAt
			record.ExactDiagnostics = buildExactDiagnosticSummary(
				record,
				time.Duration(work.tracker.forkRuntimeNanos.Load()),
				liquidatableToExact,
			)
			if record.ExactDiagnostics != nil {
				record.ExactDiagnostics.SignalReceivedAt = record.ObservedAt.UTC().Format(time.RFC3339Nano)
				if !record.PrefilterCompletedAt.IsZero() {
					record.ExactDiagnostics.PrefilterCompletedAt = record.PrefilterCompletedAt.UTC().Format(time.RFC3339Nano)
				}
				if !record.LiquidatableClassifiedAt.IsZero() {
					record.ExactDiagnostics.LiquidatableClassifiedAt = record.LiquidatableClassifiedAt.UTC().Format(time.RFC3339Nano)
				}
				record.ExactDiagnostics.ExactEnqueuedAt = work.admittedAt.UTC().Format(time.RFC3339Nano)
				record.ExactDiagnostics.ExactWorkerStartedAt = workerStartedAt.UTC().Format(time.RFC3339Nano)
				if work.tracker.firstRequestUnixNanos.Load() > 0 {
					record.ExactDiagnostics.FirstGatewayRequestAt = firstRequestAt.UTC().Format(time.RFC3339Nano)
				}
				if work.tracker.initialResponseUnixNanos.Load() > 0 {
					record.ExactDiagnostics.InitialGatewayResponseAt = initialResponseAt.UTC().Format(time.RFC3339Nano)
				}
				record.ExactDiagnostics.ExactEvaluationCompletedAt = exactCompletedAt.UTC().Format(time.RFC3339Nano)
				if !work.firstObservedMonotonic.IsZero() && work.admittedMonotonic.After(work.firstObservedMonotonic) {
					record.ExactDiagnostics.EligibilityToAdmissionMillis = uint64(work.admittedMonotonic.Sub(work.firstObservedMonotonic) / time.Millisecond)
				} else if !work.firstObserved.IsZero() && work.admittedAt.After(work.firstObserved) {
					record.ExactDiagnostics.EligibilityToAdmissionMillis = uint64(work.admittedAt.Sub(work.firstObserved) / time.Millisecond)
				}
				if work.tracker.workerStartedMonotonic.After(work.admittedMonotonic) {
					record.ExactDiagnostics.QueueToWorkerLatencyMillis = uint64(work.tracker.workerStartedMonotonic.Sub(work.admittedMonotonic) / time.Millisecond)
				}
				if work.tracker.firstRequestUnixNanos.Load() > 0 && work.tracker.firstRequestMonotonic.After(work.tracker.workerStartedMonotonic) {
					record.ExactDiagnostics.WorkerToGatewayLatencyMillis = uint64(work.tracker.firstRequestMonotonic.Sub(work.tracker.workerStartedMonotonic) / time.Millisecond)
				}
				if work.tracker.initialResponseUnixNanos.Load() > 0 && work.tracker.initialResponseMonotonic.After(work.tracker.firstRequestMonotonic) {
					record.ExactDiagnostics.InitialGatewayLatencyMillis = uint64(work.tracker.initialResponseMonotonic.Sub(work.tracker.firstRequestMonotonic) / time.Millisecond)
				}
				record.ExactDiagnostics.ExactStateRequestCount = work.tracker.requests.Load()
			}
			s.lastExactAt[work.primary.Borrower] = exactCompletedAt
			if record.ExactRouteIneligibleReason != "" {
				s.state.RouteIneligible[work.primary.Borrower] = record.ExactRouteIneligibleReason
				s.state.Counts[exactRouteIneligibleObservedKey]++
			} else {
				delete(s.state.RouteIneligible, work.primary.Borrower)
			}
			if record.ExactPrimaryProvider == primaryProviderID {
				recoveryExactTimes = append(recoveryExactTimes, exactCompletedAt)
			}
		}
		completedSignals = append(completedSignals, completedScreenSignal{
			order: work.order, record: record, borrower: work.primary.Borrower, bucket: work.bucket,
			signalToPrefilter: work.signalToPrefilter,
		})
	}
	sort.Slice(completedSignals, func(left, right int) bool {
		return completedSignals[left].order < completedSignals[right].order
	})
	for _, completed := range completedSignals {
		s.observeDurationLocked(
			signalPrefilterLatencySumKey,
			signalPrefilterLatencyCountKey,
			"signal_prefilter_latency_millis_bucket_le_",
			completed.signalToPrefilter,
		)
		if err := s.persistScreenSignalLocked(
			ctx,
			completed.record,
			completed.borrower,
			completed.bucket,
			&authorityDiverged,
		); err != nil {
			return err
		}
	}
	if !authorityDiverged {
		sort.Slice(recoveryExactTimes, func(left, right int) bool {
			return recoveryExactTimes[left].Before(recoveryExactTimes[right])
		})
		for _, recoveredAt := range recoveryExactTimes {
			if s.state.LastPrimaryExactAt != nil && !recoveredAt.After(s.state.LastPrimaryExactAt.UTC()) {
				recoveredAt = s.state.LastPrimaryExactAt.UTC().Add(time.Nanosecond)
			}
			s.state.LastProviderPrimary = primaryProviderID
			s.state.LastProviderSecond = ""
			s.state.LastPrimaryExactAt = &recoveredAt
			s.recordProviderRecoveryLocked(recoveredAt, primaryProviderID)
		}
	}
	now := time.Now().UTC()
	if advanceSeed {
		s.state.Cursor += uint64(len(borrowers))
	}
	s.state.LastBlockNumber = screenBlockNumber
	s.state.LastBlockHash = screenBlockHash
	s.state.LastProviderPrimary = primaryEvidence.ProviderID
	s.state.LastBatchAt = &now
	if authorityDiverged {
		s.state.ProviderRecoverySamples = nil
		s.state.LastPrimaryExactAt = nil
		s.state.LastProviderSecond = ""
		s.state.LastErrorClass = revenueLaneAuthorityDivergedClass
		s.state.LastAttemptAt = &now
	}
	return s.persistStateLocked()
}

func exactRouteIneligibleReason(reserves []exactReserve) string {
	var wethDebt, usdcEDebt *big.Int
	stableWETHDebt := false
	stableUSDCeDebt := false
	var wethCollateral, nativeUSDCCollateral *big.Int
	wethCollateralEnabled := false
	nativeUSDCCollateralEnabled := false

	for _, reserve := range reserves {
		switch strings.ToLower(reserve.Asset) {
		case wethAddress:
			stable, stableOK := newBigUint(reserve.CurrentStableDebt)
			variable, variableOK := newBigUint(reserve.CurrentVariableDebt)
			balance, balanceOK := newBigUint(reserve.CurrentATokenBalance)
			if !stableOK || !variableOK || !balanceOK {
				return ""
			}
			stableWETHDebt = stable.Sign() > 0
			wethDebt = variable
			wethCollateral = balance
			wethCollateralEnabled = balance.Sign() > 0 && reserve.UsageAsCollateralEnabled
		case nativeUSDCAddress:
			balance, balanceOK := newBigUint(reserve.CurrentATokenBalance)
			if !balanceOK {
				return ""
			}
			nativeUSDCCollateral = balance
			nativeUSDCCollateralEnabled = balance.Sign() > 0 && reserve.UsageAsCollateralEnabled
		case usdcEAddress:
			stable, stableOK := newBigUint(reserve.CurrentStableDebt)
			variable, variableOK := newBigUint(reserve.CurrentVariableDebt)
			if !stableOK || !variableOK {
				return ""
			}
			stableUSDCeDebt = stable.Sign() > 0
			usdcEDebt = variable
		}
	}

	if stableWETHDebt {
		return "unsupported_stable_weth_debt"
	}
	if stableUSDCeDebt {
		return "unsupported_stable_usdc_e_debt"
	}
	wethDebtReviewed := wethDebt != nil && wethDebt.Sign() > 0 &&
		((wethCollateral != nil && wethCollateral.Sign() > 0) ||
			(nativeUSDCCollateral != nil && nativeUSDCCollateral.Sign() > 0))
	usdcEDebtReviewed := usdcEDebt != nil && usdcEDebt.Sign() > 0 &&
		wethCollateral != nil && wethCollateral.Sign() > 0
	if !wethDebtReviewed && !usdcEDebtReviewed {
		if (wethDebt != nil && wethDebt.Sign() > 0) || (usdcEDebt != nil && usdcEDebt.Sign() > 0) {
			return "no_reviewed_unwind_route"
		}
		return "no_supported_debt_asset"
	}
	if wethDebtReviewed && !wethCollateralEnabled && !nativeUSDCCollateralEnabled ||
		usdcEDebtReviewed && !wethCollateralEnabled {
		return "supported_collateral_disabled"
	}
	return ""
}

func (s *Screener) resolveExact(ctx context.Context, record signal, auction *observer.LedgerRecord) (signal, error) {
	liveMaximumInput, err := s.currentAaveLiveMaximumInputAmount(ctx)
	if err != nil {
		return record, err
	}
	requestID := fmt.Sprintf("aave-exact-%d-%d", record.Cursor, time.Now().UnixMilli())
	body, _ := json.Marshal(exactRequest{
		SchemaVersion:      "phoenix.rpc.aave-exact-request.v3",
		ChainID:            42161,
		RequestID:          requestID,
		Borrower:           record.Borrower,
		MaximumInputAmount: maximumReviewedInputWei,
	})
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, s.config.GatewayURL+"/v1/aave/exact", bytes.NewReader(body))
	if err != nil {
		return record, err
	}
	req.Header.Set("Content-Type", "application/json")
	s.recordExactStateRequest(ctx)
	response, err := s.client.Do(req)
	if err != nil {
		return record, err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return record, decodeGatewayError(response)
	}
	var result exactResponse
	if err := json.NewDecoder(io.LimitReader(response.Body, maximumResponse)).Decode(&result); err != nil {
		return record, err
	}
	if result.SchemaVersion != "phoenix.rpc.aave-exact-response.v5" || result.ChainID != 42161 || result.RequestID != requestID || result.BlockNumber == 0 || result.BlockHash == "" || result.StateRoot == "" || result.Primary.ProviderID != primaryProviderID || result.Confirmation != nil || result.Quorum != 1 {
		return record, errors.New("exact Aave provider evidence is incomplete")
	}
	if tracker := exactTrackerFromContext(ctx); tracker != nil {
		if tracker.initialResponseUnixNanos.CompareAndSwap(0, s.nowUTC().UnixNano()) {
			tracker.initialResponseMonotonic = time.Now()
		}
	}
	if result.BlockNumber < record.Block {
		return record, errors.New("exact Aave evidence predates its primary screen")
	}
	record.ExactPrimaryProvider = result.Primary.ProviderID
	record.ExactConfirmationProvider = nil
	record.Block = result.BlockNumber
	record.BlockHash = result.BlockHash
	record.StateRoot = result.StateRoot
	if len(result.Primary.Liquidations) == 0 {
		record.ExactRouteIneligibleReason = exactRouteIneligibleReason(result.Primary.Reserves)
		if record.ExactRouteIneligibleReason == "" {
			record.AuthorityRejectionReason = "no_reviewed_liquidation_variant"
		}
		record.TerminalOutcome = "economic_rejection"
		return record, nil
	}
	if err := s.validateLiquidationVariants(result.Primary.Liquidations, result.Primary.FlashPremiumBPS); err != nil {
		return record, err
	}
	selected, hadSimulationFailure, diagnostics := s.evaluateLiquidationBatch(
		ctx,
		record,
		result.Primary.Liquidations,
		result.Primary.FlashPremiumBPS,
		liveMaximumInput,
	)
	record.SizeDiagnostics = diagnostics
	if selected == nil {
		if hadSimulationFailure {
			record.TerminalOutcome = "fork_pending"
		} else {
			record.TerminalOutcome = "economic_rejection"
		}
		return record, nil
	}
	record.Block = selected.Simulation.BlockNumber
	record.BlockHash = selected.Simulation.BlockHash
	record.StateRoot = selected.Simulation.StateRoot
	record.SelectedRoute = selected.Route.Name
	record.ExpectedNetPnLWei = selected.Expected.String()
	record.ConservativeNetPnLWei = selected.Conservative.String()
	record.RiskReserveAmountWei = selected.RiskReserve.String()
	record.ExecutionCostWei = selected.ExecutionCost.String()
	record.EstimatedL1CostWei = selected.EstimatedL1Cost.String()
	record.FlashPremiumWei = selected.Simulation.FlashPremiumWei
	selectedRepay, selectedRepayOK := newBigUint(selected.Liquidation.RepayAmount)
	liveMaximum, liveMaximumOK := newBigUint(selected.LiveMaximumInput)
	if !selectedRepayOK || !liveMaximumOK {
		return record, errors.New("selected liquidation size authority is invalid")
	}
	if selectedRepay.Cmp(liveMaximum) > 0 {
		if selected.Simulation.EvidenceMode != counterfactualForkEvidenceMode {
			return record, errors.New("counterfactual size lacks counterfactual fork evidence")
		}
		record.Authority = false
		record.TerminalOutcome = "counterfactual_positive"
		record.AuthorityRejectionReason = "live_size_authorization_required"
		setDiagnosticRejection(
			record.SizeDiagnostics,
			selected.Liquidation,
			selected.Route,
			record.AuthorityRejectionReason,
		)
		markSelectedDiagnostic(record.SizeDiagnostics, selected.Liquidation, selected.Route)
		return record, nil
	}
	if selected.Simulation.EvidenceMode != directForkEvidenceMode {
		return record, errors.New("live-authorized size lacks direct fork evidence")
	}
	if auction != nil && strings.ToLower(selected.Liquidation.DebtAsset) == wethAddress {
		atlas, atlasErr := s.buildAtlasCandidate(ctx, record, selected, auction)
		if atlasErr != nil {
			return record, atlasErr
		}
		if atlas == nil {
			markSelectedDiagnostic(record.SizeDiagnostics, selected.Liquidation, selected.Route)
			record.Authority = false
			record.TerminalOutcome = "atlas_evidence_rejection"
			record.AuthorityRejectionReason = "atlas_callback_evidence_unavailable"
			return record, nil
		}
		record.AtlasCandidate = atlas
	} else {
		candidate, candidateErr := s.buildExecutionCandidate(record, selected)
		if candidateErr != nil {
			return record, candidateErr
		}
		record.ExecutionCandidate = candidate
	}
	markSelectedDiagnostic(record.SizeDiagnostics, selected.Liquidation, selected.Route)
	record.Authority = true
	record.TerminalOutcome = "candidate"
	return record, nil
}

func (s *Screener) currentAaveLiveMaximumInputAmount(ctx context.Context) (string, error) {
	configured, configuredOK := newBigUint(s.config.MaximumInputAmountWei)
	reviewed, reviewedOK := newBigUint(maximumReviewedInputWei)
	if !configuredOK || !reviewedOK || configured.Sign() <= 0 || configured.Cmp(reviewed) > 0 {
		return "", errors.New("configured Aave live size ceiling is invalid")
	}
	value := s.config.MaximumInputAmountWei
	if authority, ok := s.config.SignalSink.(LiveSizeAuthority); ok {
		current, err := authority.CurrentAaveLiveMaximumInputAmount(ctx)
		if err != nil {
			return "", err
		}
		value = current
	}
	parsed, parsedOK := newBigUint(value)
	if !parsedOK || parsed.Sign() <= 0 || parsed.Cmp(configured) > 0 || parsed.Cmp(reviewed) > 0 {
		return "", errors.New("current Aave live size authority is invalid")
	}
	return parsed.String(), nil
}

func (s *Screener) fetchExactSnapshot(ctx context.Context, borrower string) (exactResponse, error) {
	requestID := fmt.Sprintf("aave-exact-refresh-%d", time.Now().UnixNano())
	body, _ := json.Marshal(exactRequest{
		SchemaVersion:      "phoenix.rpc.aave-exact-request.v3",
		ChainID:            42161,
		RequestID:          requestID,
		Borrower:           borrower,
		MaximumInputAmount: maximumReviewedInputWei,
	})
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, s.config.GatewayURL+"/v1/aave/exact", bytes.NewReader(body))
	if err != nil {
		return exactResponse{}, err
	}
	req.Header.Set("Content-Type", "application/json")
	s.recordExactStateRequest(ctx)
	response, err := s.client.Do(req)
	if err != nil {
		return exactResponse{}, err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return exactResponse{}, decodeGatewayError(response)
	}
	var result exactResponse
	if err := json.NewDecoder(io.LimitReader(response.Body, maximumResponse)).Decode(&result); err != nil {
		return exactResponse{}, err
	}
	if result.SchemaVersion != "phoenix.rpc.aave-exact-response.v5" || result.ChainID != 42161 || result.RequestID != requestID || result.BlockNumber == 0 || result.BlockHash == "" || result.StateRoot == "" || result.Primary.ProviderID != primaryProviderID || result.Confirmation != nil || result.Quorum != 1 {
		return exactResponse{}, errors.New("fresh exact Aave provider evidence is incomplete")
	}
	return result, nil
}

func (s *Screener) validateLiquidationVariants(liquidations []exactLiquidation, flashPremiumBPS uint64) error {
	if flashPremiumBPS == 0 || flashPremiumBPS > s.config.FlashPremiumBPS || len(liquidations) == 0 || len(liquidations) > 21 {
		return errors.New("exact liquidation bounds are invalid")
	}
	previousByPair := make(map[string]*big.Int)
	fixedCountByPair := make(map[string]int)
	terminalSeenByPair := make(map[string]bool)
	for index := range liquidations {
		liquidation := &liquidations[index]
		debt := strings.ToLower(liquidation.DebtAsset)
		collateral := strings.ToLower(liquidation.CollateralAsset)
		pair := debt + "|" + collateral
		requested, requestedOK := newBigUint(liquidation.RequestedRepayAmount)
		actual, actualOK := newBigUint(liquidation.ActualRepayAmount)
		repay, repayOK := newBigUint(liquidation.RepayAmount)
		premium, premiumOK := newBigUint(liquidation.FlashPremiumAmount)
		maximumRepay, maximumOK := newBigUint(liquidation.MaximumRepayAmount)
		reviewedSize, reviewedOK := newBigUint(liquidation.ReviewedSizeWETHWei)
		derivedMaximum, conversionOK := wethToDebtFloor(maximumReviewedInputWei, liquidation)
		expectedReview := "weth_debt_reviewed"
		supportedPair := debt == wethAddress && (collateral == wethAddress || collateral == nativeUSDCAddress)
		if debt == usdcEAddress {
			expectedReview = "usdc_e_debt_reviewed"
			supportedPair = collateral == wethAddress
		}
		if !requestedOK || !actualOK || !repayOK || !premiumOK || !maximumOK || !reviewedOK || !conversionOK || requested.Sign() <= 0 || actual.Sign() <= 0 || actual.Cmp(repay) != 0 || actual.Cmp(requested) != 0 || actual.Cmp(maximumRepay) > 0 || maximumRepay.Cmp(derivedMaximum) != 0 || !supportedPair || liquidation.DebtAssetReview != expectedReview || !addressPattern.MatchString(collateral) {
			return errors.New("exact liquidation variant is invalid")
		}
		switch liquidation.SizeClassification {
		case fixedReviewedSizeClassification:
			derivedReviewed, ok := wethToDebtFloor(reviewedSize.String(), liquidation)
			if !ok || derivedReviewed.Cmp(repay) != 0 || !isReviewedWETHSize(reviewedSize) || liquidation.TerminalSizeReason != "" || terminalSeenByPair[pair] {
				return errors.New("fixed reviewed liquidation classification is invalid")
			}
			fixedCountByPair[pair]++
			if fixedCountByPair[pair] > 7 {
				return errors.New("exact liquidation grid exceeds its collateral bound")
			}
		case terminalSizeClassification:
			normalized, ok := debtToWETHFloor(repay.String(), liquidation)
			if !ok || normalized.Cmp(reviewedSize) != 0 || isReviewedWETHSize(reviewedSize) || fixedCountByPair[pair] != 0 || terminalSeenByPair[pair] || liquidation.TerminalSizeReason != belowMinReviewedSizeReason && liquidation.TerminalSizeReason != dustPartialInvalidReason {
				return errors.New("terminal liquidation classification is invalid")
			}
			terminalSeenByPair[pair] = true
		default:
			return errors.New("exact liquidation size classification is invalid")
		}
		minimumRoutes, maximumRoutes := 0, 0
		if debt == wethAddress && collateral == nativeUSDCAddress {
			minimumRoutes, maximumRoutes = 1, 3
		} else if debt == usdcEAddress && collateral == wethAddress {
			minimumRoutes, maximumRoutes = 1, 1
		}
		if len(liquidation.UnwindQuotes) < minimumRoutes || len(liquidation.UnwindQuotes) > maximumRoutes {
			return errors.New("exact liquidation route count exceeds its reviewed bound")
		}
		fees := make(map[uint32]bool, len(liquidation.UnwindQuotes))
		pools := make(map[string]bool, len(liquidation.UnwindQuotes))
		for _, quote := range liquidation.UnwindQuotes {
			pool := strings.ToLower(quote.Pool)
			factory := strings.ToLower(quote.Factory)
			token0 := strings.ToLower(quote.Token0)
			token1 := strings.ToLower(quote.Token1)
			zeroForOne, directionOK := uniswapZeroForOne(collateral, token0, token1)
			outputDebt, outputOK := newBigUint(quote.OutputDebt)
			outputWETH, outputWETHOK := newBigUint(quote.OutputWETH)
			normalized, normalizedOK := debtToWETHFloor(quote.OutputDebt, liquidation)
			reviewedWETHPool := quote.Fee == 100 && pool == wethNativeUSDCPool100Address ||
				quote.Fee == 500 && pool == wethNativeUSDCPool500Address ||
				quote.Fee == 3_000 && pool == wethNativeUSDCPool3000Address
			routeReviewed := debt == wethAddress && reviewedWETHPool ||
				debt == usdcEAddress && quote.Fee == 500 && pool == wethUSDCePool500Address
			if fees[quote.Fee] || pools[pool] || factory != uniswapFactoryAddress || !directionOK || quote.ZeroForOne != zeroForOne || !routeReviewed || !outputOK || !outputWETHOK || !normalizedOK || outputDebt.Sign() <= 0 || outputWETH.Cmp(normalized) != 0 {
				return errors.New("exact liquidation routes are not unique")
			}
			fees[quote.Fee] = true
			pools[pool] = true
		}
		if previous := previousByPair[pair]; previous != nil && actual.Cmp(previous) <= 0 {
			return errors.New("exact liquidation variants are not strictly increasing")
		}
		expectedPremium := aavePercentMul(actual, flashPremiumBPS)
		if premium.Cmp(expectedPremium) != 0 {
			return errors.New("exact flash premium is inconsistent")
		}
		previousByPair[pair] = new(big.Int).Set(actual)
	}
	return nil
}

func (s *Screener) evaluateLiquidationBatch(ctx context.Context, record signal, liquidations []exactLiquidation, originalFlashPremiumBPS uint64, liveMaximumInput string) (*liquidationEvaluation, bool, []sizeDiagnostic) {
	deadline := uint64(s.nowUTC().Add(60 * time.Second).Unix())
	probes := make([]*liquidationProbe, 0, len(liquidations))
	requests := make([]simulationRequest, 0, len(liquidations))
	diagnostics := make([]sizeDiagnostic, 0, len(liquidations))
	diagnosticIndex := make(map[string]int)
	floor, _ := newBigUint(s.config.RetainedProfitFloorWei)
	hadSimulationFailure := false
	for index := range liquidations {
		prepared, err := s.prepareLiquidationProbes(&liquidations[index])
		if err != nil {
			hadSimulationFailure = true
			continue
		}
		for _, probe := range prepared {
			diagnostic := newSizeDiagnostic(probe, liveMaximumInput)
			diagnosticIndex[liquidationDiagnosticKey(probe.Liquidation, probe.Route)] = len(diagnostics)
			diagnostics = append(diagnostics, diagnostic)
			if probe.ExactEdge.Cmp(floor) <= 0 {
				diagnostic := &diagnostics[len(diagnostics)-1]
				diagnostic.MarginToRetainedFloorWei = new(big.Int).Sub(
					new(big.Int).Set(probe.ExactEdge),
					floor,
				).String()
				diagnostic.FinalRejectionReason = "gross_edge_below_retained_profit_gate"
				if probe.Liquidation.SizeClassification == terminalSizeClassification {
					diagnostic.TerminalSizeUnprofitable = true
				}
				continue
			}
			probes = append(probes, probe)
			requests = append(requests, s.newSimulationRequest(record, probe.Liquidation, simulationRequest{
				MinimumCollateralReceived: probe.MinimumCollateral,
				MinimumUnwindOutput:       "1",
				MinimumProfitWETHWei:      s.config.RetainedProfitFloorWei,
				ExpectedProfit:            probe.ExactEdge.String(),
				SelectedPool:              probe.Route.SelectedPool,
				SelectedFactory:           probe.Route.Factory,
				SelectedFee:               probe.Route.SelectedFee,
				ZeroForOne:                probe.Route.ZeroForOne,
			}, deadline, liveMaximumInput))
		}
	}
	if len(requests) == 0 {
		return nil, hadSimulationFailure, diagnostics
	}
	outcomes, err := s.simulateExactBatch(ctx, record, requests)
	if err != nil {
		for _, probe := range probes {
			setDiagnosticRejection(diagnostics, probe.Liquidation, probe.Route, "fork_simulation_failed")
		}
		return nil, true, diagnostics
	}
	viable := make([]*liquidationEvaluation, 0, len(outcomes))
	for index, outcome := range outcomes {
		if outcome.Err != nil {
			hadSimulationFailure = true
			diagnostics[diagnosticIndex[liquidationDiagnosticKey(probes[index].Liquidation, probes[index].Route)]].FinalRejectionReason = "fork_simulation_failed"
			continue
		}
		evaluation, viableForMaterialization, evaluationErr := s.evaluateLiquidationProbe(probes[index], outcome.Response, liveMaximumInput)
		if evaluationErr != nil {
			hadSimulationFailure = true
			diagnostics[diagnosticIndex[liquidationDiagnosticKey(probes[index].Liquidation, probes[index].Route)]].FinalRejectionReason = "fork_economics_invalid"
			continue
		}
		if evaluation != nil {
			updateSizeDiagnostic(&diagnostics[diagnosticIndex[liquidationDiagnosticKey(evaluation.Liquidation, evaluation.Route)]], evaluation, floor)
		}
		if viableForMaterialization {
			viable = append(viable, evaluation)
		} else {
			diagnostics[diagnosticIndex[liquidationDiagnosticKey(probes[index].Liquidation, probes[index].Route)]].FinalRejectionReason = "conservative_net_pnl_below_threshold"
		}
	}

	finalized := make([]*liquidationEvaluation, 0, len(viable))
	pending := viable
	for attempt := 0; attempt < 2 && len(pending) > 0; attempt++ {
		deadline = uint64(s.nowUTC().Add(60 * time.Second).Unix())
		requests = requests[:0]
		for _, evaluation := range pending {
			requests = append(requests, s.newSimulationRequest(record, evaluation.Liquidation, simulationRequest{
				MinimumCollateralReceived: evaluation.MinimumCollateral,
				MinimumUnwindOutput:       evaluation.MinimumUnwind,
				MinimumProfit:             evaluation.MinimumProfit,
				MinimumProfitWETHWei:      evaluation.MinimumProfitWETH,
				ExpectedProfit:            evaluation.Simulation.RealizedProfit,
				SelectedPool:              evaluation.Route.SelectedPool,
				SelectedFactory:           evaluation.Route.Factory,
				SelectedFee:               evaluation.Route.SelectedFee,
				ZeroForOne:                evaluation.Route.ZeroForOne,
			}, deadline, liveMaximumInput))
		}
		outcomes, err = s.simulateExactBatch(ctx, record, requests)
		if err != nil {
			hadSimulationFailure = true
			for _, evaluation := range pending {
				setDiagnosticRejection(diagnostics, evaluation.Liquidation, evaluation.Route, "fork_simulation_failed")
			}
			break
		}
		next := make([]*liquidationEvaluation, 0, len(pending))
		for index, outcome := range outcomes {
			if outcome.Err != nil {
				hadSimulationFailure = true
				setDiagnosticRejection(diagnostics, pending[index].Liquidation, pending[index].Route, "fork_simulation_failed")
				continue
			}
			complete, retry, evaluationErr := s.advanceLiquidationMaterialization(pending[index], outcome.Response)
			if evaluationErr != nil {
				hadSimulationFailure = true
				setDiagnosticRejection(diagnostics, pending[index].Liquidation, pending[index].Route, "fork_economics_invalid")
				continue
			}
			if complete != nil {
				updateSizeDiagnostic(&diagnostics[diagnosticIndex[liquidationDiagnosticKey(complete.Liquidation, complete.Route)]], complete, floor)
				finalized = append(finalized, complete)
			} else if retry != nil {
				if attempt == 1 {
					hadSimulationFailure = true
					setDiagnosticRejection(diagnostics, retry.Liquidation, retry.Route, "bound_convergence_failed")
				} else {
					next = append(next, retry)
				}
			} else {
				setDiagnosticRejection(diagnostics, pending[index].Liquidation, pending[index].Route, "conservative_net_pnl_below_threshold")
			}
		}
		pending = next
	}
	if len(finalized) == 0 {
		return nil, hadSimulationFailure, diagnostics
	}
	sort.SliceStable(finalized, func(left, right int) bool {
		return betterLiquidationEvaluation(finalized[right], finalized[left])
	})
	freshExact, err := s.fetchExactSnapshot(ctx, record.Borrower)
	if err != nil {
		for _, evaluation := range finalized {
			setDiagnosticRejection(diagnostics, evaluation.Liquidation, evaluation.Route, "fresh_exact_unavailable")
		}
		return nil, true, diagnostics
	}
	if freshExact.Primary.FlashPremiumBPS != originalFlashPremiumBPS || !equalLiquidations(freshExact.Primary.Liquidations, liquidations) {
		for _, evaluation := range finalized {
			setDiagnosticRejection(diagnostics, evaluation.Liquidation, evaluation.Route, "fresh_state_mismatch")
		}
		return nil, true, diagnostics
	}
	if freshExact.BlockNumber < record.Block {
		for _, evaluation := range finalized {
			setDiagnosticRejection(diagnostics, evaluation.Liquidation, evaluation.Route, "fresh_state_mismatch")
		}
		return nil, true, diagnostics
	}
	if err := s.validateLiquidationVariants(freshExact.Primary.Liquidations, freshExact.Primary.FlashPremiumBPS); err != nil {
		for _, evaluation := range finalized {
			setDiagnosticRejection(diagnostics, evaluation.Liquidation, evaluation.Route, "fresh_state_mismatch")
		}
		return nil, true, diagnostics
	}
	record.Block = freshExact.BlockNumber
	record.BlockHash = freshExact.BlockHash
	record.StateRoot = freshExact.StateRoot
	deadline = uint64(s.nowUTC().Add(60 * time.Second).Unix())
	requests = requests[:0]
	for _, evaluation := range finalized {
		requests = append(requests, s.newSimulationRequest(record, evaluation.Liquidation, simulationRequest{
			MinimumCollateralReceived: evaluation.MinimumCollateral,
			MinimumUnwindOutput:       evaluation.MinimumUnwind,
			MinimumProfit:             evaluation.MinimumProfit,
			MinimumProfitWETHWei:      evaluation.MinimumProfitWETH,
			ExpectedProfit:            evaluation.Simulation.RealizedProfit,
			SelectedPool:              evaluation.Route.SelectedPool,
			SelectedFactory:           evaluation.Route.Factory,
			SelectedFee:               evaluation.Route.SelectedFee,
			ZeroForOne:                evaluation.Route.ZeroForOne,
		}, deadline, liveMaximumInput))
	}
	outcomes, err = s.simulateExactBatch(ctx, record, requests)
	if err != nil {
		for _, evaluation := range finalized {
			setDiagnosticRejection(diagnostics, evaluation.Liquidation, evaluation.Route, "fork_simulation_failed")
		}
		return nil, true, diagnostics
	}
	var selected *liquidationEvaluation
	for index, outcome := range outcomes {
		if outcome.Err != nil {
			hadSimulationFailure = true
			setDiagnosticRejection(diagnostics, finalized[index].Liquidation, finalized[index].Route, "fork_simulation_failed")
			continue
		}
		fresh, retry, evaluationErr := s.advanceLiquidationMaterialization(finalized[index], outcome.Response)
		if retry != nil {
			setDiagnosticRejection(diagnostics, retry.Liquidation, retry.Route, "bound_convergence_failed")
			return nil, true, diagnostics
		}
		if evaluationErr != nil {
			hadSimulationFailure = true
			setDiagnosticRejection(diagnostics, finalized[index].Liquidation, finalized[index].Route, "fork_economics_invalid")
			continue
		}
		if fresh != nil {
			updateSizeDiagnostic(&diagnostics[diagnosticIndex[liquidationDiagnosticKey(fresh.Liquidation, fresh.Route)]], fresh, floor)
			if betterLiquidationEvaluation(fresh, selected) {
				selected = fresh
			}
		} else {
			setDiagnosticRejection(diagnostics, finalized[index].Liquidation, finalized[index].Route, "conservative_net_pnl_below_threshold")
		}
	}
	for index := range diagnostics {
		if selected == nil || liquidationDiagnosticKey(selected.Liquidation, selected.Route) != liquidationDiagnosticKeyValues(diagnostics[index].ReviewedSize, diagnostics[index].Route) {
			if diagnostics[index].FinalRejectionReason == "" && diagnostics[index].ConservativeNetPnLWei != "" {
				diagnostics[index].FinalRejectionReason = "smallest_positive_reviewed_size_not_selected"
			}
		}
	}
	sort.SliceStable(diagnostics, func(left, right int) bool {
		leftSize, _ := newBigUint(diagnostics[left].ReviewedSize)
		rightSize, _ := newBigUint(diagnostics[right].ReviewedSize)
		if comparison := leftSize.Cmp(rightSize); comparison != 0 {
			return comparison < 0
		}
		return diagnostics[left].Route < diagnostics[right].Route
	})
	return selected, hadSimulationFailure, diagnostics
}

func (s *Screener) prepareLiquidationProbes(liquidation *exactLiquidation) ([]*liquidationProbe, error) {
	routes, err := liquidationRoutesFor(liquidation)
	if err != nil {
		return nil, err
	}
	repay, repayOK := newBigUint(liquidation.RepayAmount)
	flash, flashOK := newBigUint(liquidation.FlashPremiumAmount)
	minimumCollateral, collateralOK := newBigUint(liquidation.LiquidatorCollateral)
	if !repayOK || !flashOK || !collateralOK || minimumCollateral.Sign() <= 0 {
		return nil, errors.New("exact liquidation economics are invalid")
	}
	probes := make([]*liquidationProbe, 0, len(routes))
	for _, route := range routes {
		exactEdgeDebt := new(big.Int).Sub(new(big.Int).Set(route.OutputDebt), repay)
		exactEdgeDebt.Sub(exactEdgeDebt, flash)
		exactEdge := new(big.Int)
		if exactEdgeDebt.Sign() > 0 {
			var ok bool
			exactEdge, ok = debtToWETHFloor(exactEdgeDebt.String(), liquidation)
			if !ok {
				return nil, errors.New("exact debt economics normalization is invalid")
			}
		}
		probes = append(probes, &liquidationProbe{
			Liquidation: liquidation, Route: route, ExactEdge: exactEdge,
			MinimumCollateral: minimumCollateral.String(),
		})
	}
	return probes, nil
}

func liquidationDiagnosticKey(liquidation *exactLiquidation, route liquidationRoute) string {
	if liquidation == nil {
		return ""
	}
	return liquidationDiagnosticKeyValues(liquidation.RepayAmount, route.Name)
}

func liquidationDiagnosticKeyValues(size, route string) string {
	return size + "|" + route
}

func setDiagnosticRejection(diagnostics []sizeDiagnostic, liquidation *exactLiquidation, route liquidationRoute, reason string) {
	key := liquidationDiagnosticKey(liquidation, route)
	for index := range diagnostics {
		if liquidationDiagnosticKeyValues(diagnostics[index].ReviewedSize, diagnostics[index].Route) == key {
			diagnostics[index].FinalRejectionReason = reason
			if liquidation != nil && liquidation.SizeClassification == terminalSizeClassification && reason == "conservative_net_pnl_below_threshold" {
				diagnostics[index].TerminalSizeUnprofitable = true
			}
			return
		}
	}
}

func markSelectedDiagnostic(diagnostics []sizeDiagnostic, liquidation *exactLiquidation, route liquidationRoute) {
	key := liquidationDiagnosticKey(liquidation, route)
	for index := range diagnostics {
		if liquidationDiagnosticKeyValues(diagnostics[index].ReviewedSize, diagnostics[index].Route) == key {
			diagnostics[index].Selected = true
			return
		}
	}
}

func diagnosticMargin(diagnostic sizeDiagnostic) (*big.Int, bool) {
	if diagnostic.GasLimit == 0 && diagnostic.FinalRejectionReason != "gross_edge_below_retained_profit_gate" {
		return nil, false
	}
	value, ok := new(big.Int).SetString(diagnostic.MarginToRetainedFloorWei, 10)
	return value, ok
}

func canonicalExactFailureClass(counts map[string]uint64) string {
	for _, reason := range []string{
		"fresh_state_mismatch",
		"fresh_exact_unavailable",
		"bound_convergence_failed",
		"fork_simulation_failed",
		"fork_economics_invalid",
		"conservative_net_pnl_below_threshold",
		"gross_edge_below_retained_profit_gate",
	} {
		if counts[reason] > 0 {
			return reason
		}
	}
	return ""
}

func buildExactDiagnosticSummary(record signal, exactForkLatency, liquidatableToExact time.Duration) *exactDiagnosticSummary {
	summary := &exactDiagnosticSummary{
		Schema:                   "phoenix.aave-exact-diagnostics.v1",
		EvaluationStage:          "exact",
		RouteEligibility:         "eligible",
		ReviewedCombinationCount: uint64(len(record.SizeDiagnostics)),
		RejectionCounts:          make(map[string]uint64),
	}
	if record.ExactRouteIneligibleReason != "" {
		summary.RouteEligibility = record.ExactRouteIneligibleReason
	}
	if exactForkLatency > 0 {
		summary.ExactForkLatencyMillis = uint64(exactForkLatency / time.Millisecond)
	}
	if liquidatableToExact > 0 {
		summary.LiquidatableToExactLatencyMillis = uint64(liquidatableToExact / time.Millisecond)
	}
	ranked := make([]sizeDiagnostic, 0, len(record.SizeDiagnostics))
	for index := range record.SizeDiagnostics {
		diagnostic := record.SizeDiagnostics[index]
		if diagnostic.FinalRejectionReason != "" {
			summary.RejectionCounts[diagnostic.FinalRejectionReason]++
		}
		if diagnostic.GasLimit > 0 || strings.HasPrefix(diagnostic.FinalRejectionReason, "fork_") ||
			diagnostic.FinalRejectionReason == "bound_convergence_failed" ||
			strings.HasPrefix(diagnostic.FinalRejectionReason, "fresh_") {
			summary.ForkAttempted = true
		}
		if diagnostic.EvidenceMode != "" {
			summary.ForkPassed = true
		}
		margin, marginOK := diagnosticMargin(diagnostic)
		if marginOK {
			ranked = append(ranked, diagnostic)
			if margin.Sign() > 0 {
				if diagnostic.LiveAuthorized {
					summary.AnyLiveAuthorizedPositive = true
				} else {
					summary.AnyCounterfactualPositive = true
				}
			}
		}
		if diagnostic.Selected {
			copy := diagnostic
			summary.SelectedDiagnostic = &copy
			summary.ForkEvidenceMode = diagnostic.EvidenceMode
		}
	}
	if record.AuthorityRejectionReason != "" && summary.RejectionCounts[record.AuthorityRejectionReason] == 0 {
		summary.RejectionCounts[record.AuthorityRejectionReason]++
	}
	if record.ExactRouteIneligibleReason != "" {
		summary.RejectionCounts[record.ExactRouteIneligibleReason]++
	}
	sort.SliceStable(ranked, func(left, right int) bool {
		leftMargin, _ := diagnosticMargin(ranked[left])
		rightMargin, _ := diagnosticMargin(ranked[right])
		if comparison := leftMargin.Cmp(rightMargin); comparison != 0 {
			return comparison > 0
		}
		leftSize, _ := newBigUint(ranked[left].ReviewedSize)
		rightSize, _ := newBigUint(ranked[right].ReviewedSize)
		if comparison := leftSize.Cmp(rightSize); comparison != 0 {
			return comparison < 0
		}
		return ranked[left].Route < ranked[right].Route
	})
	if len(ranked) > 0 {
		best := ranked[0]
		summary.BestDiagnostic = &best
		closest := ranked[0]
		closestMargin, _ := diagnosticMargin(closest)
		closestDistance := new(big.Int).Abs(new(big.Int).Set(closestMargin))
		for index := 1; index < len(ranked); index++ {
			margin, _ := diagnosticMargin(ranked[index])
			distance := new(big.Int).Abs(new(big.Int).Set(margin))
			if distance.Cmp(closestDistance) < 0 ||
				(distance.Cmp(closestDistance) == 0 && margin.Cmp(closestMargin) > 0) {
				closest = ranked[index]
				closestMargin = margin
				closestDistance = distance
			}
		}
		summary.ClosestMarginToRetainedFloorWei = closest.MarginToRetainedFloorWei
		if summary.ForkEvidenceMode == "" {
			summary.ForkEvidenceMode = best.EvidenceMode
		}
		for index := range ranked {
			if ranked[index].LiveAuthorized {
				bestLive := ranked[index]
				summary.BestLiveAuthorizedDiagnostic = &bestLive
				break
			}
		}
		limit := len(ranked)
		if limit > 3 {
			limit = 3
		}
		summary.TopDiagnostics = append(summary.TopDiagnostics, ranked[:limit]...)
	}
	summary.FailureClass = canonicalExactFailureClass(summary.RejectionCounts)
	if summary.FailureClass == "" {
		if record.AuthorityRejectionReason != "" {
			summary.FailureClass = record.AuthorityRejectionReason
		} else if record.ExactRouteIneligibleReason != "" {
			summary.FailureClass = record.ExactRouteIneligibleReason
		}
	}
	if summary.ForkAttempted {
		summary.EvaluationStage = "fork"
	}
	if summary.FailureClass == "fresh_state_mismatch" || summary.FailureClass == "fresh_exact_unavailable" {
		summary.EvaluationStage = "fresh_exact"
	}
	if record.TerminalOutcome == "candidate" {
		summary.EvaluationStage = "candidate_materialized"
	}
	return summary
}

func newSizeDiagnostic(probe *liquidationProbe, liveMaximumText string) sizeDiagnostic {
	flashWETH, flashWETHOK := debtToWETHFloor(probe.Liquidation.FlashPremiumAmount, probe.Liquidation)
	if !flashWETHOK {
		flashWETH = new(big.Int)
	}
	diagnostic := sizeDiagnostic{
		ReviewedSize:             probe.Liquidation.RepayAmount,
		DebtAsset:                probe.Liquidation.DebtAsset,
		CollateralAsset:          probe.Liquidation.CollateralAsset,
		DebtAssetReview:          probe.Liquidation.DebtAssetReview,
		SizeClassification:       probe.Liquidation.SizeClassification,
		TerminalSizeReason:       probe.Liquidation.TerminalSizeReason,
		Route:                    probe.Route.Name,
		FlashPremiumWei:          flashWETH.String(),
		AtlasExposureWei:         "0",
		AtlasBidWei:              "0",
		RiskReserveWei:           "0",
		ExecutionCostWei:         "0",
		L1CostWei:                "0",
		GasPriceWei:              "0",
		ExpectedNetPnLWei:        "0",
		ConservativeNetPnLWei:    "0",
		MarginToRetainedFloorWei: "0",
	}
	repay, repayOK := newBigUint(probe.Liquidation.RepayAmount)
	liveMaximum, liveOK := wethToDebtFloor(liveMaximumText, probe.Liquidation)
	if repayOK && flashWETHOK {
		diagnostic.GrossLiquidationEdgeWei = new(big.Int).Add(new(big.Int).Set(probe.ExactEdge), flashWETH).String()
	}
	if repayOK && liveOK {
		diagnostic.LiveAuthorized = repay.Cmp(liveMaximum) <= 0
	}
	oracle, oracleOK := newBigUint(probe.Liquidation.OracleUnwindOutputWETH)
	if !oracleOK || oracle.Sign() == 0 {
		oracle = new(big.Int).Set(probe.Route.Output)
	}
	loss := new(big.Int).Sub(new(big.Int).Set(oracle), probe.Route.Output)
	if loss.Sign() < 0 {
		loss.SetInt64(0)
	}
	diagnostic.DEXUnwindLossWei = loss.String()
	if oracle.Sign() > 0 {
		diagnostic.PriceImpactBPS = new(big.Int).Div(
			new(big.Int).Mul(new(big.Int).Set(loss), big.NewInt(10_000)),
			oracle,
		).String()
	} else {
		diagnostic.PriceImpactBPS = "0"
	}
	return diagnostic
}

func updateSizeDiagnostic(diagnostic *sizeDiagnostic, evaluation *liquidationEvaluation, floor *big.Int) {
	if diagnostic == nil || evaluation == nil || evaluation.Simulation == nil {
		return
	}
	diagnostic.GasLimit = evaluation.Simulation.EstimatedGasLimit
	diagnostic.GasPriceWei = evaluation.Simulation.EstimatedMaxFeePerGasWei
	diagnostic.L1CostWei = evaluation.EstimatedL1Cost.String()
	diagnostic.ExecutionCostWei = evaluation.ExecutionCost.String()
	diagnostic.RiskReserveWei = evaluation.RiskReserve.String()
	diagnostic.ExpectedNetPnLWei = evaluation.Expected.String()
	diagnostic.ConservativeNetPnLWei = evaluation.Conservative.String()
	diagnostic.MarginToRetainedFloorWei = new(big.Int).Sub(
		new(big.Int).Set(evaluation.Conservative),
		floor,
	).String()
	diagnostic.EvidenceMode = evaluation.Simulation.EvidenceMode
	diagnostic.FinalRejectionReason = ""
}

func (s *Screener) evaluateLiquidationProbe(probe *liquidationProbe, simulation *simulationResponse, liveMaximumInput string) (*liquidationEvaluation, bool, error) {
	if probe == nil || probe.Liquidation == nil || probe.Route.Output == nil || probe.ExactEdge == nil {
		return nil, false, errors.New("liquidation probe is incomplete")
	}
	realized, cost, l1Cost, err := s.boundedSimulationEconomics(simulation, probe.Liquidation)
	if err != nil {
		return nil, false, err
	}
	if realized.Cmp(probe.ExactEdge) != 0 {
		return nil, false, errors.New("exact quote and fork realization disagree")
	}
	expected, err := authoritativeGatewayNet(simulation, realized, cost, new(big.Int))
	if err != nil {
		return nil, false, err
	}
	floor, _ := newBigUint(s.config.RetainedProfitFloorWei)
	reserve, conservative, _ := profitEdgeReserve(expected, probe.Route.Output, s.config.EconomicReserveBPS)
	minimumUnwind, unwindOK := minimumUnwindForReserve(probe.Route.OutputDebt, reserve, probe.Liquidation)
	if !unwindOK {
		return nil, false, errors.New("minimum unwind unit conversion is invalid")
	}
	minimumProfitWETH := strictMinimumProfit(floor, cost)
	minimumProfit, minimumProfitOK := wethToDebtCeil(minimumProfitWETH.String(), probe.Liquidation)
	liveMaximumRaw, liveMaximumOK := wethToDebtFloor(liveMaximumInput, probe.Liquidation)
	if !minimumProfitOK || !liveMaximumOK {
		return nil, false, errors.New("liquidation authority unit conversion is invalid")
	}
	evaluation := &liquidationEvaluation{
		Liquidation: probe.Liquidation, Route: probe.Route, Simulation: simulation,
		Expected: expected, Conservative: conservative, RiskReserve: reserve,
		ExecutionCost: cost, EstimatedL1Cost: l1Cost,
		MinimumCollateral: probe.MinimumCollateral, MinimumUnwind: minimumUnwind.String(),
		MinimumProfit: minimumProfit.String(), MinimumProfitWETH: minimumProfitWETH.String(),
		LiveMaximumInput: liveMaximumRaw.String(), LiveMaximumInputWETH: liveMaximumInput,
	}
	return evaluation, conservative.Cmp(floor) > 0 && minimumUnwind.Sign() > 0, nil
}

func (s *Screener) advanceLiquidationMaterialization(probe *liquidationEvaluation, simulation *simulationResponse) (*liquidationEvaluation, *liquidationEvaluation, error) {
	if probe == nil || probe.Liquidation == nil || probe.Simulation == nil || probe.Route.Output == nil {
		return nil, nil, errors.New("liquidation probe evidence is incomplete")
	}
	realized, realizedOK := newBigUint(probe.Simulation.RealizedProfit)
	floor, floorOK := newBigUint(s.config.RetainedProfitFloorWei)
	minimumUnwind, unwindOK := newBigUint(probe.MinimumUnwind)
	minimumProfit, profitOK := newBigUint(probe.MinimumProfit)
	if !realizedOK || !floorOK || !unwindOK || !profitOK || minimumUnwind.Sign() <= 0 || minimumProfit.Sign() <= 0 {
		return nil, nil, errors.New("liquidation probe bounds are invalid")
	}
	finalRealized, finalCost, finalL1Cost, err := s.boundedSimulationEconomics(simulation, probe.Liquidation)
	if err != nil {
		return nil, nil, err
	}
	if finalRealized.Cmp(realized) != 0 {
		return nil, nil, errors.New("final fork realization changed across bound materialization")
	}
	finalExpected, err := authoritativeGatewayNet(simulation, finalRealized, finalCost, new(big.Int))
	if err != nil {
		return nil, nil, err
	}
	finalReserve, finalConservative, _ := profitEdgeReserve(finalExpected, probe.Route.Output, s.config.EconomicReserveBPS)
	finalMinimumUnwind, unwindOK := minimumUnwindForReserve(probe.Route.OutputDebt, finalReserve, probe.Liquidation)
	requiredMinimumProfitWETH := strictMinimumProfit(floor, finalCost)
	requiredMinimumProfit, profitConversionOK := wethToDebtCeil(requiredMinimumProfitWETH.String(), probe.Liquidation)
	if !unwindOK || !profitConversionOK {
		return nil, nil, errors.New("final liquidation unit conversion is invalid")
	}
	if finalConservative.Cmp(floor) <= 0 || finalMinimumUnwind.Sign() <= 0 {
		return nil, nil, nil
	}
	if minimumUnwind.Cmp(finalMinimumUnwind) >= 0 && minimumProfit.Cmp(requiredMinimumProfit) >= 0 {
		return &liquidationEvaluation{
			Liquidation: probe.Liquidation, Route: probe.Route, Simulation: simulation,
			Expected: finalExpected, Conservative: finalConservative, RiskReserve: finalReserve,
			ExecutionCost: finalCost, EstimatedL1Cost: finalL1Cost,
			MinimumCollateral: probe.MinimumCollateral, MinimumUnwind: probe.MinimumUnwind,
			MinimumProfit: probe.MinimumProfit, MinimumProfitWETH: probe.MinimumProfitWETH,
			LiveMaximumInput: probe.LiveMaximumInput, LiveMaximumInputWETH: probe.LiveMaximumInputWETH,
		}, nil, nil
	}
	retry := *probe
	retry.MinimumUnwind = finalMinimumUnwind.String()
	retry.MinimumProfit = requiredMinimumProfit.String()
	retry.MinimumProfitWETH = requiredMinimumProfitWETH.String()
	return nil, &retry, nil
}

func liquidationRoutesFor(liquidation *exactLiquidation) ([]liquidationRoute, error) {
	if liquidation == nil {
		return nil, errors.New("unsupported exact debt route")
	}
	debt := strings.ToLower(liquidation.DebtAsset)
	collateral := strings.ToLower(liquidation.CollateralAsset)
	if debt == wethAddress && collateral == wethAddress {
		output, ok := newBigUint(liquidation.LiquidatorCollateral)
		if !ok || output.Sign() <= 0 {
			return nil, errors.New("exact identity-route output is invalid")
		}
		return []liquidationRoute{{
			Name: "WETH_IDENTITY", Output: output, OutputDebt: new(big.Int).Set(output), SelectedPool: zeroAddress,
			Factory: zeroAddress, TokenPath: []string{wethAddress}, TokenIn: wethAddress, TokenOut: wethAddress,
		}}, nil
	}
	if !(debt == wethAddress && collateral == nativeUSDCAddress) &&
		!(debt == usdcEAddress && collateral == wethAddress) {
		return nil, errors.New("unsupported exact collateral route")
	}
	routes := make([]liquidationRoute, 0, len(liquidation.UnwindQuotes))
	seen := make(map[string]bool)
	for _, quote := range liquidation.UnwindQuotes {
		pool := strings.ToLower(quote.Pool)
		factory := strings.ToLower(quote.Factory)
		token0 := strings.ToLower(quote.Token0)
		token1 := strings.ToLower(quote.Token1)
		output, outputOK := newBigUint(quote.OutputWETH)
		outputDebt, outputDebtOK := newBigUint(quote.OutputDebt)
		zeroForOne, directionOK := uniswapZeroForOne(collateral, token0, token1)
		identityValid := addressPattern.MatchString(pool) && factory == uniswapFactoryAddress &&
			directionOK &&
			((debt == wethAddress && (quote.Fee == 100 || quote.Fee == 500 || quote.Fee == 3_000)) ||
				(debt == usdcEAddress && quote.Fee == 500 && pool == wethUSDCePool500Address)) &&
			quote.ZeroForOne == zeroForOne && !seen[pool]
		if !identityValid || !outputOK || !outputDebtOK || output.Sign() <= 0 || outputDebt.Sign() <= 0 {
			return nil, errors.New("exact route quotation identity is invalid")
		}
		seen[pool] = true
		routes = append(routes, liquidationRoute{
			Name: fmt.Sprintf("UNISWAP_V3_%d", quote.Fee), Output: output, OutputDebt: outputDebt,
			SelectedPool: pool, Factory: factory, SelectedFee: quote.Fee,
			ZeroForOne: zeroForOne, TokenPath: []string{collateral, debt}, TokenIn: collateral, TokenOut: debt,
		})
	}
	if len(routes) == 0 {
		return nil, errors.New("exact route quotation is invalid")
	}
	sort.Slice(routes, func(left, right int) bool { return routes[left].SelectedFee < routes[right].SelectedFee })
	return routes, nil
}

func uniswapZeroForOne(tokenIn, token0, token1 string) (bool, bool) {
	if tokenIn == token0 && token0 != token1 {
		return true, true
	}
	if tokenIn == token1 && token0 != token1 {
		return false, true
	}
	return false, false
}

func executionLegsFor(route liquidationRoute, minimumUnwind string) []executionLeg {
	if route.SelectedFee == 0 {
		return []executionLeg{}
	}
	return []executionLeg{{
		Pool: route.SelectedPool, Factory: route.Factory,
		TokenIn: route.TokenIn, TokenOut: route.TokenOut,
		Fee: route.SelectedFee, ZeroForOne: route.ZeroForOne, MinAmountOut: minimumUnwind,
	}}
}

func (s *Screener) boundedSimulationEconomics(simulation *simulationResponse, liquidation *exactLiquidation) (*big.Int, *big.Int, *big.Int, error) {
	if simulation == nil || simulation.EstimatedGasLimit == 0 || simulation.EstimatedGasLimit > s.config.MaximumGasLimit {
		return nil, nil, nil, errors.New("bounded simulation gas is invalid")
	}
	realized, realizedOK := newBigUint(simulation.RealizedProfit)
	realizedDebt, realizedDebtOK := newBigUint(simulation.RealizedProfitDebtAsset)
	expectedRealized, expectedRealizedOK := debtToWETHFloor(simulation.RealizedProfitDebtAsset, liquidation)
	cost, costOK := newBigUint(simulation.EstimatedExecutionCostWei)
	l1Cost, l1OK := newBigUint(simulation.EstimatedL1CostWei)
	flash, flashOK := newBigUint(simulation.FlashPremiumWei)
	flashDebt, flashDebtOK := newBigUint(simulation.FlashPremiumDebtAsset)
	expectedFlash, expectedFlashOK := newBigUint(liquidation.FlashPremiumAmount)
	expectedFlashWETH, expectedFlashWETHOK := debtToWETHFloor(liquidation.FlashPremiumAmount, liquidation)
	maxFee, maxFeeOK := newBigUint(s.config.MaximumFeePerGasWei)
	estimatedMaxFee, estimatedMaxFeeOK := newBigUint(simulation.EstimatedMaxFeePerGasWei)
	if !realizedOK || !realizedDebtOK || !expectedRealizedOK || !costOK || !l1OK || !flashOK || !flashDebtOK || !expectedFlashOK || !expectedFlashWETHOK || !maxFeeOK || !estimatedMaxFeeOK || realizedDebt.Sign() <= 0 || realized.Cmp(expectedRealized) != 0 || cost.Sign() <= 0 || estimatedMaxFee.Sign() <= 0 || flashDebt.Cmp(expectedFlash) != 0 || flash.Cmp(expectedFlashWETH) != 0 || l1Cost.Cmp(cost) > 0 || estimatedMaxFee.Cmp(maxFee) > 0 {
		return nil, nil, nil, errors.New("bounded simulation economics are invalid")
	}
	boundCost := new(big.Int).Mul(new(big.Int).SetUint64(simulation.EstimatedGasLimit), estimatedMaxFee)
	if cost.Cmp(boundCost) != 0 {
		return nil, nil, nil, errors.New("bounded simulation cost is not bound to the quoted gas limit and fee")
	}
	return realized, cost, l1Cost, nil
}

func authoritativeGatewayNet(simulation *simulationResponse, realized, executionCost, atlasBid *big.Int) (*big.Int, error) {
	reported, reportedOK := newBigUint(simulation.ConservativeNetPnL)
	calculated := new(big.Int).Sub(new(big.Int).Set(realized), executionCost)
	calculated.Sub(calculated, atlasBid)
	if calculated.Sign() < 0 {
		calculated.SetInt64(0)
	}
	if !reportedOK || reported.Cmp(calculated) != 0 {
		return nil, errors.New("gateway conservative economics are inconsistent")
	}
	return reported, nil
}

func betterLiquidationEvaluation(candidate, current *liquidationEvaluation) bool {
	if candidate == nil {
		return false
	}
	if current == nil {
		return true
	}
	candidateRepay, _ := newBigUint(candidate.Liquidation.RepayAmount)
	currentRepay, _ := newBigUint(current.Liquidation.RepayAmount)
	if comparison := candidateRepay.Cmp(currentRepay); comparison != 0 {
		return comparison < 0
	}
	if comparison := candidate.Conservative.Cmp(current.Conservative); comparison != 0 {
		return comparison > 0
	}
	if comparison := candidate.Expected.Cmp(current.Expected); comparison != 0 {
		return comparison > 0
	}
	return candidate.Route.Name < current.Route.Name
}

func strictMinimumProfit(floor, cost *big.Int) *big.Int {
	result := new(big.Int).Add(new(big.Int).Set(floor), cost)
	return result.Add(result, big.NewInt(1))
}

func ceilBasisPoints(value *big.Int, bps uint64) *big.Int {
	result := new(big.Int).Mul(new(big.Int).Set(value), new(big.Int).SetUint64(bps))
	return result.Add(result, big.NewInt(9_999)).Div(result, big.NewInt(10_000))
}

func aavePercentMul(value *big.Int, bps uint64) *big.Int {
	result := new(big.Int).Mul(new(big.Int).Set(value), new(big.Int).SetUint64(bps))
	return result.Add(result, big.NewInt(5_000)).Div(result, big.NewInt(10_000))
}

func profitEdgeReserve(expected, grossOutput *big.Int, reserveBPS uint64) (*big.Int, *big.Int, *big.Int) {
	reserve := new(big.Int)
	if expected.Sign() > 0 && reserveBPS > 0 {
		reserve.Mul(new(big.Int).Set(expected), new(big.Int).SetUint64(reserveBPS))
		reserve.Add(reserve, big.NewInt(9_999)).Div(reserve, big.NewInt(10_000))
	}
	conservative := new(big.Int).Sub(new(big.Int).Set(expected), reserve)
	minimumUnwind := new(big.Int).Sub(new(big.Int).Set(grossOutput), reserve)
	if minimumUnwind.Sign() < 0 {
		minimumUnwind.SetInt64(0)
	}
	return reserve, conservative, minimumUnwind
}

func (s *Screener) buildAtlasCandidate(
	ctx context.Context,
	record signal,
	selected *liquidationEvaluation,
	auction *observer.LedgerRecord,
) (*atlasCandidate, error) {
	if auction == nil || selected == nil || selected.Simulation == nil || !auction.RelevantAaveAuction {
		return nil, nil
	}
	if strings.ToLower(selected.Liquidation.DebtAsset) != wethAddress {
		return nil, nil
	}
	// The current gateway evidence exercises executeAaveLiquidation directly.
	// It does not prove Atlas caller/solver/bid/reconcile behavior, so an auction
	// must fail closed until the gateway returns explicit callback-path evidence.
	if selected.Simulation.EvidenceMode != atlasCallbackEvidenceMode {
		return nil, nil
	}
	deadline, deadlineOK := newUint64(auction.AuctionDeadlineBlock)
	oracleGasPrice, gasPriceOK := newBigUint(auction.OracleGasPriceWei)
	maximumFee, maximumFeeOK := newBigUint(s.config.MaximumFeePerGasWei)
	maximumPriorityFee, maximumPriorityOK := newBigUint(s.config.MaximumPriorityFeeWei)
	if !deadlineOK || deadline == 0 || auction.SolverGasLimit == 0 || auction.SolverGasLimit < selected.Simulation.EstimatedGasLimit || auction.SolverGasLimit > s.config.MaximumGasLimit || !gasPriceOK || oracleGasPrice.Sign() <= 0 || !maximumFeeOK || !maximumPriorityOK || oracleGasPrice.Cmp(maximumFee) > 0 || oracleGasPrice.Cmp(maximumPriorityFee) > 0 {
		return nil, errors.New("Atlas auction bounds are invalid")
	}
	gross, grossOK := newBigUint(selected.Simulation.RealizedProfit)
	floor, floorOK := newBigUint(s.config.RetainedProfitFloorWei)
	configuredMaximumBid, bidCapOK := newBigUint(s.config.MaximumAtlasBidWei)
	if !grossOK || !floorOK || !bidCapOK {
		return nil, errors.New("Atlas economics configuration is invalid")
	}
	if configuredMaximumBid.Sign() == 0 {
		return nil, nil
	}
	// Atlas can settle its solver gas liability from bonded AtlETH. Treat the
	// larger of the bounded direct estimate and auction liability as one
	// execution/bond exposure; adding both would count the same gas twice.
	auctionExposure := new(big.Int).Mul(new(big.Int).SetUint64(auction.SolverGasLimit), oracleGasPrice)
	exposure := new(big.Int).Set(selected.ExecutionCost)
	if auctionExposure.Cmp(exposure) > 0 {
		exposure.Set(auctionExposure)
	}
	preBidExpected := new(big.Int).Sub(new(big.Int).Set(gross), exposure)
	_, zeroBidConservative, _ := profitEdgeReserve(preBidExpected, selected.Route.Output, s.config.EconomicReserveBPS)
	if zeroBidConservative.Cmp(floor) <= 0 {
		return nil, nil
	}
	maximumBid := new(big.Int).Sub(new(big.Int).Set(zeroBidConservative), floor)
	maximumBid.Sub(maximumBid, big.NewInt(1))
	if maximumBid.Cmp(configuredMaximumBid) > 0 {
		maximumBid.Set(configuredMaximumBid)
	}
	if maximumBid.Sign() <= 0 {
		return nil, nil
	}
	selectedBid := new(big.Int).Div(new(big.Int).Set(maximumBid), big.NewInt(2))
	if selectedBid.Sign() == 0 {
		selectedBid.SetUint64(1)
	}
	expected := new(big.Int).Sub(new(big.Int).Set(preBidExpected), selectedBid)
	_, conservative, atlasMinimumUnwind := profitEdgeReserve(expected, selected.Route.Output, s.config.EconomicReserveBPS)
	if conservative.Cmp(floor) <= 0 || atlasMinimumUnwind.Sign() <= 0 {
		return nil, nil
	}
	minimumProfit := strictMinimumProfit(floor, exposure)
	atlasSimulation, err := s.simulateExact(ctx, record, selected.Liquidation, simulationRequest{
		MinimumCollateralReceived: selected.MinimumCollateral,
		MinimumUnwindOutput:       atlasMinimumUnwind.String(),
		MinimumProfit:             minimumProfit.String(),
		MinimumProfitWETHWei:      minimumProfit.String(),
		ExpectedProfit:            gross.String(),
		SelectedPool:              selected.Route.SelectedPool,
		SelectedFactory:           selected.Route.Factory,
		SelectedFee:               selected.Route.SelectedFee,
		ZeroForOne:                selected.Route.ZeroForOne,
		AtlasMode:                 true,
		AtlasBid:                  selectedBid.String(),
	}, selected.LiveMaximumInput)
	if err != nil {
		return nil, nil
	}
	if atlasSimulation.EvidenceMode != atlasCallbackEvidenceMode {
		return nil, nil
	}
	atlasGross, atlasDirectCost, _, economicsErr := s.boundedSimulationEconomics(atlasSimulation, selected.Liquidation)
	if economicsErr != nil {
		return nil, economicsErr
	}
	if atlasGross.Cmp(gross) != 0 || new(big.Int).Sub(new(big.Int).Set(atlasGross), selectedBid).Cmp(minimumProfit) < 0 {
		return nil, nil
	}
	gatewayNet, economicsErr := authoritativeGatewayNet(atlasSimulation, atlasGross, atlasDirectCost, selectedBid)
	if economicsErr != nil {
		return nil, economicsErr
	}
	extraExposure := new(big.Int)
	if auctionExposure.Cmp(atlasDirectCost) > 0 {
		extraExposure.Sub(auctionExposure, atlasDirectCost)
	}
	finalExpected := new(big.Int).Sub(new(big.Int).Set(gatewayNet), extraExposure)
	finalExposure := new(big.Int).Set(atlasDirectCost)
	if auctionExposure.Cmp(finalExposure) > 0 {
		finalExposure.Set(auctionExposure)
	}
	_, finalConservative, _ := profitEdgeReserve(finalExpected, selected.Route.Output, s.config.EconomicReserveBPS)
	if finalConservative.Cmp(floor) <= 0 || minimumProfit.Cmp(strictMinimumProfit(floor, finalExposure)) < 0 {
		return nil, nil
	}
	calldata, err := hex.DecodeString(strings.TrimPrefix(atlasSimulation.CalldataHex, "0x"))
	if err != nil || len(calldata) <= 4 {
		return nil, errors.New("Atlas solver calldata is invalid")
	}
	operation := atlasPreparedOperation{
		From:         s.config.CallerAddress,
		To:           strings.ToLower(auction.Atlas),
		Value:        "0",
		Gas:          auction.SolverGasLimit,
		MaxFeePerGas: auction.OracleGasPriceWei,
		Deadline:     deadline,
		Solver:       s.config.ExecutorAddress,
		Control:      strings.ToLower(auction.DappControl),
		UserOpHash:   strings.ToLower(auction.UserOpHash),
		BidToken:     nil,
		BidAmount:    selectedBid.String(),
		Data:         "0x" + hex.EncodeToString(calldata[4:]),
	}
	encoded, err := json.Marshal(operation)
	if err != nil {
		return nil, err
	}
	operationHash := sha256.Sum256(encoded)
	return &atlasCandidate{
		AuctionID:            auction.AuctionID,
		SelectedSize:         selected.Liquidation.RepayAmount,
		MaximumInputAmount:   selected.LiveMaximumInput,
		MaximumBid:           maximumBid.String(),
		SelectedBid:          selectedBid.String(),
		ExpectedNetPnL:       finalExpected.String(),
		ConservativeNetPnL:   finalConservative.String(),
		EvidenceMode:         atlasSimulation.EvidenceMode,
		SimulationResultHash: atlasSimulation.SimulationResultHash,
		OperationHash:        hex.EncodeToString(operationHash[:]),
		Operation:            operation,
		ObservedAt:           auction.ObservedAt,
	}, nil
}

func (s *Screener) simulateExact(ctx context.Context, record signal, liquidation *exactLiquidation, partial simulationRequest, liveMaximumInput string) (*simulationResponse, error) {
	request := s.newSimulationRequest(
		record,
		liquidation,
		partial,
		uint64(s.nowUTC().Add(60*time.Second).Unix()),
		liveMaximumInput,
	)
	outcomes, err := s.simulateExactBatch(ctx, record, []simulationRequest{request})
	if err != nil {
		return nil, err
	}
	if len(outcomes) != 1 {
		return nil, errors.New("simulation batch result is incomplete")
	}
	return outcomes[0].Response, outcomes[0].Err
}

func (s *Screener) newSimulationRequest(record signal, liquidation *exactLiquidation, partial simulationRequest, deadline uint64, liveMaximumInput string) simulationRequest {
	partial.SchemaVersion = "phoenix.rpc.aave-simulate-request.v4"
	partial.ChainID = 42161
	partial.RequestID = fmt.Sprintf("aave-sim-%d-%s-%s-%d", record.Cursor, liquidation.CollateralAsset, liquidation.RepayAmount, time.Now().UnixNano())
	partial.BlockNumber = record.Block
	partial.BlockHash = record.BlockHash
	partial.StateRoot = record.StateRoot
	partial.ExecutorAddress = s.config.ExecutorAddress
	partial.ExecutorCodeHash = s.config.ExecutorCodeHash
	partial.CallerAddress = s.config.CallerAddress
	partial.ReleaseSHA = s.config.ReleaseSHA
	partial.Borrower = record.Borrower
	partial.DebtAsset = liquidation.DebtAsset
	partial.CollateralAsset = liquidation.CollateralAsset
	partial.DebtAssetDecimals = liquidation.DebtAssetDecimals
	partial.DebtAssetPriceBase = liquidation.DebtAssetPriceBase
	partial.WETHPriceBase = liquidation.WETHPriceBase
	partial.RepayAmount = liquidation.RepayAmount
	repay, repayOK := newBigUint(liquidation.RepayAmount)
	liveMaximum, liveOK := wethToDebtFloor(liveMaximumInput, liquidation)
	if !liveOK {
		liveMaximum = new(big.Int)
	}
	partial.LiveMaximumInputAmount = liveMaximum.String()
	partial.LiveMaximumInputWETHWei = liveMaximumInput
	partial.Counterfactual = repayOK && liveOK && repay.Cmp(liveMaximum) > 0
	if partial.Counterfactual {
		partial.MaximumInputAmount = liquidation.MaximumRepayAmount
		partial.MaximumInputWETHWei = maximumReviewedInputWei
	} else {
		partial.MaximumInputAmount = liveMaximum.String()
		partial.MaximumInputWETHWei = liveMaximumInput
	}
	if partial.MinimumProfitWETHWei == "" {
		partial.MinimumProfitWETHWei = partial.MinimumProfit
	}
	if minimumProfit, ok := wethToDebtCeil(partial.MinimumProfitWETHWei, liquidation); ok {
		partial.MinimumProfit = minimumProfit.String()
	} else {
		partial.MinimumProfit = "0"
	}
	partial.RetainedProfitFloor = s.config.RetainedProfitFloorWei
	partial.GasLimit = s.config.MaximumGasLimit
	partial.MaxFeePerGas = s.config.MaximumFeePerGasWei
	partial.MaxPriorityFeePerGas = s.config.MaximumPriorityFeeWei
	partial.DeadlineUnixSeconds = deadline
	if partial.AtlasBid == "" {
		partial.AtlasBid = "0"
	}
	return partial
}

func (s *Screener) simulateExactBatch(ctx context.Context, record signal, simulations []simulationRequest) ([]simulationBatchOutcome, error) {
	if len(simulations) == 0 {
		return nil, errors.New("simulation batch size is invalid")
	}
	queueStarted := time.Now()
	select {
	case s.exactForkPermits() <- struct{}{}:
	case <-ctx.Done():
		return nil, ctx.Err()
	}
	tracker := exactTrackerFromContext(ctx)
	if tracker != nil {
		tracker.forkQueueNanos.Add(uint64(time.Since(queueStarted)))
	}
	forkStarted := time.Now()
	defer func() {
		if tracker != nil {
			tracker.forkRuntimeNanos.Add(uint64(time.Since(forkStarted)))
		}
		<-s.exactForkPermits()
	}()
	outcomes := make([]simulationBatchOutcome, 0, len(simulations))
	for offset := 0; offset < len(simulations); offset += 8 {
		end := offset + 8
		if end > len(simulations) {
			end = len(simulations)
		}
		// Each Production-sized chunk can consume most of its 60-second paced
		// gateway window. Refresh only the execution deadline (never the pinned
		// state or economic bounds) so a later reviewed route cannot fail merely
		// because an earlier chunk used the shared upstream budget.
		chunkRequests := append([]simulationRequest(nil), simulations[offset:end]...)
		chunkDeadline := uint64(s.nowUTC().Add(60 * time.Second).Unix())
		for index := range chunkRequests {
			chunkRequests[index].DeadlineUnixSeconds = chunkDeadline
		}
		chunk, err := s.simulateExactBatchChunk(ctx, record, chunkRequests)
		if err != nil {
			return nil, err
		}
		outcomes = append(outcomes, chunk...)
	}
	return outcomes, nil
}

func (s *Screener) simulateExactBatchChunk(ctx context.Context, record signal, simulations []simulationRequest) ([]simulationBatchOutcome, error) {
	if len(simulations) == 0 || len(simulations) > 8 {
		return nil, errors.New("simulation batch chunk size is invalid")
	}
	atlasMode := simulations[0].AtlasMode
	for _, simulation := range simulations {
		if simulation.AtlasMode != atlasMode {
			return nil, errors.New("simulation batch mixes incompatible evidence paths")
		}
	}
	expectedBatchEvidenceMode := directForkEvidenceMode
	if atlasMode {
		expectedBatchEvidenceMode = atlasCallbackEvidenceMode
	}
	requestID := fmt.Sprintf("aave-sim-batch-%d-%d", record.Cursor, time.Now().UnixNano())
	batch := simulationBatchRequest{
		SchemaVersion: "phoenix.rpc.aave-simulate-batch-request.v3",
		ChainID:       42161,
		RequestID:     requestID,
		Simulations:   simulations,
	}
	body, err := json.Marshal(batch)
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, s.config.GatewayURL+"/v1/aave/simulate-batch", bytes.NewReader(body))
	if err != nil {
		return nil, err
	}
	req.Header.Set("Content-Type", "application/json")
	client := s.batchClient
	if client == nil {
		client = s.client
	}
	s.recordExactStateRequest(ctx)
	response, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusOK {
		return nil, decodeGatewayError(response)
	}
	var result simulationBatchResponse
	if err := json.NewDecoder(io.LimitReader(response.Body, maximumResponse)).Decode(&result); err != nil {
		return nil, err
	}
	if result.SchemaVersion != "phoenix.rpc.aave-simulate-batch-response.v4" || result.ChainID != 42161 || result.RequestID != requestID || result.BlockNumber != record.Block || result.BlockHash != record.BlockHash || result.StateRoot != record.StateRoot || result.PrimaryProviderID != primaryProviderID || result.ConfirmationProviderID != nil || result.Quorum != 1 || result.EvidenceMode != expectedBatchEvidenceMode || len(result.Results) != len(simulations) {
		return nil, errors.New("simulation batch evidence is incomplete")
	}
	outcomes := make([]simulationBatchOutcome, len(simulations))
	for index, item := range result.Results {
		if item.RequestID != simulations[index].RequestID || item.Response == nil == (item.Error == nil) {
			return nil, errors.New("simulation batch result identity is incomplete")
		}
		if item.Error != nil {
			if !errorClassPattern.MatchString(item.Error.ErrorClass) {
				return nil, errors.New("simulation batch error contract is invalid")
			}
			outcomes[index].Err = &gatewayResponseError{
				statusCode: batchGatewayErrorStatus(item.Error.ErrorClass),
				class:      item.Error.ErrorClass,
				retryable:  item.Error.Retryable,
			}
			continue
		}
		expectedEvidenceMode := directForkEvidenceMode
		if simulations[index].AtlasMode {
			expectedEvidenceMode = atlasCallbackEvidenceMode
		} else if simulations[index].Counterfactual {
			expectedEvidenceMode = counterfactualForkEvidenceMode
		}
		if err := validateSimulationResponse(item.Response, &simulations[index], record, expectedEvidenceMode); err != nil {
			return nil, err
		}
		outcomes[index].Response = item.Response
	}
	return outcomes, nil
}

func validateSimulationResponse(result *simulationResponse, request *simulationRequest, record signal, expectedEvidenceMode string) error {
	if result == nil || request == nil || result.SchemaVersion != "phoenix.rpc.aave-simulate-response.v5" || result.ChainID != 42161 || result.RequestID != request.RequestID || result.BlockNumber != record.Block || result.BlockHash != record.BlockHash || result.StateRoot != record.StateRoot || result.PrimaryProviderID != primaryProviderID || result.ConfirmationProviderID != nil || result.Quorum != 1 || result.EvidenceMode != expectedEvidenceMode || len(result.RouteID) != 66 || len(result.CalldataHash) != 64 || len(result.SimulationResultHash) != 64 || result.DeadlineUnixSeconds != request.DeadlineUnixSeconds {
		return errors.New("simulation evidence is incomplete")
	}
	calldata, err := hex.DecodeString(strings.TrimPrefix(result.CalldataHex, "0x"))
	if err != nil {
		return errors.New("simulation calldata identity mismatch")
	}
	calldataDigest := sha256.Sum256(calldata)
	if hex.EncodeToString(calldataDigest[:]) != result.CalldataHash {
		return errors.New("simulation calldata identity mismatch")
	}
	return nil
}

func batchGatewayErrorStatus(class string) int {
	switch class {
	case "state_request_budget_exhausted", "upstream_call_budget_exhausted", "provider_rate_limited", "secondary_rate_limited":
		return http.StatusTooManyRequests
	case "provider_unavailable":
		return http.StatusServiceUnavailable
	case "provider_timeout", "secondary_timeout":
		return http.StatusGatewayTimeout
	default:
		return http.StatusBadGateway
	}
}

func (s *Screener) buildExecutionCandidate(record signal, selected *liquidationEvaluation) (*executionCandidate, error) {
	if selected == nil || selected.Liquidation == nil || selected.Simulation == nil {
		return nil, errors.New("simulation result is missing")
	}
	liquidation := selected.Liquidation
	simulation := selected.Simulation
	repay, repayOK := newBigUint(liquidation.RepayAmount)
	liveMaximumInput := selected.LiveMaximumInput
	if liveMaximumInput == "" {
		liveMaximumInput = s.config.MaximumInputAmountWei
	}
	liveMaximum, liveMaximumOK := newBigUint(liveMaximumInput)
	if !repayOK || !liveMaximumOK || repay.Cmp(liveMaximum) > 0 || simulation.EvidenceMode != directForkEvidenceMode {
		return nil, errors.New("selected liquidation exceeds live size authority")
	}
	approvedAt := time.Now().UTC().Truncate(time.Second)
	approvalDeadline := approvedAt.Add(15 * time.Second)
	deadline := time.Unix(int64(simulation.DeadlineUnixSeconds), 0).UTC()
	if !deadline.After(approvalDeadline) {
		return nil, errors.New("simulation deadline is already stale")
	}
	identity := fmt.Sprintf("%s|%s|%d|%s", record.Borrower, simulation.RouteID, record.Block, simulation.SimulationResultHash)
	requestDigest := sha256.Sum256([]byte("request|" + identity))
	opportunityDigest := sha256.Sum256([]byte("opportunity|" + identity))
	legs := executionLegsFor(selected.Route, selected.MinimumUnwind)
	candidate := &executionCandidate{
		RequestID: deterministicUUID(requestDigest), OpportunityID: deterministicUUID(opportunityDigest),
		RouteID:      simulation.RouteID,
		RoutePayload: aaveRoutePayload{Borrower: record.Borrower, DebtAsset: liquidation.DebtAsset, CollateralAsset: liquidation.CollateralAsset, DebtAssetDecimals: liquidation.DebtAssetDecimals, DebtAssetPriceBase: liquidation.DebtAssetPriceBase, WETHPriceBase: liquidation.WETHPriceBase, MaximumInputWETHWei: selected.LiveMaximumInputWETH, MinimumProfitWETHWei: selected.MinimumProfitWETH, ReceiveAToken: false, MinimumCollateralReceived: selected.MinimumCollateral, MinimumUnwindOutput: selected.MinimumUnwind, MaximumAtlasBid: "0", EvidenceMode: simulation.EvidenceMode, StateRoot: record.StateRoot, ReleaseSHA: s.config.ReleaseSHA},
		SelectedSize: liquidation.RepayAmount, TokenPath: selected.Route.TokenPath, OriginRouter: aavePoolAddress,
		ExecutorAddress: s.config.ExecutorAddress, ExecutorCodeHash: s.config.ExecutorCodeHash,
		CalldataHash: simulation.CalldataHash, SimulationResultHash: simulation.SimulationResultHash,
		PinnedBlockNumber: record.Block, PinnedBlockHash: record.BlockHash,
		FlashAsset: liquidation.DebtAsset, FlashAmount: liquidation.RepayAmount, MaximumInputAmount: liveMaximumInput,
		MinimumProfit: selected.MinimumProfit, ExpectedProfit: simulation.RealizedProfit,
		Deadline: deadline, Legs: legs, GasLimit: simulation.EstimatedGasLimit,
		MaxFeePerGas: simulation.EstimatedMaxFeePerGasWei, MaxPriorityFeePerGas: s.config.MaximumPriorityFeeWei,
		ApprovedBy: "atlas-aave-hunter", ApprovedAt: approvedAt, ApprovalDeadline: approvalDeadline,
		PolicyVersion: "phoenix.live-canary-approval.v1",
	}
	plan, err := json.Marshal(struct {
		Schema         string `json:"schema"`
		Identity       string `json:"identity"`
		RouteID        string `json:"route_id"`
		SimulationHash string `json:"simulation_result_hash"`
		StateRoot      string `json:"state_root"`
		ReleaseSHA     string `json:"release_sha"`
	}{"phoenix.aave-execution-plan.v1", identity, candidate.RouteID, candidate.SimulationResultHash, record.StateRoot, s.config.ReleaseSHA})
	if err != nil {
		return nil, err
	}
	planDigest := sha256.Sum256(plan)
	candidate.PlanHash = hex.EncodeToString(planDigest[:])
	approval, err := candidate.approvalBody()
	if err != nil {
		return nil, err
	}
	approvalDigest := sha256.Sum256(approval)
	candidate.ApprovalDigest = hex.EncodeToString(approvalDigest[:])
	return candidate, nil
}

func (c *executionCandidate) approvalBody() ([]byte, error) {
	if c == nil {
		return nil, errors.New("execution candidate is missing")
	}
	return json.Marshal(approvalBody{
		SchemaVersion:        "phoenix.live-execution-request.v2",
		RequestID:            c.RequestID,
		OpportunityID:        c.OpportunityID,
		ChainID:              42161,
		RouteID:              c.RouteID,
		RouteFingerprint:     "AAVE_LIQUIDATION_V1",
		RouteType:            "AAVE_LIQUIDATION_V1",
		RoutePayload:         c.RoutePayload,
		SelectedSize:         c.SelectedSize,
		TokenPath:            c.TokenPath,
		OriginRouter:         c.OriginRouter,
		ExecutorAddress:      c.ExecutorAddress,
		ExecutorCodeHash:     c.ExecutorCodeHash,
		CalldataHash:         c.CalldataHash,
		SimulationResultHash: c.SimulationResultHash,
		PlanHash:             c.PlanHash,
		PinnedBlockNumber:    c.PinnedBlockNumber,
		PinnedBlockHash:      c.PinnedBlockHash,
		FlashAsset:           c.FlashAsset,
		FlashAmount:          c.FlashAmount,
		MaximumInputAmount:   c.MaximumInputAmount,
		MinimumProfit:        c.MinimumProfit,
		ExpectedProfit:       c.ExpectedProfit,
		DeadlineUnixSeconds:  c.Deadline.Unix(),
		Legs:                 c.Legs,
		GasLimit:             c.GasLimit,
		MaxFeePerGas:         c.MaxFeePerGas,
		MaxPriorityFeePerGas: c.MaxPriorityFeePerGas,
		ApprovedBy:           c.ApprovedBy,
		ApprovedAt:           c.ApprovedAt.Format(time.RFC3339),
		ApprovalDeadline:     c.ApprovalDeadline.Format(time.RFC3339),
		PolicyVersion:        c.PolicyVersion,
	})
}

func equalLiquidations(first, second []exactLiquidation) bool {
	return reflect.DeepEqual(first, second)
}

func generousUpperBound(value account, wethPriceText string) (*big.Int, error) {
	debt, ok := newBigUint(value.TotalDebtBase)
	if !ok || debt.Sign() <= 0 {
		return nil, errors.New("debt is invalid")
	}
	hf, ok := newBigUint(value.HealthFactorWAD)
	if !ok {
		return nil, errors.New("health factor is invalid")
	}
	wethPrice, ok := newBigUint(wethPriceText)
	if !ok || wethPrice.Sign() <= 0 {
		return nil, errors.New("WETH price is invalid")
	}
	closePercent := int64(50)
	if hf.Cmp(big.NewInt(950_000_000_000_000_000)) <= 0 {
		closePercent = 100
	}
	// Fifty percent of repay value is deliberately looser than any reviewed
	// Aave liquidation bonus. It is an optimistic rejection bound, never an
	// execution profit estimate.
	repayBase := new(big.Int).Mul(debt, big.NewInt(closePercent))
	repayBase.Div(repayBase, big.NewInt(100))
	upperBase := new(big.Int).Div(new(big.Int).Mul(repayBase, big.NewInt(50)), big.NewInt(100))
	upperWei := new(big.Int).Mul(upperBase, new(big.Int).Exp(big.NewInt(10), big.NewInt(18), nil))
	upperWei.Div(upperWei, wethPrice)
	return upperWei, nil
}

func newBigUint(value string) (*big.Int, bool) {
	result, ok := new(big.Int).SetString(value, 10)
	return result, ok && result.Sign() >= 0
}

func exactDebtUnit(liquidation *exactLiquidation) (*big.Int, *big.Int, *big.Int, bool) {
	if liquidation == nil || liquidation.DebtAssetDecimals > 36 {
		return nil, nil, nil, false
	}
	debtPrice, debtOK := newBigUint(liquidation.DebtAssetPriceBase)
	wethPrice, wethOK := newBigUint(liquidation.WETHPriceBase)
	unit := new(big.Int).Exp(big.NewInt(10), new(big.Int).SetUint64(uint64(liquidation.DebtAssetDecimals)), nil)
	if !debtOK || !wethOK || debtPrice.Sign() <= 0 || wethPrice.Sign() <= 0 {
		return nil, nil, nil, false
	}
	debt := strings.ToLower(liquidation.DebtAsset)
	supported := debt == wethAddress && liquidation.DebtAssetDecimals == 18 && debtPrice.Cmp(wethPrice) == 0 ||
		debt == usdcEAddress && liquidation.DebtAssetDecimals == 6
	if !supported {
		return nil, nil, nil, false
	}
	return debtPrice, wethPrice, unit, true
}

func wethToDebtFloor(wethWei string, liquidation *exactLiquidation) (*big.Int, bool) {
	weth, wethOK := newBigUint(wethWei)
	debtPrice, wethPrice, unit, unitOK := exactDebtUnit(liquidation)
	if !wethOK || !unitOK {
		return nil, false
	}
	numerator := new(big.Int).Mul(weth, wethPrice)
	numerator.Mul(numerator, unit)
	denominator := new(big.Int).Mul(big.NewInt(1_000_000_000_000_000_000), debtPrice)
	return numerator.Div(numerator, denominator), true
}

func wethToDebtCeil(wethWei string, liquidation *exactLiquidation) (*big.Int, bool) {
	weth, wethOK := newBigUint(wethWei)
	debtPrice, wethPrice, unit, unitOK := exactDebtUnit(liquidation)
	if !wethOK || !unitOK {
		return nil, false
	}
	numerator := new(big.Int).Mul(weth, wethPrice)
	numerator.Mul(numerator, unit)
	denominator := new(big.Int).Mul(big.NewInt(1_000_000_000_000_000_000), debtPrice)
	numerator.Add(numerator, new(big.Int).Sub(new(big.Int).Set(denominator), big.NewInt(1)))
	return numerator.Div(numerator, denominator), true
}

func debtToWETHFloor(debtRaw string, liquidation *exactLiquidation) (*big.Int, bool) {
	debt, debtOK := newBigUint(debtRaw)
	debtPrice, wethPrice, unit, unitOK := exactDebtUnit(liquidation)
	if !debtOK || !unitOK {
		return nil, false
	}
	numerator := new(big.Int).Mul(debt, debtPrice)
	numerator.Mul(numerator, big.NewInt(1_000_000_000_000_000_000))
	denominator := new(big.Int).Mul(unit, wethPrice)
	return numerator.Div(numerator, denominator), true
}

func minimumUnwindForReserve(outputDebt, reserveWETH *big.Int, liquidation *exactLiquidation) (*big.Int, bool) {
	if outputDebt == nil || reserveWETH == nil {
		return nil, false
	}
	reserveDebt, ok := wethToDebtFloor(reserveWETH.String(), liquidation)
	if !ok {
		return nil, false
	}
	minimum := new(big.Int).Sub(new(big.Int).Set(outputDebt), reserveDebt)
	if minimum.Sign() < 0 {
		minimum.SetInt64(0)
	}
	return minimum, true
}

func isReviewedWETHSize(value *big.Int) bool {
	if value == nil {
		return false
	}
	for _, size := range []string{
		"100000000000000", "250000000000000", "500000000000000",
		"1000000000000000", "2500000000000000", "5000000000000000",
		maximumReviewedInputWei,
	} {
		if value.String() == size {
			return true
		}
	}
	return false
}

// FailClosedExactAuthority revokes the durable Exact gate before a fatal
// observer boundary returns. It is intentionally narrower than a full
// Aave+Atlas disarm; the supervisor owns sustained-failure convergence.
func (s *Screener) FailClosedExactAuthority(class string) error {
	return s.recordError(class)
}

func (s *Screener) recordError(class string) error {
	s.mu.Lock()
	now := s.nowUTC()
	s.state.ProviderCircuitOpenUntilUnixMillis = 0
	s.state.ProviderRecoverySamples = nil
	s.state.LastPrimaryExactAt = nil
	s.state.IncompleteCount++
	s.state.LastErrorClass = class
	s.state.LastAttemptAt = &now
	err := s.persistStateLocked()
	s.mu.Unlock()
	if err != nil {
		return err
	}
	if sink, ok := s.config.SignalSink.(ProviderAuthoritySink); ok {
		ctx, cancel := context.WithTimeout(context.Background(), 5*time.Second)
		defer cancel()
		return sink.RecordProviderFailure(ctx, class, now)
	}
	return nil
}

func (s *Screener) recordProviderDegradation(class string) error {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := s.nowUTC()
	return s.recordProviderDegradationLocked(now, class)
}

func (s *Screener) recordProviderDegradationLocked(now time.Time, class string) error {
	if s.state.Counts == nil {
		s.state.Counts = make(map[string]uint64)
	}
	if s.state.Counts[providerDegradedSinceMillisKey] == 0 {
		s.state.Counts[providerDegradationTotalKey]++
		if classKey := providerDegradationClassKey(class); classKey != "" {
			s.state.Counts[classKey]++
		}
		s.state.Counts[providerDegradedSinceMillisKey] = uint64(now.UnixMilli())
	}
	s.state.Counts[providerRecoveryAttemptTotalKey]++
	s.state.Counts[providerLastDegradedAtMillisKey] = uint64(now.UnixMilli())
	s.state.IncompleteCount++
	s.state.LastErrorClass = class
	s.state.LastAttemptAt = &now
	return s.persistStateLocked()
}

func providerDegradationClassKey(class string) string {
	switch class {
	case "provider_disagreement":
		return providerDisagreementTotalKey
	case "provider_unavailable":
		return providerUnavailableTotalKey
	case "provider_timeout":
		return providerTimeoutTotalKey
	case "provider_rate_limited":
		return providerRateLimitedTotalKey
	default:
		return ""
	}
}

func (s *Screener) recordProviderRecoveryLocked(now time.Time, primary string, _ ...string) {
	if s.state.Counts == nil {
		s.state.Counts = make(map[string]uint64)
	}
	if primary == primaryProviderID {
		collecting := s.state.LastErrorClass != "" || s.state.Counts[providerDegradedSinceMillisKey] > 0
		if collecting && len(s.state.ProviderRecoverySamples) > 0 {
			last := s.state.ProviderRecoverySamples[len(s.state.ProviderRecoverySamples)-1].ObservedAt
			if !now.After(last) || now.Sub(last) > inMemoryProviderRecoveryWindow {
				s.state.ProviderRecoverySamples = nil
			}
		}
		s.state.ProviderRecoverySamples = append(s.state.ProviderRecoverySamples, ProviderRecoverySample{
			ObservedAt: now, PrimaryProvider: primary, Confirmation: nil, Quorum: 1,
		})
		if len(s.state.ProviderRecoverySamples) > 3 {
			s.state.ProviderRecoverySamples = append([]ProviderRecoverySample(nil), s.state.ProviderRecoverySamples[len(s.state.ProviderRecoverySamples)-3:]...)
		}
	}
	// A successful primary Exact proves the circuit is currently closed,
	// but does not restore Exact authority until all three samples exist.
	s.state.ProviderCircuitOpenUntilUnixMillis = 0
	degradedSince := s.state.Counts[providerDegradedSinceMillisKey]
	collecting := s.state.LastErrorClass != "" || degradedSince > 0
	if collecting && len(s.state.ProviderRecoverySamples) < 3 {
		return
	}
	if degradedSince > 0 {
		s.state.Counts[providerRecoverySuccessTotalKey]++
		s.state.Counts[providerLastRecoveryAtMillisKey] = uint64(now.UnixMilli())
		if uint64(now.UnixMilli()) >= degradedSince {
			duration := uint64(now.UnixMilli()) - degradedSince
			if duration == 0 {
				duration = 1
			}
			s.state.Counts[providerLastDegradedDurationKey] = duration
		}
		delete(s.state.Counts, providerDegradedSinceMillisKey)
	}
	delete(s.state.Counts, providerCurrentFailureStreakKey)
	s.state.LastErrorClass = ""
}

func (s *Screener) recordStartupAttempt(retry uint64, class string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := time.Now().UTC()
	s.state.LastAttemptAt = &now
	s.state.StartupRetryCount = retry
	if class != "" {
		s.state.LastErrorClass = class
	}
	_ = s.persistStateLocked()
}

func (s *Screener) clearStartupError() {
	s.mu.Lock()
	defer s.mu.Unlock()
	s.state.LastErrorClass = ""
	_ = s.persistStateLocked()
}

func classify(debt, hf string) string {
	if debt == "0" {
		return "no_debt"
	}
	value, ok := newUint64(hf)
	if !ok {
		return "incomplete"
	}
	if value < liquidatableHF {
		return "liquidatable"
	}
	if value < urgentHF {
		return "urgent"
	}
	if value < watchHF {
		return "watch"
	}
	return "debt_safe"
}

func newUint64(value string) (uint64, bool) {
	var result uint64
	if value == "" {
		return 0, false
	}
	for _, digit := range []byte(value) {
		if digit < '0' || digit > '9' || result > (^uint64(0)-uint64(digit-'0'))/10 {
			return 0, false
		}
		result = result*10 + uint64(digit-'0')
	}
	return result, true
}

func (s *Screener) loadState() error {
	path := filepath.Join(s.config.StateDir, "state.json")
	data, err := os.ReadFile(path)
	if errors.Is(err, os.ErrNotExist) {
		return s.persistState()
	}
	if err != nil {
		return err
	}
	var state State
	if err := json.Unmarshal(data, &state); err != nil {
		return err
	}
	if state.Schema != StateSchema || state.DiscoverySHA256 != s.config.DiscoverySHA256 || state.Cursor < s.config.StartingCursor || state.Counts == nil {
		return errors.New("existing hunter state is incompatible")
	}
	if state.RouteIneligible == nil {
		state.RouteIneligible = make(map[string]string)
	}
	if state.TailInvalidatedBlock == nil {
		state.TailInvalidatedBlock = make(map[string]uint64)
	}
	for borrower, invalidatedBlock := range state.TailInvalidatedBlock {
		if !addressPattern.MatchString(borrower) || invalidatedBlock == 0 {
			return errors.New("existing tail-invalidation state is invalid")
		}
	}
	for borrower, reason := range state.RouteIneligible {
		if !addressPattern.MatchString(borrower) {
			return errors.New("existing route-ineligible state is invalid")
		}
		switch reason {
		case "no_weth_debt", "no_supported_collateral", "supported_collateral_disabled", "unsupported_stable_weth_debt":
			// These reasons remain valid under the WETH-identity route universe.
		case "no_native_usdc_collateral", "native_usdc_not_collateral":
			// These legacy reasons were learned when native USDC was the only
			// supported collateral. Force a fresh exact screen after upgrade so
			// WETH-collateral borrowers cannot remain incorrectly deprioritized.
			delete(state.RouteIneligible, borrower)
		default:
			return errors.New("existing route-ineligible state is invalid")
		}
	}
	s.state = state
	if state.LastExactAdmissionAt != nil {
		s.lastExactAdmissionAt = state.LastExactAdmissionAt.UTC()
		s.hasDurableAdmission = true
	}
	return nil
}

func (s *Screener) loadBorrowerIndex() error {
	path := filepath.Join(s.config.StateDir, "borrower-index.ndjson")
	file, err := os.Open(path)
	if errors.Is(err, os.ErrNotExist) {
		return s.replayLegacySignals()
	}
	if err != nil {
		return err
	}
	defer file.Close()
	decoder := json.NewDecoder(bufio.NewReaderSize(file, 1<<20))
	for {
		var activity borrowerActivity
		if err := decoder.Decode(&activity); errors.Is(err, io.EOF) {
			break
		} else if err != nil {
			return err
		}
		if !addressPattern.MatchString(activity.Borrower) {
			return errors.New("borrower index contains an invalid identity")
		}
		s.refreshKnown[activity.Borrower] = true
		if activity.Active {
			s.debtBearing[activity.Borrower] = true
		} else {
			delete(s.debtBearing, activity.Borrower)
		}
	}
	for borrower := range s.refreshKnown {
		s.refreshOrder = append(s.refreshOrder, borrower)
	}
	sort.Strings(s.refreshOrder)
	s.state.DebtBearingCount = uint64(len(s.debtBearing))
	return nil
}

func (s *Screener) replayLegacySignals() error {
	path := filepath.Join(s.config.StateDir, "signals.ndjson")
	file, err := os.Open(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	defer file.Close()
	decoder := json.NewDecoder(bufio.NewReaderSize(file, 1<<20))
	for {
		var record signal
		if err := decoder.Decode(&record); errors.Is(err, io.EOF) {
			break
		} else if err != nil {
			return err
		}
		if !addressPattern.MatchString(record.Borrower) {
			return errors.New("legacy signal contains an invalid borrower")
		}
		s.refreshKnown[record.Borrower] = true
		if record.DebtBase != "0" {
			s.debtBearing[record.Borrower] = true
		} else {
			delete(s.debtBearing, record.Borrower)
		}
		s.applyHotSignal(record)
	}
	for borrower := range s.refreshKnown {
		s.refreshOrder = append(s.refreshOrder, borrower)
	}
	sort.Strings(s.refreshOrder)
	s.state.DebtBearingCount = uint64(len(s.debtBearing))
	return nil
}

func (s *Screener) loadHotSignals() error {
	path := filepath.Join(s.config.StateDir, "signals.ndjson")
	file, err := os.Open(path)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	defer file.Close()
	decoder := json.NewDecoder(bufio.NewReaderSize(file, 1<<20))
	for {
		var record signal
		if err := decoder.Decode(&record); errors.Is(err, io.EOF) {
			return nil
		} else if err != nil {
			return err
		}
		if !addressPattern.MatchString(record.Borrower) {
			return errors.New("signal contains an invalid borrower")
		}
		s.applyHotSignal(record)
	}
}

func (s *Screener) ensureHotMapsLocked() {
	if s.hotBorrowers == nil {
		s.hotBorrowers = make(map[string]string)
	}
	if s.hotDebtBase == nil {
		s.hotDebtBase = make(map[string]string)
	}
	if s.hotUpperPositive == nil {
		s.hotUpperPositive = make(map[string]bool)
	}
	if s.latestOutcome == nil {
		s.latestOutcome = make(map[string]string)
	}
	if s.lastExactAt == nil {
		s.lastExactAt = make(map[string]time.Time)
	}
	if s.firstLiquidatableAt == nil {
		s.firstLiquidatableAt = make(map[string]time.Time)
	}
	if s.state.Counts == nil {
		s.state.Counts = make(map[string]uint64)
	}
	if s.state.RouteIneligible == nil {
		s.state.RouteIneligible = make(map[string]string)
	}
}

func (s *Screener) applyHotSignal(record signal) {
	s.ensureHotMapsLocked()
	if invalidatedBlock, present := s.state.TailInvalidatedBlock[record.Borrower]; present {
		if record.Block < invalidatedBlock {
			return
		}
		delete(s.state.TailInvalidatedBlock, record.Borrower)
	}
	if record.Bucket == "liquidatable" || record.Bucket == "urgent" || record.Bucket == "watch" {
		s.hotBorrowers[record.Borrower] = record.HF
		s.hotDebtBase[record.Borrower] = record.DebtBase
		if record.Bucket == "liquidatable" {
			upper, upperOK := newBigUint(record.ZeroCostProfitUpperBoundWei)
			floor, floorOK := newBigUint(s.config.RetainedProfitFloorWei)
			s.hotUpperPositive[record.Borrower] = upperOK && floorOK && upper.Cmp(floor) > 0
		} else {
			delete(s.hotUpperPositive, record.Borrower)
		}
		if record.ExactDeferredReason == "" || s.latestOutcome[record.Borrower] == "" {
			s.latestOutcome[record.Borrower] = record.TerminalOutcome
		}
		if record.Bucket == "liquidatable" {
			if _, present := s.firstLiquidatableAt[record.Borrower]; !present {
				s.firstLiquidatableAt[record.Borrower] = record.ObservedAt
			}
		} else {
			delete(s.firstLiquidatableAt, record.Borrower)
			delete(s.firstLiquidatableMono, record.Borrower)
		}
		if record.StateRoot != "" {
			completedAt := record.ObservedAt
			if record.ExactCompletedAt != nil && !record.ExactCompletedAt.Before(record.ObservedAt) {
				completedAt = *record.ExactCompletedAt
			}
			previous, exists := s.lastExactAt[record.Borrower]
			if !exists || completedAt.After(previous) {
				s.lastExactAt[record.Borrower] = completedAt
			}
			if !s.hasDurableAdmission && completedAt.After(s.lastExactAdmissionAt) {
				s.lastExactAdmissionAt = completedAt
				s.state.LastExactAdmissionAt = &completedAt
			}
			delete(s.firstLiquidatableAt, record.Borrower)
			delete(s.firstLiquidatableMono, record.Borrower)
		}
	} else {
		delete(s.hotBorrowers, record.Borrower)
		delete(s.hotDebtBase, record.Borrower)
		delete(s.hotUpperPositive, record.Borrower)
		delete(s.latestOutcome, record.Borrower)
		delete(s.lastExactAt, record.Borrower)
	}
}

func (s *Screener) updateBorrowerActivityLocked(borrower string, active bool) error {
	if !addressPattern.MatchString(borrower) {
		return errors.New("borrower activity identity is invalid")
	}
	if s.debtBearing[borrower] == active && (active || s.refreshKnown[borrower]) {
		return nil
	}
	if err := appendJSON(filepath.Join(s.config.StateDir, "borrower-index.ndjson"), borrowerActivity{Borrower: borrower, Active: active}); err != nil {
		return err
	}
	if !s.refreshKnown[borrower] {
		s.refreshKnown[borrower] = true
		s.refreshOrder = append(s.refreshOrder, borrower)
	}
	if active {
		s.debtBearing[borrower] = true
	} else {
		delete(s.debtBearing, borrower)
	}
	s.state.DebtBearingCount = uint64(len(s.debtBearing))
	return nil
}

func (s *Screener) persistState() error {
	s.mu.Lock()
	defer s.mu.Unlock()
	return s.persistStateLocked()
}

func (s *Screener) persistStateLocked() error {
	path := filepath.Join(s.config.StateDir, "state.json")
	temporary, err := os.CreateTemp(s.config.StateDir, ".state-*.tmp")
	if err != nil {
		return err
	}
	name := temporary.Name()
	defer os.Remove(name)
	if err := temporary.Chmod(0o600); err != nil {
		temporary.Close()
		return err
	}
	if err := json.NewEncoder(temporary).Encode(s.state); err != nil {
		temporary.Close()
		return err
	}
	if err := temporary.Sync(); err != nil {
		temporary.Close()
		return err
	}
	if err := temporary.Close(); err != nil {
		return err
	}
	return os.Rename(name, path)
}

func appendJSON(path string, value any) error {
	file, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		return err
	}
	defer file.Close()
	if err := json.NewEncoder(file).Encode(value); err != nil {
		return err
	}
	return file.Sync()
}

func fileSHA256(path string) (string, error) {
	file, err := os.Open(path)
	if err != nil {
		return "", err
	}
	defer file.Close()
	hash := sha256.New()
	if _, err := io.Copy(hash, file); err != nil {
		return "", err
	}
	return hex.EncodeToString(hash.Sum(nil)), nil
}

type borrowerStream struct {
	file    *os.File
	decoder *json.Decoder
	started bool
}

func streamBorrowers(path string) (*borrowerStream, error) {
	file, err := os.Open(path)
	if err != nil {
		return nil, err
	}
	return &borrowerStream{file: file, decoder: json.NewDecoder(bufio.NewReaderSize(file, 1<<20))}, nil
}

func (s *borrowerStream) Next() (string, error) {
	if !s.started {
		token, err := s.decoder.Token()
		if err != nil || token != json.Delim('{') {
			return "", errors.New("discovery root is invalid")
		}
		for s.decoder.More() {
			key, _ := s.decoder.Token()
			if key == "borrowers" {
				array, err := s.decoder.Token()
				if err != nil || array != json.Delim('[') {
					return "", errors.New("discovery borrowers are invalid")
				}
				s.started = true
				break
			}
			var discard json.RawMessage
			if err := s.decoder.Decode(&discard); err != nil {
				return "", err
			}
		}
		if !s.started {
			return "", errors.New("discovery borrowers are missing")
		}
	}
	if !s.decoder.More() {
		return "", io.EOF
	}
	var address string
	if err := s.decoder.Decode(&address); err != nil {
		return "", err
	}
	if !addressPattern.MatchString(address) {
		return "", errors.New("discovery borrower is not canonical")
	}
	return address, nil
}

func (s *borrowerStream) Close() error { return s.file.Close() }
