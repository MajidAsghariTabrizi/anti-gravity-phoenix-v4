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
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
)

const (
	StateSchema                     = "phoenix.atlas-aave-hunter-state.v1"
	RequestSchema                   = "phoenix.rpc.aave-screen-request.v1"
	ResponseSchema                  = "phoenix.rpc.aave-screen-response.v1"
	DefaultBatch                    = 100
	MaximumBatch                    = 100
	watchHF                         = uint64(1_100_000_000_000_000_000)
	urgentHF                        = uint64(1_020_000_000_000_000_000)
	liquidatableHF                  = uint64(1_000_000_000_000_000_000)
	maximumResponse                 = 2 << 20
	gatewayReadyTimeout             = 90 * time.Second
	gatewayReadyPoll                = 5 * time.Second
	initialScreenOffset             = 10 * time.Second
	startupRetryTimeout             = 90 * time.Second
	maximumStartupRetries           = 3
	providerDegradationTotalKey     = "provider_retryable_degradation_total"
	providerRecoveryAttemptTotalKey = "provider_recovery_attempt_total"
	providerRecoverySuccessTotalKey = "provider_recovery_success_total"
	providerDegradedSinceMillisKey  = "provider_degraded_since_unix_millis"
	providerLastDegradedAtMillisKey = "provider_last_degraded_at_unix_millis"
	providerLastRecoveryAtMillisKey = "provider_last_recovery_at_unix_millis"
	providerLastDegradedDurationKey = "provider_last_degraded_duration_millis"
	providerCircuitOpenTotalKey     = "provider_circuit_open_total"
	providerCircuitSkippedTotalKey  = "provider_circuit_skipped_total"
	providerCircuitCooldown         = 5 * time.Minute
	gatewayBudgetCircuitCooldown    = 30 * time.Second
	hotRevisitCadence               = 10 * time.Second
	aaveSimulationBatchTimeout      = 55 * time.Second
	exactBorrowerCooldown           = 2 * time.Minute
	directForkEvidenceMode          = "DUAL_PROVIDER_FORK_VERIFIED"
	counterfactualForkEvidenceMode  = "DUAL_PROVIDER_COUNTERFACTUAL_FORK_VERIFIED"
	atlasCallbackEvidenceMode       = "DUAL_PROVIDER_ATLAS_CALLBACK_FORK_VERIFIED"
	maximumReviewedInputWei         = "10000000000000000"
	exactDeferredCooldownKey        = "exact_deferred_cooldown_total"
	exactDeferredRouteIneligibleKey = "exact_deferred_route_ineligible_total"
	exactRouteIneligibleObservedKey = "exact_route_ineligible_observed_total"
	hotRecheckTotalKey              = "hot_recheck_total"
	hotRecheckDeferredBudgetKey     = "hot_recheck_deferred_budget_total"
	exactEvalStartedKey             = "exact_eval_started_total"
	exactEvalCompletedKey           = "exact_eval_completed_total"
	exactEvalLatencySumKey          = "exact_eval_latency_millis_sum"
	exactEvalLatencyCountKey        = "exact_eval_latency_millis_count"
	liquidatableToExactSumKey       = "liquidatable_to_exact_millis_sum"
	liquidatableToExactCountKey     = "liquidatable_to_exact_millis_count"
	routeIneligibleRechecksKey      = "route_ineligible_rechecks_total"
	aavePoolAddress                 = "0x794a61358d6845594f94dc1db02a252b5b4814ad"
	wethAddress                     = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1"
	nativeUSDCAddress               = "0xaf88d065e77c8cc2239327c5edb3a432268e5831"
	zeroAddress                     = "0x0000000000000000000000000000000000000000"
	uniswapFactoryAddress           = "0x1f98431c8ad98523631ae4a59f267346ea31f984"
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
	SignalSink             SignalSink
}

type SignalSink interface {
	RecordAaveSignal(context.Context, signal) error
}

// LiveSizeAuthority is implemented by the durable Production sink. It keeps
// shadow evaluation bounded by the reviewed ladder while ensuring that live
// Candidate authority comes from the current economic/revenue-lane controls,
// not merely from the executor-wide reviewed ceiling.
type LiveSizeAuthority interface {
	CurrentAaveLiveMaximumInputAmount(context.Context) (string, error)
}

type State struct {
	Schema                             string            `json:"schema"`
	DiscoverySHA256                    string            `json:"discovery_sha256"`
	SourceAddressCount                 uint64            `json:"source_address_count"`
	Cursor                             uint64            `json:"cursor"`
	LastBlockNumber                    uint64            `json:"last_block_number"`
	LastBlockHash                      string            `json:"last_block_hash"`
	LastProviderPrimary                string            `json:"last_provider_primary"`
	LastProviderSecond                 string            `json:"last_provider_secondary"`
	LastBatchAt                        *time.Time        `json:"last_batch_at"`
	LastTailAt                         *time.Time        `json:"last_tail_at"`
	LastDualAgreementAt                *time.Time        `json:"last_dual_agreement_at,omitempty"`
	TailNextBlock                      uint64            `json:"tail_next_block"`
	DebtBearingCount                   uint64            `json:"debt_bearing_count"`
	Counts                             map[string]uint64 `json:"counts"`
	RouteIneligible                    map[string]string `json:"route_ineligible,omitempty"`
	ExactQueueCount                    uint64            `json:"exact_queue_count"`
	IncompleteCount                    uint64            `json:"incomplete_count"`
	LastErrorClass                     string            `json:"last_error_class,omitempty"`
	LastAttemptAt                      *time.Time        `json:"last_attempt_at,omitempty"`
	StartupRetryCount                  uint64            `json:"startup_retry_count,omitempty"`
	ProviderCircuitOpenTotal           uint64            `json:"provider_circuit_open_total"`
	ProviderCircuitSkippedTotal        uint64            `json:"provider_circuit_skipped_total"`
	ProviderCircuitOpenUntilUnixMillis int64             `json:"provider_circuit_open_until_unix_millis"`
	HotQueueSize                       uint64            `json:"hot_queue_size"`
	LiquidatableHotCount               uint64            `json:"liquidatable_hot_count"`
	UrgentHotCount                     uint64            `json:"urgent_hot_count"`
}

type Screener struct {
	config              Config
	client              *http.Client
	batchClient         *http.Client
	operationMu         sync.Mutex
	mu                  sync.Mutex
	state               State
	debtBearing         map[string]bool
	refreshKnown        map[string]bool
	refreshOrder        []string
	refreshCursor       int
	hotBorrowers        map[string]string
	hotDebtBase         map[string]string
	lastExactAt         map[string]time.Time
	firstLiquidatableAt map[string]time.Time
	wait                func(context.Context, time.Duration) bool
	now                 func() time.Time
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
	SchemaVersion string         `json:"schema_version"`
	ChainID       uint64         `json:"chain_id"`
	RequestID     string         `json:"request_id"`
	BlockNumber   uint64         `json:"block_number"`
	BlockHash     string         `json:"block_hash"`
	Primary       providerScreen `json:"primary"`
	Secondary     providerScreen `json:"secondary"`
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
	SecondaryProviderID  string   `json:"secondary_provider_id"`
	Borrowers            []string `json:"borrowers"`
}

type borrowerActivity struct {
	Borrower string `json:"borrower"`
	Active   bool   `json:"active"`
}

type exactLiquidation struct {
	DebtAsset              string             `json:"debt_asset"`
	CollateralAsset        string             `json:"collateral_asset"`
	RequestedRepayAmount   string             `json:"requested_repay_amount"`
	ActualRepayAmount      string             `json:"actual_repay_amount"`
	RepayAmount            string             `json:"repay_amount"`
	FlashPremiumAmount     string             `json:"flash_premium_amount"`
	SeizedCollateral       string             `json:"seized_collateral"`
	ProtocolFeeCollateral  string             `json:"protocol_fee_collateral"`
	LiquidatorCollateral   string             `json:"liquidator_collateral"`
	OracleUnwindOutputWETH string             `json:"oracle_unwind_output_weth"`
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
	SchemaVersion string        `json:"schema_version"`
	ChainID       uint64        `json:"chain_id"`
	RequestID     string        `json:"request_id"`
	BlockNumber   uint64        `json:"block_number"`
	BlockHash     string        `json:"block_hash"`
	StateRoot     string        `json:"state_root"`
	Primary       exactProvider `json:"primary"`
	Secondary     exactProvider `json:"secondary"`
}

type signal struct {
	Schema                      string              `json:"schema"`
	ObservedAt                  time.Time           `json:"observed_at"`
	Cursor                      uint64              `json:"cursor"`
	Block                       uint64              `json:"block_number"`
	BlockHash                   string              `json:"block_hash"`
	Borrower                    string              `json:"borrower"`
	DebtBase                    string              `json:"total_debt_base"`
	HF                          string              `json:"health_factor_wad"`
	Bucket                      string              `json:"bucket"`
	Authority                   bool                `json:"candidate_authority"`
	ExactDeferredReason         string              `json:"exact_deferred_reason,omitempty"`
	ExactRouteIneligibleReason  string              `json:"exact_route_ineligible_reason,omitempty"`
	ZeroCostProfitUpperBoundWei string              `json:"zero_cost_profit_upper_bound_wei,omitempty"`
	ExpectedNetPnLWei           string              `json:"expected_net_pnl_wei,omitempty"`
	ConservativeNetPnLWei       string              `json:"conservative_net_pnl_wei,omitempty"`
	RiskReserveAmountWei        string              `json:"risk_reserve_amount_wei,omitempty"`
	ExecutionCostWei            string              `json:"execution_cost_wei,omitempty"`
	EstimatedL1CostWei          string              `json:"estimated_l1_cost_wei,omitempty"`
	FlashPremiumWei             string              `json:"flash_premium_wei,omitempty"`
	StateRoot                   string              `json:"state_root,omitempty"`
	SelectedRoute               string              `json:"selected_route,omitempty"`
	TerminalOutcome             string              `json:"terminal_outcome"`
	AuthorityRejectionReason    string              `json:"authority_rejection_reason,omitempty"`
	SizeDiagnostics             []sizeDiagnostic    `json:"reviewed_size_diagnostics,omitempty"`
	ExecutionCandidate          *executionCandidate `json:"-"`
	AtlasCandidate              *atlasCandidate     `json:"-"`
}

type sizeDiagnostic struct {
	ReviewedSize             string `json:"reviewed_size"`
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
	RepayAmount               string `json:"repay_amount"`
	MaximumInputAmount        string `json:"maximum_input_amount"`
	LiveMaximumInputAmount    string `json:"live_maximum_input_amount"`
	Counterfactual            bool   `json:"counterfactual"`
	MinimumCollateralReceived string `json:"minimum_collateral_received"`
	MinimumUnwindOutput       string `json:"minimum_unwind_output"`
	MinimumProfit             string `json:"minimum_profit"`
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
	SchemaVersion             string `json:"schema_version"`
	ChainID                   uint64 `json:"chain_id"`
	RequestID                 string `json:"request_id"`
	BlockNumber               uint64 `json:"block_number"`
	BlockHash                 string `json:"block_hash"`
	StateRoot                 string `json:"state_root"`
	PrimaryProviderID         string `json:"primary_provider_id"`
	SecondaryProviderID       string `json:"secondary_provider_id"`
	EvidenceMode              string `json:"evidence_mode"`
	RouteID                   string `json:"route_id"`
	CalldataHex               string `json:"calldata_hex"`
	CalldataHash              string `json:"calldata_hash"`
	SimulationResultHash      string `json:"simulation_result_hash"`
	RealizedProfit            string `json:"realized_profit"`
	ConservativeNetPnL        string `json:"conservative_net_pnl"`
	EstimatedGasLimit         uint64 `json:"estimated_gas_limit"`
	EstimatedMaxFeePerGasWei  string `json:"estimated_max_fee_per_gas_wei"`
	EstimatedExecutionCostWei string `json:"estimated_execution_cost_wei"`
	EstimatedL1CostWei        string `json:"estimated_l1_cost_wei"`
	FlashPremiumWei           string `json:"flash_premium_wei"`
	DeadlineUnixSeconds       uint64 `json:"deadline_unix_seconds"`
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
	SchemaVersion       string                  `json:"schema_version"`
	ChainID             uint64                  `json:"chain_id"`
	RequestID           string                  `json:"request_id"`
	BlockNumber         uint64                  `json:"block_number"`
	BlockHash           string                  `json:"block_hash"`
	StateRoot           string                  `json:"state_root"`
	PrimaryProviderID   string                  `json:"primary_provider_id"`
	SecondaryProviderID string                  `json:"secondary_provider_id"`
	EvidenceMode        string                  `json:"evidence_mode"`
	Results             []simulationBatchResult `json:"results"`
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
	SelectedPool string
	Factory      string
	SelectedFee  uint32
	ZeroForOne   bool
	TokenPath    []string
}

type liquidationEvaluation struct {
	Liquidation       *exactLiquidation
	Route             liquidationRoute
	Simulation        *simulationResponse
	Expected          *big.Int
	Conservative      *big.Int
	RiskReserve       *big.Int
	ExecutionCost     *big.Int
	EstimatedL1Cost   *big.Int
	MinimumCollateral string
	MinimumUnwind     string
	MinimumProfit     string
	LiveMaximumInput  string
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
		lastExactAt:         make(map[string]time.Time),
		firstLiquidatableAt: make(map[string]time.Time),
		wait:                waitContext,
		now:                 func() time.Time { return time.Now().UTC() },
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
			s.recordError("rpc_gateway_not_ready")
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
					s.recordError(gatewayErrorClass(screenErr, "rpc_gateway_screen_failure"))
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
				s.recordError(gatewayErrorClass(err, "rpc_gateway_priority_failure"))
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
	hotNext := true
	for elapsed+hotRevisitCadence < window {
		if !s.waitDuration(ctx, hotRevisitCadence) {
			return errPriorityWindowStopped
		}
		elapsed += hotRevisitCadence
		var err error
		if hotNext {
			_, err = s.runHotPriority(ctx)
		} else {
			_, err = s.runTailPriority(ctx)
		}
		if err != nil {
			return err
		}
		hotNext = !hotNext
	}
	if remaining := window - elapsed; remaining > 0 && !s.waitDuration(ctx, remaining) {
		return errPriorityWindowStopped
	}
	return nil
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
	copy.HotQueueSize = uint64(len(s.hotBorrowers))
	copy.LiquidatableHotCount = 0
	copy.UrgentHotCount = 0
	for borrower, hf := range s.hotBorrowers {
		switch classify(s.hotDebtBase[borrower], hf) {
		case "liquidatable":
			copy.LiquidatableHotCount++
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

var exactLatencyBucketsMillis = [...]uint64{1_000, 5_000, 15_000, 30_000, 60_000, 120_000, 300_000}

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
	lines := []string{
		"# TYPE phoenix_aave_hot_queue_size gauge",
		fmt.Sprintf("phoenix_aave_hot_queue_size %d", state.HotQueueSize),
		"# TYPE phoenix_aave_liquidatable_hot_count gauge",
		fmt.Sprintf("phoenix_aave_liquidatable_hot_count %d", state.LiquidatableHotCount),
		"# TYPE phoenix_aave_urgent_hot_count gauge",
		fmt.Sprintf("phoenix_aave_urgent_hot_count %d", state.UrgentHotCount),
		"# TYPE phoenix_aave_hot_recheck_total counter",
		fmt.Sprintf("phoenix_aave_hot_recheck_total %d", state.Counts[hotRecheckTotalKey]),
		"# TYPE phoenix_aave_hot_recheck_deferred_budget_total counter",
		fmt.Sprintf("phoenix_aave_hot_recheck_deferred_budget_total %d", state.Counts[hotRecheckDeferredBudgetKey]),
		"# TYPE phoenix_aave_exact_eval_started_total counter",
		fmt.Sprintf("phoenix_aave_exact_eval_started_total %d", state.Counts[exactEvalStartedKey]),
		"# TYPE phoenix_aave_exact_eval_completed_total counter",
		fmt.Sprintf("phoenix_aave_exact_eval_completed_total %d", state.Counts[exactEvalCompletedKey]),
		"# TYPE phoenix_aave_route_ineligible_rechecks_total counter",
		fmt.Sprintf("phoenix_aave_route_ineligible_rechecks_total %d", state.Counts[routeIneligibleRechecksKey]),
		"# TYPE phoenix_aave_provider_circuit_deferrals_total counter",
		fmt.Sprintf("phoenix_aave_provider_circuit_deferrals_total %d", state.ProviderCircuitSkippedTotal),
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
	if !s.providerCircuitIsOpenLocked(now) {
		s.state.ProviderCircuitOpenTotal++
	}
	s.state.ProviderCircuitOpenUntilUnixMillis = now.Add(cooldown).UnixMilli()
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
	return true, nil
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
			return "provider_rate_limited", gatewayBudgetCircuitCooldown,
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
	if result.SchemaVersion != "phoenix.rpc.aave-tail-response.v1" || result.ChainID != 42161 || result.RequestID != requestID || result.FinalizedBlockNumber == 0 || len(result.FinalizedBlockHash) != 66 || result.PrimaryProviderID == "" || result.PrimaryProviderID == result.SecondaryProviderID || result.NextBlock != result.ToBlock+1 || len(result.Borrowers) > 1024 {
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
	for _, borrower := range result.Borrowers {
		if err := s.updateBorrowerActivityLocked(borrower, true); err != nil {
			return nil, err
		}
		// A new Borrow/Repay/Liquidation event is a material state change and
		// immediately invalidates both the short Exact cooldown and any
		// route-ineligibility learned from a prior Exact reserve snapshot.
		delete(s.lastExactAt, borrower)
		delete(s.state.RouteIneligible, borrower)
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
	if s.IsProviderCircuitOpen() {
		if err := s.recordProviderCircuitSkip(); err != nil {
			return err
		}
		return nil
	}
	if s.Snapshot().LastErrorClass != "" {
		return nil
	}
	borrowers := s.nextHotBatch()
	if len(borrowers) == 0 {
		return nil
	}
	return s.screen(ctx, borrowers, false, auction)
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

	leftDebtValue, leftDebtOK := newBigUint(leftDebt)
	rightDebtValue, rightDebtOK := newBigUint(rightDebt)
	leftHFValue, leftHFOK := newBigUint(leftHF)
	rightHFValue, rightHFOK := newBigUint(rightHF)

	compareDebt := func() (bool, bool) {
		if leftDebtOK != rightDebtOK {
			return leftDebtOK, true
		}
		if leftDebtOK && rightDebtOK && leftDebtValue.Cmp(rightDebtValue) != 0 {
			return leftDebtValue.Cmp(rightDebtValue) > 0, true
		}
		return false, false
	}
	compareHF := func() (bool, bool) {
		if leftHFOK != rightHFOK {
			return leftHFOK, true
		}
		if leftHFOK && rightHFOK && leftHFValue.Cmp(rightHFValue) != 0 {
			return leftHFValue.Cmp(rightHFValue) < 0, true
		}
		return false, false
	}

	// Once a borrower is liquidatable, larger repay capacity is the first
	// economic discriminator. For near-liquidation buckets, health factor
	// remains the first urgency discriminator.
	if leftRank == 0 {
		if result, decided := compareDebt(); decided {
			return result
		}
		if result, decided := compareHF(); decided {
			return result
		}
	} else {
		if result, decided := compareHF(); decided {
			return result
		}
		if result, decided := compareDebt(); decided {
			return result
		}
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

func (s *Screener) nextHotBatch() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	type entry struct {
		borrower        string
		hf              string
		debt            string
		routeIneligible bool
	}
	entries := make([]entry, 0, len(s.hotBorrowers))
	for borrower, hf := range s.hotBorrowers {
		entries = append(entries, entry{
			borrower:        borrower,
			hf:              hf,
			debt:            s.hotDebtBase[borrower],
			routeIneligible: s.state.RouteIneligible[borrower] != "",
		})
	}
	sort.Slice(entries, func(i, j int) bool {
		if entries[i].routeIneligible != entries[j].routeIneligible {
			return !entries[i].routeIneligible
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

func (s *Screener) screen(ctx context.Context, borrowers []string, advanceSeed bool, auction *observer.LedgerRecord) error {
	s.operationMu.Lock()
	defer s.operationMu.Unlock()
	cursor := s.Snapshot().Cursor
	requestID := fmt.Sprintf("aave-%d-%d", cursor, time.Now().UnixMilli())
	body, _ := json.Marshal(screenRequest{SchemaVersion: RequestSchema, ChainID: 42161, RequestID: requestID, Borrowers: borrowers})
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, s.config.GatewayURL+"/v1/aave/screen", bytes.NewReader(body))
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
	var result screenResponse
	if err := json.NewDecoder(io.LimitReader(response.Body, maximumResponse)).Decode(&result); err != nil {
		return err
	}
	if result.SchemaVersion != ResponseSchema || result.ChainID != 42161 || result.RequestID != requestID || result.BlockNumber == 0 || len(result.BlockHash) != 66 || result.Primary.ProviderID == result.Secondary.ProviderID || result.Primary.WETHPriceBase != result.Secondary.WETHPriceBase || len(result.Primary.Accounts) != len(borrowers) || len(result.Secondary.Accounts) != len(borrowers) {
		return errors.New("gateway Aave evidence is incomplete")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	s.ensureHotMapsLocked()
	exactAuthorityWasDegraded := s.state.LastErrorClass != ""
	for _, index := range prioritizedAccountOrder(result.Primary.Accounts) {
		primary := result.Primary.Accounts[index]
		if primary != result.Secondary.Accounts[index] || primary.Borrower != borrowers[index] {
			return errors.New("gateway Aave providers disagree")
		}
		bucket := classify(primary.TotalDebtBase, primary.HealthFactorWAD)
		if bucket == "liquidatable" || bucket == "urgent" || bucket == "watch" {
			s.hotBorrowers[primary.Borrower] = primary.HealthFactorWAD
			s.hotDebtBase[primary.Borrower] = primary.TotalDebtBase
			if bucket == "liquidatable" {
				if _, present := s.firstLiquidatableAt[primary.Borrower]; !present {
					s.firstLiquidatableAt[primary.Borrower] = s.nowUTC()
				}
			} else {
				delete(s.firstLiquidatableAt, primary.Borrower)
			}
		} else {
			delete(s.hotBorrowers, primary.Borrower)
			delete(s.hotDebtBase, primary.Borrower)
			delete(s.lastExactAt, primary.Borrower)
			delete(s.firstLiquidatableAt, primary.Borrower)
			delete(s.state.RouteIneligible, primary.Borrower)
		}
		if err := s.updateBorrowerActivityLocked(primary.Borrower, primary.TotalDebtBase != "0"); err != nil {
			return err
		}
		record := signal{Schema: "phoenix.atlas-aave-hunting-signal.v1", ObservedAt: time.Now().UTC(), Cursor: cursor + uint64(index), Block: result.BlockNumber, BlockHash: result.BlockHash, Borrower: primary.Borrower, DebtBase: primary.TotalDebtBase, HF: primary.HealthFactorWAD, Bucket: bucket, Authority: false, TerminalOutcome: "prefiltered"}
		if bucket == "liquidatable" {
			upper, upperErr := generousUpperBound(primary, result.Primary.WETHPriceBase)
			if upperErr != nil {
				bucket = "incomplete"
				record.Bucket = bucket
				record.TerminalOutcome = "incomplete"
			} else {
				record.ZeroCostProfitUpperBoundWei = upper.String()
				floor, _ := newBigUint(s.config.RetainedProfitFloorWei)
				if upper.Cmp(floor) <= 0 {
					record.TerminalOutcome = "economic_rejection"
				} else {
					record.TerminalOutcome = "exact_pending"
				}
			}
		}
		if record.TerminalOutcome == "exact_pending" && !exactAuthorityWasDegraded {
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
			} else {
				// Collateral supply/withdraw and collateral-enable changes are not
				// present in the debt tail. Re-probe those route reasons after the
				// normal exact cooldown instead of deferring them forever.
				if knownRouteIneligible {
					delete(s.state.RouteIneligible, primary.Borrower)
					s.state.Counts[routeIneligibleRechecksKey]++
				}
				exactStartedAt := s.nowUTC()
				s.state.Counts[exactEvalStartedKey]++
				s.mu.Unlock()
				exactRecord, exactErr := s.resolveExact(ctx, record, auction)
				s.mu.Lock()
				if exactErr != nil {
					return exactErr
				}
				exactCompletedAt := s.nowUTC()
				s.state.Counts[exactEvalCompletedKey]++
				s.observeDurationLocked(
					exactEvalLatencySumKey,
					exactEvalLatencyCountKey,
					"exact_eval_latency_millis_bucket_le_",
					exactCompletedAt.Sub(exactStartedAt),
				)
				if firstObserved, present := s.firstLiquidatableAt[primary.Borrower]; present {
					s.observeDurationLocked(
						liquidatableToExactSumKey,
						liquidatableToExactCountKey,
						"liquidatable_to_exact_millis_bucket_le_",
						exactCompletedAt.Sub(firstObserved),
					)
					delete(s.firstLiquidatableAt, primary.Borrower)
				}
				record = exactRecord
				s.lastExactAt[primary.Borrower] = now
				if record.ExactRouteIneligibleReason != "" {
					s.state.RouteIneligible[primary.Borrower] = record.ExactRouteIneligibleReason
					if s.state.Counts == nil {
						s.state.Counts = make(map[string]uint64)
					}
					s.state.Counts[exactRouteIneligibleObservedKey]++
				} else {
					delete(s.state.RouteIneligible, primary.Borrower)
				}
			}
		}
		if err := appendJSON(filepath.Join(s.config.StateDir, "signals.ndjson"), record); err != nil {
			return err
		}
		if s.config.SignalSink != nil {
			s.mu.Unlock()
			sinkErr := s.config.SignalSink.RecordAaveSignal(ctx, record)
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
	}
	now := time.Now().UTC()
	if advanceSeed {
		s.state.Cursor += uint64(len(borrowers))
	}
	s.state.LastBlockNumber = result.BlockNumber
	s.state.LastBlockHash = result.BlockHash
	s.state.LastProviderPrimary = result.Primary.ProviderID
	s.state.LastProviderSecond = result.Secondary.ProviderID
	s.state.LastBatchAt = &now
	s.state.LastDualAgreementAt = &now
	s.recordProviderRecoveryLocked(now)
	return s.persistStateLocked()
}

func equalExactReserves(first, second []exactReserve) bool {
	if len(first) != len(second) {
		return false
	}
	for index := range first {
		if first[index] != second[index] {
			return false
		}
	}
	return true
}

func equalExactProviders(first, second exactProvider) bool {
	return first.PoolCodeHash == second.PoolCodeHash &&
		first.PoolImplementation == second.PoolImplementation &&
		first.PoolImplementationCodeHash == second.PoolImplementationCodeHash &&
		first.UserConfiguration == second.UserConfiguration &&
		first.UserEModeCategory == second.UserEModeCategory &&
		first.EModeCollateralBitmap == second.EModeCollateralBitmap &&
		first.EModeLiquidationBonusBPS == second.EModeLiquidationBonusBPS &&
		first.FlashPremiumBPS == second.FlashPremiumBPS &&
		first.Account == second.Account &&
		equalExactReserves(first.Reserves, second.Reserves) &&
		equalLiquidations(first.Liquidations, second.Liquidations)
}

func exactRouteIneligibleReason(reserves []exactReserve) string {
	var wethDebt *big.Int
	stableWETHDebt := false
	hasSupportedCollateral := false
	hasEnabledSupportedCollateral := false

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
			if balance.Sign() > 0 {
				hasSupportedCollateral = true
				hasEnabledSupportedCollateral = hasEnabledSupportedCollateral || reserve.UsageAsCollateralEnabled
			}
		case nativeUSDCAddress:
			balance, balanceOK := newBigUint(reserve.CurrentATokenBalance)
			if !balanceOK {
				return ""
			}
			if balance.Sign() > 0 {
				hasSupportedCollateral = true
				hasEnabledSupportedCollateral = hasEnabledSupportedCollateral || reserve.UsageAsCollateralEnabled
			}
		}
	}

	if stableWETHDebt {
		return "unsupported_stable_weth_debt"
	}
	if wethDebt == nil || wethDebt.Sign() == 0 {
		return "no_weth_debt"
	}
	if !hasSupportedCollateral {
		return "no_supported_collateral"
	}
	if !hasEnabledSupportedCollateral {
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
	if result.SchemaVersion != "phoenix.rpc.aave-exact-response.v3" || result.ChainID != 42161 || result.RequestID != requestID || result.BlockNumber == 0 || result.BlockHash == "" || result.StateRoot == "" || result.Primary.ProviderID == result.Secondary.ProviderID || !equalExactProviders(result.Primary, result.Secondary) {
		return record, errors.New("exact Aave provider evidence is incomplete")
	}
	record.Block = result.BlockNumber
	record.BlockHash = result.BlockHash
	record.StateRoot = result.StateRoot
	if len(result.Primary.Liquidations) == 0 {
		record.ExactRouteIneligibleReason = exactRouteIneligibleReason(result.Primary.Reserves)
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
	record.FlashPremiumWei = selected.Liquidation.FlashPremiumAmount
	selectedRepay, selectedRepayOK := newBigUint(selected.Liquidation.RepayAmount)
	liveMaximum, liveMaximumOK := newBigUint(liveMaximumInput)
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
		return record, nil
	}
	if selected.Simulation.EvidenceMode != directForkEvidenceMode {
		return record, errors.New("live-authorized size lacks direct fork evidence")
	}
	if auction != nil {
		atlas, atlasErr := s.buildAtlasCandidate(ctx, record, selected, auction)
		if atlasErr != nil {
			return record, atlasErr
		}
		if atlas == nil {
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
	if result.SchemaVersion != "phoenix.rpc.aave-exact-response.v3" || result.ChainID != 42161 || result.RequestID != requestID || result.BlockNumber == 0 || result.BlockHash == "" || result.StateRoot == "" || result.Primary.ProviderID == result.Secondary.ProviderID || !equalExactProviders(result.Primary, result.Secondary) {
		return exactResponse{}, errors.New("fresh exact Aave provider evidence is incomplete")
	}
	return result, nil
}

func (s *Screener) validateLiquidationVariants(liquidations []exactLiquidation, flashPremiumBPS uint64) error {
	maximumInput, maximumOK := newBigUint(maximumReviewedInputWei)
	if !maximumOK || maximumInput.Sign() <= 0 || flashPremiumBPS == 0 || flashPremiumBPS > s.config.FlashPremiumBPS || len(liquidations) == 0 || len(liquidations) > 14 {
		return errors.New("exact liquidation bounds are invalid")
	}
	previousByCollateral := make(map[string]*big.Int)
	countByCollateral := make(map[string]int)
	for index := range liquidations {
		liquidation := &liquidations[index]
		collateral := strings.ToLower(liquidation.CollateralAsset)
		requested, requestedOK := newBigUint(liquidation.RequestedRepayAmount)
		actual, actualOK := newBigUint(liquidation.ActualRepayAmount)
		repay, repayOK := newBigUint(liquidation.RepayAmount)
		premium, premiumOK := newBigUint(liquidation.FlashPremiumAmount)
		if !requestedOK || !actualOK || !repayOK || !premiumOK || requested.Sign() <= 0 || actual.Sign() <= 0 || actual.Cmp(repay) != 0 || actual.Cmp(requested) != 0 || actual.Cmp(maximumInput) > 0 || strings.ToLower(liquidation.DebtAsset) != wethAddress || collateral != wethAddress && collateral != nativeUSDCAddress || !addressPattern.MatchString(collateral) {
			return errors.New("exact liquidation variant is invalid")
		}
		countByCollateral[collateral]++
		if countByCollateral[collateral] > 7 {
			return errors.New("exact liquidation grid exceeds its collateral bound")
		}
		if previous := previousByCollateral[collateral]; previous != nil && actual.Cmp(previous) <= 0 {
			return errors.New("exact liquidation variants are not strictly increasing")
		}
		expectedPremium := aavePercentMul(actual, flashPremiumBPS)
		if premium.Cmp(expectedPremium) != 0 {
			return errors.New("exact flash premium is inconsistent")
		}
		previousByCollateral[collateral] = new(big.Int).Set(actual)
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
				diagnostics[len(diagnostics)-1].FinalRejectionReason = "gross_edge_below_retained_profit_gate"
				continue
			}
			probes = append(probes, probe)
			requests = append(requests, s.newSimulationRequest(record, probe.Liquidation, simulationRequest{
				MinimumCollateralReceived: probe.MinimumCollateral,
				MinimumUnwindOutput:       "1",
				MinimumProfit:             s.config.RetainedProfitFloorWei,
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
		return nil, true, diagnostics
	}
	viable := make([]*liquidationEvaluation, 0, len(outcomes))
	for index, outcome := range outcomes {
		if outcome.Err != nil {
			hadSimulationFailure = true
			diagnostics[diagnosticIndex[liquidationDiagnosticKey(probes[index].Liquidation, probes[index].Route)]].FinalRejectionReason = "fork_simulation_failed"
			continue
		}
		evaluation, evaluationErr := s.evaluateLiquidationProbe(probes[index], outcome.Response, liveMaximumInput)
		if evaluationErr != nil {
			hadSimulationFailure = true
			diagnostics[diagnosticIndex[liquidationDiagnosticKey(probes[index].Liquidation, probes[index].Route)]].FinalRejectionReason = "fork_economics_invalid"
			continue
		}
		if evaluation != nil {
			updateSizeDiagnostic(&diagnostics[diagnosticIndex[liquidationDiagnosticKey(evaluation.Liquidation, evaluation.Route)]], evaluation, floor)
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
	if err != nil || freshExact.Primary.FlashPremiumBPS != originalFlashPremiumBPS || !equalLiquidations(freshExact.Primary.Liquidations, liquidations) {
		return nil, true, diagnostics
	}
	if err := s.validateLiquidationVariants(freshExact.Primary.Liquidations, freshExact.Primary.FlashPremiumBPS); err != nil {
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
			ExpectedProfit:            evaluation.Simulation.RealizedProfit,
			SelectedPool:              evaluation.Route.SelectedPool,
			SelectedFactory:           evaluation.Route.Factory,
			SelectedFee:               evaluation.Route.SelectedFee,
			ZeroForOne:                evaluation.Route.ZeroForOne,
		}, deadline, liveMaximumInput))
	}
	outcomes, err = s.simulateExactBatch(ctx, record, requests)
	if err != nil {
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
		exactEdge := new(big.Int).Sub(new(big.Int).Set(route.Output), repay)
		exactEdge.Sub(exactEdge, flash)
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
			return
		}
	}
}

func newSizeDiagnostic(probe *liquidationProbe, liveMaximumText string) sizeDiagnostic {
	diagnostic := sizeDiagnostic{
		ReviewedSize:             probe.Liquidation.RepayAmount,
		Route:                    probe.Route.Name,
		FlashPremiumWei:          probe.Liquidation.FlashPremiumAmount,
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
	flash, flashOK := newBigUint(probe.Liquidation.FlashPremiumAmount)
	liveMaximum, liveOK := newBigUint(liveMaximumText)
	if repayOK && flashOK {
		diagnostic.GrossLiquidationEdgeWei = new(big.Int).Add(new(big.Int).Set(probe.ExactEdge), flash).String()
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
	diagnostic.FinalRejectionReason = ""
}

func (s *Screener) evaluateLiquidationProbe(probe *liquidationProbe, simulation *simulationResponse, liveMaximumInput string) (*liquidationEvaluation, error) {
	if probe == nil || probe.Liquidation == nil || probe.Route.Output == nil || probe.ExactEdge == nil {
		return nil, errors.New("liquidation probe is incomplete")
	}
	realized, cost, l1Cost, err := s.boundedSimulationEconomics(simulation, probe.Liquidation)
	if err != nil {
		return nil, err
	}
	if realized.Cmp(probe.ExactEdge) != 0 {
		return nil, errors.New("exact quote and fork realization disagree")
	}
	expected, err := authoritativeGatewayNet(simulation, realized, cost, new(big.Int))
	if err != nil {
		return nil, err
	}
	floor, _ := newBigUint(s.config.RetainedProfitFloorWei)
	reserve, conservative, minimumUnwind := profitEdgeReserve(expected, probe.Route.Output, s.config.EconomicReserveBPS)
	if conservative.Cmp(floor) <= 0 || minimumUnwind.Sign() <= 0 {
		return nil, nil
	}
	return &liquidationEvaluation{
		Liquidation: probe.Liquidation, Route: probe.Route, Simulation: simulation,
		Expected: expected, Conservative: conservative, RiskReserve: reserve,
		ExecutionCost: cost, EstimatedL1Cost: l1Cost,
		MinimumCollateral: probe.MinimumCollateral, MinimumUnwind: minimumUnwind.String(),
		MinimumProfit:    strictMinimumProfit(floor, cost).String(),
		LiveMaximumInput: liveMaximumInput,
	}, nil
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
	finalReserve, finalConservative, finalMinimumUnwind := profitEdgeReserve(finalExpected, probe.Route.Output, s.config.EconomicReserveBPS)
	requiredMinimumProfit := strictMinimumProfit(floor, finalCost)
	if finalConservative.Cmp(floor) <= 0 || finalMinimumUnwind.Sign() <= 0 {
		return nil, nil, nil
	}
	if minimumUnwind.Cmp(finalMinimumUnwind) >= 0 && minimumProfit.Cmp(requiredMinimumProfit) >= 0 {
		return &liquidationEvaluation{
			Liquidation: probe.Liquidation, Route: probe.Route, Simulation: simulation,
			Expected: finalExpected, Conservative: finalConservative, RiskReserve: finalReserve,
			ExecutionCost: finalCost, EstimatedL1Cost: finalL1Cost,
			MinimumCollateral: probe.MinimumCollateral, MinimumUnwind: probe.MinimumUnwind,
			MinimumProfit:    probe.MinimumProfit,
			LiveMaximumInput: probe.LiveMaximumInput,
		}, nil, nil
	}
	retry := *probe
	retry.MinimumUnwind = finalMinimumUnwind.String()
	retry.MinimumProfit = requiredMinimumProfit.String()
	return nil, &retry, nil
}

func liquidationRoutesFor(liquidation *exactLiquidation) ([]liquidationRoute, error) {
	if liquidation == nil || strings.ToLower(liquidation.DebtAsset) != wethAddress {
		return nil, errors.New("unsupported exact debt route")
	}
	if strings.ToLower(liquidation.CollateralAsset) == wethAddress {
		output, ok := newBigUint(liquidation.LiquidatorCollateral)
		if !ok || output.Sign() <= 0 {
			return nil, errors.New("exact identity-route output is invalid")
		}
		return []liquidationRoute{{
			Name: "WETH_IDENTITY", Output: output, SelectedPool: zeroAddress,
			Factory: zeroAddress, TokenPath: []string{wethAddress},
		}}, nil
	}
	if strings.ToLower(liquidation.CollateralAsset) != nativeUSDCAddress {
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
		zeroForOne, directionOK := uniswapZeroForOne(nativeUSDCAddress, token0, token1)
		identityValid := addressPattern.MatchString(pool) && factory == uniswapFactoryAddress &&
			directionOK &&
			(quote.Fee == 100 || quote.Fee == 500 || quote.Fee == 3_000) &&
			quote.ZeroForOne == zeroForOne && !seen[pool]
		if !identityValid || !outputOK || output.Sign() <= 0 {
			return nil, errors.New("exact route quotation identity is invalid")
		}
		seen[pool] = true
		routes = append(routes, liquidationRoute{
			Name: fmt.Sprintf("UNISWAP_V3_%d", quote.Fee), Output: output,
			SelectedPool: pool, Factory: factory, SelectedFee: quote.Fee,
			ZeroForOne: zeroForOne, TokenPath: []string{nativeUSDCAddress, wethAddress},
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
		TokenIn: nativeUSDCAddress, TokenOut: wethAddress,
		Fee: route.SelectedFee, ZeroForOne: route.ZeroForOne, MinAmountOut: minimumUnwind,
	}}
}

func (s *Screener) boundedSimulationEconomics(simulation *simulationResponse, liquidation *exactLiquidation) (*big.Int, *big.Int, *big.Int, error) {
	if simulation == nil || simulation.EstimatedGasLimit == 0 || simulation.EstimatedGasLimit > s.config.MaximumGasLimit {
		return nil, nil, nil, errors.New("bounded simulation gas is invalid")
	}
	realized, realizedOK := newBigUint(simulation.RealizedProfit)
	cost, costOK := newBigUint(simulation.EstimatedExecutionCostWei)
	l1Cost, l1OK := newBigUint(simulation.EstimatedL1CostWei)
	flash, flashOK := newBigUint(simulation.FlashPremiumWei)
	expectedFlash, expectedFlashOK := newBigUint(liquidation.FlashPremiumAmount)
	maxFee, maxFeeOK := newBigUint(s.config.MaximumFeePerGasWei)
	estimatedMaxFee, estimatedMaxFeeOK := newBigUint(simulation.EstimatedMaxFeePerGasWei)
	if !realizedOK || !costOK || !l1OK || !flashOK || !expectedFlashOK || !maxFeeOK || !estimatedMaxFeeOK || cost.Sign() <= 0 || estimatedMaxFee.Sign() <= 0 || flash.Cmp(expectedFlash) != 0 || l1Cost.Cmp(cost) > 0 || estimatedMaxFee.Cmp(maxFee) > 0 {
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
	partial.SchemaVersion = "phoenix.rpc.aave-simulate-request.v3"
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
	partial.RepayAmount = liquidation.RepayAmount
	repay, repayOK := newBigUint(liquidation.RepayAmount)
	liveMaximum, liveOK := newBigUint(liveMaximumInput)
	partial.LiveMaximumInputAmount = liveMaximumInput
	partial.Counterfactual = repayOK && liveOK && repay.Cmp(liveMaximum) > 0
	if partial.Counterfactual {
		partial.MaximumInputAmount = maximumReviewedInputWei
	} else {
		partial.MaximumInputAmount = liveMaximumInput
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
		SchemaVersion: "phoenix.rpc.aave-simulate-batch-request.v2",
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
	if result.SchemaVersion != "phoenix.rpc.aave-simulate-batch-response.v2" || result.ChainID != 42161 || result.RequestID != requestID || result.BlockNumber != record.Block || result.BlockHash != record.BlockHash || result.StateRoot != record.StateRoot || result.PrimaryProviderID == "" || result.PrimaryProviderID == result.SecondaryProviderID || result.EvidenceMode != expectedBatchEvidenceMode || len(result.Results) != len(simulations) {
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
	if result == nil || request == nil || result.SchemaVersion != "phoenix.rpc.aave-simulate-response.v3" || result.ChainID != 42161 || result.RequestID != request.RequestID || result.BlockNumber != record.Block || result.BlockHash != record.BlockHash || result.StateRoot != record.StateRoot || result.PrimaryProviderID == "" || result.PrimaryProviderID == result.SecondaryProviderID || result.EvidenceMode != expectedEvidenceMode || len(result.RouteID) != 66 || len(result.CalldataHash) != 64 || len(result.SimulationResultHash) != 64 || result.DeadlineUnixSeconds != request.DeadlineUnixSeconds {
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
		RoutePayload: aaveRoutePayload{Borrower: record.Borrower, DebtAsset: liquidation.DebtAsset, CollateralAsset: liquidation.CollateralAsset, ReceiveAToken: false, MinimumCollateralReceived: selected.MinimumCollateral, MinimumUnwindOutput: selected.MinimumUnwind, MaximumAtlasBid: "0", EvidenceMode: simulation.EvidenceMode, StateRoot: record.StateRoot, ReleaseSHA: s.config.ReleaseSHA},
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

func (s *Screener) recordError(class string) {
	s.mu.Lock()
	defer s.mu.Unlock()
	now := s.nowUTC()
	s.state.ProviderCircuitOpenUntilUnixMillis = 0
	s.state.IncompleteCount++
	s.state.LastErrorClass = class
	s.state.LastAttemptAt = &now
	_ = s.persistStateLocked()
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
		s.state.Counts[providerDegradedSinceMillisKey] = uint64(now.UnixMilli())
	}
	s.state.Counts[providerRecoveryAttemptTotalKey]++
	s.state.Counts[providerLastDegradedAtMillisKey] = uint64(now.UnixMilli())
	s.state.IncompleteCount++
	s.state.LastErrorClass = class
	s.state.LastAttemptAt = &now
	return s.persistStateLocked()
}

func (s *Screener) recordProviderRecoveryLocked(now time.Time) {
	if s.state.Counts == nil {
		s.state.Counts = make(map[string]uint64)
	}
	s.state.ProviderCircuitOpenUntilUnixMillis = 0
	degradedSince := s.state.Counts[providerDegradedSinceMillisKey]
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
	if record.Bucket == "liquidatable" || record.Bucket == "urgent" || record.Bucket == "watch" {
		s.hotBorrowers[record.Borrower] = record.HF
		s.hotDebtBase[record.Borrower] = record.DebtBase
		if record.StateRoot != "" {
			previous, exists := s.lastExactAt[record.Borrower]
			if !exists || record.ObservedAt.After(previous) {
				s.lastExactAt[record.Borrower] = record.ObservedAt
			}
		}
	} else {
		delete(s.hotBorrowers, record.Borrower)
		delete(s.hotDebtBase, record.Borrower)
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
