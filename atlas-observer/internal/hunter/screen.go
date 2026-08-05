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
	"net/http"
	"os"
	"path/filepath"
	"regexp"
	"sort"
	"strings"
	"sync"
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
)

const (
	StateSchema            = "phoenix.atlas-aave-hunter-state.v1"
	RequestSchema          = "phoenix.rpc.aave-screen-request.v1"
	ResponseSchema         = "phoenix.rpc.aave-screen-response.v1"
	DefaultBatch           = 100
	MaximumBatch           = 100
	watchHF                = uint64(1_100_000_000_000_000_000)
	urgentHF               = uint64(1_020_000_000_000_000_000)
	liquidatableHF         = uint64(1_000_000_000_000_000_000)
	maximumResponse        = 2 << 20
	gatewayReadyTimeout    = 90 * time.Second
	gatewayReadyPoll       = 5 * time.Second
	initialScreenOffset    = 10 * time.Second
	startupRetryTimeout    = 90 * time.Second
	maximumStartupRetries  = 3
	aavePoolAddress        = "0x794a61358d6845594f94dc1db02a252b5b4814ad"
	wethAddress            = "0x82af49447d8a07e3bd95bd0d56f35241523fbab1"
	nativeUSDCAddress      = "0xaf88d065e77c8cc2239327c5edb3a432268e5831"
	uniswapFactoryAddress  = "0x1f98431c8ad98523631ae4a59f267346ea31f984"
	uniswapPool500Address  = "0xc6962004f452be9203591991d15f6b388e09e8d0"
	uniswapPool3000Address = "0xc473e2aee3441bf9240be85eb122abb059a3b57c"
)

var addressPattern = regexp.MustCompile(`^0x[0-9a-f]{40}$`)
var releaseSHAPattern = regexp.MustCompile(`^[0-9a-f]{40}$`)
var errorClassPattern = regexp.MustCompile(`^[a-z][a-z0-9_]{0,63}$`)

type Config struct {
	DiscoveryPath          string
	DiscoverySHA256        string
	StateDir               string
	GatewayURL             string
	StartingCursor         uint64
	BatchSize              int
	Pace                   time.Duration
	RetainedProfitFloorWei string
	MaximumGasLimit        uint64
	MaximumFeePerGasWei    string
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

type State struct {
	Schema              string            `json:"schema"`
	DiscoverySHA256     string            `json:"discovery_sha256"`
	SourceAddressCount  uint64            `json:"source_address_count"`
	Cursor              uint64            `json:"cursor"`
	LastBlockNumber     uint64            `json:"last_block_number"`
	LastBlockHash       string            `json:"last_block_hash"`
	LastProviderPrimary string            `json:"last_provider_primary"`
	LastProviderSecond  string            `json:"last_provider_secondary"`
	LastBatchAt         *time.Time        `json:"last_batch_at"`
	LastTailAt          *time.Time        `json:"last_tail_at"`
	TailNextBlock       uint64            `json:"tail_next_block"`
	DebtBearingCount    uint64            `json:"debt_bearing_count"`
	Counts              map[string]uint64 `json:"counts"`
	ExactQueueCount     uint64            `json:"exact_queue_count"`
	IncompleteCount     uint64            `json:"incomplete_count"`
	LastErrorClass      string            `json:"last_error_class,omitempty"`
	LastAttemptAt       *time.Time        `json:"last_attempt_at,omitempty"`
	StartupRetryCount   uint64            `json:"startup_retry_count,omitempty"`
}

type Screener struct {
	config        Config
	client        *http.Client
	mu            sync.Mutex
	state         State
	debtBearing   map[string]bool
	refreshKnown  map[string]bool
	refreshOrder  []string
	refreshCursor int
	hotBorrowers  map[string]string
	wait          func(context.Context, time.Duration) bool
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
	SchemaVersion string `json:"schema_version"`
	ChainID       uint64 `json:"chain_id"`
	RequestID     string `json:"request_id"`
	Borrower      string `json:"borrower"`
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
	DebtAsset                string `json:"debt_asset"`
	CollateralAsset          string `json:"collateral_asset"`
	RepayAmount              string `json:"repay_amount"`
	SeizedCollateral         string `json:"seized_collateral"`
	ProtocolFeeCollateral    string `json:"protocol_fee_collateral"`
	LiquidatorCollateral     string `json:"liquidator_collateral"`
	UniswapFee500OutputWETH  string `json:"uniswap_v3_fee_500_output_weth"`
	UniswapFee3000OutputWETH string `json:"uniswap_v3_fee_3000_output_weth"`
}

type exactProvider struct {
	ProviderID  string            `json:"provider_id"`
	Account     account           `json:"account"`
	Liquidation *exactLiquidation `json:"liquidation"`
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
	ZeroCostProfitUpperBoundWei string              `json:"zero_cost_profit_upper_bound_wei,omitempty"`
	ExpectedNetPnLWei           string              `json:"expected_net_pnl_wei,omitempty"`
	ConservativeNetPnLWei       string              `json:"conservative_net_pnl_wei,omitempty"`
	StateRoot                   string              `json:"state_root,omitempty"`
	SelectedRoute               string              `json:"selected_route,omitempty"`
	TerminalOutcome             string              `json:"terminal_outcome"`
	ExecutionCandidate          *executionCandidate `json:"-"`
	AtlasCandidate              *atlasCandidate     `json:"-"`
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
	SchemaVersion        string `json:"schema_version"`
	ChainID              uint64 `json:"chain_id"`
	RequestID            string `json:"request_id"`
	BlockNumber          uint64 `json:"block_number"`
	BlockHash            string `json:"block_hash"`
	StateRoot            string `json:"state_root"`
	PrimaryProviderID    string `json:"primary_provider_id"`
	SecondaryProviderID  string `json:"secondary_provider_id"`
	EvidenceMode         string `json:"evidence_mode"`
	RouteID              string `json:"route_id"`
	CalldataHex          string `json:"calldata_hex"`
	CalldataHash         string `json:"calldata_hash"`
	SimulationResultHash string `json:"simulation_result_hash"`
	RealizedProfit       string `json:"realized_profit"`
	ConservativeNetPnL   string `json:"conservative_net_pnl"`
	DeadlineUnixSeconds  uint64 `json:"deadline_unix_seconds"`
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
		config: config, client: &http.Client{Timeout: 35 * time.Second}, state: state,
		debtBearing: make(map[string]bool), refreshKnown: make(map[string]bool),
		hotBorrowers: make(map[string]string), wait: waitContext,
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
	for {
		batch := make([]string, 0, s.config.BatchSize)
		for !seedComplete && len(batch) < s.config.BatchSize {
			address, nextErr := addresses.Next()
			if errors.Is(nextErr, io.EOF) {
				seedComplete = true
				break
			}
			if nextErr != nil {
				return nextErr
			}
			batch = append(batch, address)
		}
		advanceSeed := len(batch) > 0
		if seedComplete && len(batch) == 0 {
			batch = s.nextRefreshBatch()
		}
		if len(batch) > 0 {
			attempt := func() error { return s.screen(ctx, batch, advanceSeed, nil) }
			var screenErr error
			if s.Snapshot().LastBatchAt == nil {
				screenErr = s.screenWithStartupRetry(ctx, attempt)
			} else {
				screenErr = attempt()
			}
			if screenErr != nil {
				s.recordError(gatewayErrorClass(screenErr, "rpc_gateway_screen_failure"))
				if !s.waitDuration(ctx, s.config.Pace) {
					return nil
				}
				continue
			}
		}
		tailBorrowers, err := s.pollTail(ctx)
		if err != nil {
			s.recordError(gatewayErrorClass(err, "rpc_gateway_tail_failure"))
			if !s.waitDuration(ctx, s.config.Pace) {
				return nil
			}
			continue
		}
		for offset := 0; offset < len(tailBorrowers); offset += MaximumBatch {
			end := offset + MaximumBatch
			if end > len(tailBorrowers) {
				end = len(tailBorrowers)
			}
			if err := s.screen(ctx, tailBorrowers[offset:end], false, nil); err != nil {
				s.recordError("rpc_gateway_tail_screen_failure")
				break
			}
		}
		if !s.waitDuration(ctx, s.config.Pace) {
			return nil
		}
	}
}

func (s *Screener) Snapshot() State {
	s.mu.Lock()
	defer s.mu.Unlock()
	copy := s.state
	copy.Counts = make(map[string]uint64, len(s.state.Counts))
	for key, value := range s.state.Counts {
		copy.Counts[key] = value
	}
	return copy
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
	for _, borrower := range result.Borrowers {
		if err := s.updateBorrowerActivityLocked(borrower, true); err != nil {
			return nil, err
		}
	}
	now := time.Now().UTC()
	s.state.TailNextBlock = result.NextBlock
	s.state.LastTailAt = &now
	s.state.LastErrorClass = ""
	if err := s.persistStateLocked(); err != nil {
		return nil, err
	}
	return result.Borrowers, nil
}

func (s *Screener) HandleAtlasAuction(ctx context.Context, auction *observer.LedgerRecord) error {
	if auction == nil || !auction.RelevantAaveAuction || auction.ChainID != 42161 {
		return nil
	}
	borrowers := s.nextHotBatch()
	if len(borrowers) == 0 {
		return nil
	}
	return s.screen(ctx, borrowers, false, auction)
}

func (s *Screener) nextHotBatch() []string {
	s.mu.Lock()
	defer s.mu.Unlock()
	type entry struct{ borrower, hf string }
	entries := make([]entry, 0, len(s.hotBorrowers))
	for borrower, hf := range s.hotBorrowers {
		entries = append(entries, entry{borrower, hf})
	}
	sort.Slice(entries, func(i, j int) bool {
		first, firstOK := newBigUint(entries[i].hf)
		second, secondOK := newBigUint(entries[j].hf)
		if firstOK && secondOK && first.Cmp(second) != 0 {
			return first.Cmp(second) < 0
		}
		return entries[i].borrower < entries[j].borrower
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
	if result.SchemaVersion != ResponseSchema || result.ChainID != 42161 || result.RequestID != requestID || result.Primary.ProviderID == result.Secondary.ProviderID || result.Primary.WETHPriceBase != result.Secondary.WETHPriceBase || len(result.Primary.Accounts) != len(borrowers) || len(result.Secondary.Accounts) != len(borrowers) {
		return errors.New("gateway Aave evidence is incomplete")
	}
	s.mu.Lock()
	defer s.mu.Unlock()
	for index, primary := range result.Primary.Accounts {
		if primary != result.Secondary.Accounts[index] || primary.Borrower != borrowers[index] {
			return errors.New("gateway Aave providers disagree")
		}
		bucket := classify(primary.TotalDebtBase, primary.HealthFactorWAD)
		if bucket == "liquidatable" || bucket == "urgent" || bucket == "watch" {
			s.hotBorrowers[primary.Borrower] = primary.HealthFactorWAD
		} else {
			delete(s.hotBorrowers, primary.Borrower)
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
		if record.TerminalOutcome == "exact_pending" {
			exactRecord, exactErr := s.resolveExact(ctx, record, auction)
			if exactErr != nil {
				record.TerminalOutcome = "incomplete"
				bucket = "incomplete"
				record.Bucket = bucket
			} else {
				record = exactRecord
			}
		}
		if err := appendJSON(filepath.Join(s.config.StateDir, "signals.ndjson"), record); err != nil {
			return err
		}
		if s.config.SignalSink != nil {
			if err := s.config.SignalSink.RecordAaveSignal(ctx, record); err != nil {
				return err
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
	s.state.LastErrorClass = ""
	return s.persistStateLocked()
}

func (s *Screener) resolveExact(ctx context.Context, record signal, auction *observer.LedgerRecord) (signal, error) {
	requestID := fmt.Sprintf("aave-exact-%d-%d", record.Cursor, time.Now().UnixMilli())
	body, _ := json.Marshal(exactRequest{SchemaVersion: "phoenix.rpc.aave-exact-request.v1", ChainID: 42161, RequestID: requestID, Borrower: record.Borrower})
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
		return record, fmt.Errorf("exact gateway status %d", response.StatusCode)
	}
	var result exactResponse
	if err := json.NewDecoder(io.LimitReader(response.Body, maximumResponse)).Decode(&result); err != nil {
		return record, err
	}
	if result.SchemaVersion != "phoenix.rpc.aave-exact-response.v1" || result.ChainID != 42161 || result.RequestID != requestID || result.BlockNumber == 0 || result.BlockHash == "" || result.StateRoot == "" || result.Primary.ProviderID == result.Secondary.ProviderID || result.Primary.Account != result.Secondary.Account || !equalLiquidation(result.Primary.Liquidation, result.Secondary.Liquidation) {
		return record, errors.New("exact Aave provider evidence is incomplete")
	}
	record.Block = result.BlockNumber
	record.BlockHash = result.BlockHash
	record.StateRoot = result.StateRoot
	liquidation := result.Primary.Liquidation
	if liquidation == nil {
		record.TerminalOutcome = "economic_rejection"
		return record, nil
	}
	repay, ok := newBigUint(liquidation.RepayAmount)
	if !ok || repay.Sign() <= 0 {
		return record, errors.New("exact repay is invalid")
	}
	fee500, ok500 := newBigUint(liquidation.UniswapFee500OutputWETH)
	fee3000, ok3000 := newBigUint(liquidation.UniswapFee3000OutputWETH)
	if !ok500 || !ok3000 {
		return record, errors.New("exact route quotation is invalid")
	}
	output := fee500
	record.SelectedRoute = "UNISWAP_V3_500"
	if fee3000.Cmp(fee500) > 0 {
		output = fee3000
		record.SelectedRoute = "UNISWAP_V3_3000"
	}
	flash := new(big.Int).Mul(repay, new(big.Int).SetUint64(s.config.FlashPremiumBPS))
	flash.Add(flash, big.NewInt(9_999)).Div(flash, big.NewInt(10_000))
	maxFee, _ := newBigUint(s.config.MaximumFeePerGasWei)
	gas := new(big.Int).Mul(maxFee, new(big.Int).SetUint64(s.config.MaximumGasLimit))
	expected := new(big.Int).Sub(new(big.Int).Set(output), repay)
	expected.Sub(expected, flash).Sub(expected, gas)
	conservativeOutput := new(big.Int).Mul(output, new(big.Int).SetUint64(10_000-s.config.EconomicReserveBPS))
	conservativeOutput.Div(conservativeOutput, big.NewInt(10_000))
	conservative := new(big.Int).Sub(conservativeOutput, repay)
	conservative.Sub(conservative, flash).Sub(conservative, gas)
	record.ExpectedNetPnLWei = expected.String()
	record.ConservativeNetPnLWei = conservative.String()
	floor, _ := newBigUint(s.config.RetainedProfitFloorWei)
	if expected.Cmp(floor) <= 0 || conservative.Cmp(floor) <= 0 {
		record.TerminalOutcome = "economic_rejection"
	} else {
		minimumCollateral, ok := newBigUint(liquidation.LiquidatorCollateral)
		if !ok || minimumCollateral.Sign() <= 0 {
			return record, errors.New("exact collateral is invalid")
		}
		minimumCollateral.Mul(minimumCollateral, new(big.Int).SetUint64(10_000-s.config.EconomicReserveBPS))
		minimumCollateral.Div(minimumCollateral, big.NewInt(10_000))
		minimumProfit := new(big.Int).Add(new(big.Int).Set(floor), gas)
		selectedPool := uniswapPool500Address
		selectedFee := uint32(500)
		if record.SelectedRoute == "UNISWAP_V3_3000" {
			selectedPool = uniswapPool3000Address
			selectedFee = 3_000
		}
		simulation, simulationErr := s.simulateExact(ctx, record, liquidation, simulationRequest{
			MinimumCollateralReceived: minimumCollateral.String(),
			MinimumUnwindOutput:       conservativeOutput.String(),
			MinimumProfit:             minimumProfit.String(),
			ExpectedProfit:            expected.String(),
			SelectedPool:              selectedPool,
			SelectedFee:               selectedFee,
		})
		if simulationErr != nil {
			record.TerminalOutcome = "fork_pending"
			return record, nil
		}
		candidate, candidateErr := s.buildExecutionCandidate(record, liquidation, simulation, minimumCollateral.String(), conservativeOutput.String(), minimumProfit.String(), selectedPool, selectedFee)
		if candidateErr != nil {
			return record, candidateErr
		}
		record.ExecutionCandidate = candidate
		if auction != nil {
			atlas, atlasErr := s.buildAtlasCandidate(ctx, record, liquidation, simulation, auction, minimumCollateral.String(), conservativeOutput.String(), minimumProfit.String(), selectedPool, selectedFee)
			if atlasErr != nil {
				return record, atlasErr
			}
			record.AtlasCandidate = atlas
		}
		record.Authority = true
		record.TerminalOutcome = "candidate"
	}
	return record, nil
}

func (s *Screener) buildAtlasCandidate(
	ctx context.Context,
	record signal,
	liquidation *exactLiquidation,
	direct *simulationResponse,
	auction *observer.LedgerRecord,
	minimumCollateral, minimumUnwind, minimumProfit, selectedPool string,
	selectedFee uint32,
) (*atlasCandidate, error) {
	if auction == nil || direct == nil || !auction.RelevantAaveAuction {
		return nil, nil
	}
	gross, grossOK := newBigUint(direct.RealizedProfit)
	minimum, minimumOK := newBigUint(minimumProfit)
	if !grossOK || !minimumOK || gross.Cmp(minimum) <= 0 {
		return nil, nil
	}
	maximumBid := new(big.Int).Sub(new(big.Int).Set(gross), minimum)
	selectedBid := new(big.Int).Div(new(big.Int).Set(maximumBid), big.NewInt(2))
	if selectedBid.Sign() == 0 {
		selectedBid.SetUint64(1)
	}
	atlasSimulation, err := s.simulateExact(ctx, record, liquidation, simulationRequest{
		MinimumCollateralReceived: minimumCollateral,
		MinimumUnwindOutput:       minimumUnwind,
		MinimumProfit:             minimumProfit,
		ExpectedProfit:            direct.RealizedProfit,
		SelectedPool:              selectedPool,
		SelectedFee:               selectedFee,
		AtlasMode:                 true,
		AtlasBid:                  selectedBid.String(),
	})
	if err != nil {
		return nil, nil
	}
	calldata, err := hex.DecodeString(strings.TrimPrefix(atlasSimulation.CalldataHex, "0x"))
	if err != nil || len(calldata) <= 4 {
		return nil, errors.New("Atlas solver calldata is invalid")
	}
	deadline, ok := newUint64(auction.AuctionDeadlineBlock)
	if !ok || deadline == 0 || auction.SolverGasLimit == 0 {
		return nil, errors.New("Atlas auction bounds are invalid")
	}
	expected, expectedOK := newBigUint(record.ExpectedNetPnLWei)
	if !expectedOK || expected.Cmp(selectedBid) <= 0 {
		return nil, nil
	}
	expected.Sub(expected, selectedBid)
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
		ExpectedNetPnL:       expected.String(),
		ConservativeNetPnL:   atlasSimulation.ConservativeNetPnL,
		EvidenceMode:         atlasSimulation.EvidenceMode,
		SimulationResultHash: atlasSimulation.SimulationResultHash,
		OperationHash:        hex.EncodeToString(operationHash[:]),
		Operation:            operation,
		ObservedAt:           auction.ObservedAt,
	}, nil
}

func (s *Screener) simulateExact(ctx context.Context, record signal, liquidation *exactLiquidation, partial simulationRequest) (*simulationResponse, error) {
	requestID := fmt.Sprintf("aave-sim-%d-%d", record.Cursor, time.Now().UnixMilli())
	deadline := time.Now().UTC().Add(60 * time.Second).Unix()
	partial.SchemaVersion = "phoenix.rpc.aave-simulate-request.v1"
	partial.ChainID = 42161
	partial.RequestID = requestID
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
	partial.RetainedProfitFloor = s.config.RetainedProfitFloorWei
	partial.SelectedFactory = uniswapFactoryAddress
	partial.ZeroForOne = false
	partial.GasLimit = s.config.MaximumGasLimit
	partial.MaxFeePerGas = s.config.MaximumFeePerGasWei
	partial.MaxPriorityFeePerGas = s.config.MaximumPriorityFeeWei
	partial.DeadlineUnixSeconds = uint64(deadline)
	if partial.AtlasBid == "" {
		partial.AtlasBid = "0"
	}
	body, err := json.Marshal(partial)
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodPost, s.config.GatewayURL+"/v1/aave/simulate", bytes.NewReader(body))
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
		return nil, fmt.Errorf("simulation gateway status %d", response.StatusCode)
	}
	var result simulationResponse
	if err := json.NewDecoder(io.LimitReader(response.Body, maximumResponse)).Decode(&result); err != nil {
		return nil, err
	}
	if result.SchemaVersion != "phoenix.rpc.aave-simulate-response.v1" || result.ChainID != 42161 || result.RequestID != requestID || result.BlockNumber != record.Block || result.BlockHash != record.BlockHash || result.StateRoot != record.StateRoot || result.PrimaryProviderID == "" || result.PrimaryProviderID == result.SecondaryProviderID || result.EvidenceMode != "DUAL_PROVIDER_FORK_VERIFIED" || len(result.RouteID) != 66 || len(result.CalldataHash) != 64 || len(result.SimulationResultHash) != 64 || result.DeadlineUnixSeconds != partial.DeadlineUnixSeconds {
		return nil, errors.New("simulation evidence is incomplete")
	}
	calldata, err := hex.DecodeString(strings.TrimPrefix(result.CalldataHex, "0x"))
	calldataDigest := sha256.Sum256(calldata)
	if err != nil || hex.EncodeToString(calldataDigest[:]) != result.CalldataHash {
		return nil, errors.New("simulation calldata identity mismatch")
	}
	floor, _ := newBigUint(s.config.RetainedProfitFloorWei)
	conservative, ok := newBigUint(result.ConservativeNetPnL)
	if !ok || conservative.Cmp(floor) <= 0 {
		return nil, errors.New("simulation PnL is below the retained-profit floor")
	}
	return &result, nil
}

func (s *Screener) buildExecutionCandidate(record signal, liquidation *exactLiquidation, simulation *simulationResponse, minimumCollateral, minimumUnwind, minimumProfit, selectedPool string, selectedFee uint32) (*executionCandidate, error) {
	if simulation == nil {
		return nil, errors.New("simulation result is missing")
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
	leg := executionLeg{Pool: selectedPool, Factory: uniswapFactoryAddress, TokenIn: nativeUSDCAddress, TokenOut: wethAddress, Fee: selectedFee, ZeroForOne: false, MinAmountOut: minimumUnwind}
	candidate := &executionCandidate{
		RequestID: deterministicUUID(requestDigest), OpportunityID: deterministicUUID(opportunityDigest),
		RouteID:      simulation.RouteID,
		RoutePayload: aaveRoutePayload{Borrower: record.Borrower, DebtAsset: liquidation.DebtAsset, CollateralAsset: liquidation.CollateralAsset, ReceiveAToken: false, MinimumCollateralReceived: minimumCollateral, MinimumUnwindOutput: minimumUnwind, MaximumAtlasBid: "0", EvidenceMode: simulation.EvidenceMode, StateRoot: record.StateRoot, ReleaseSHA: s.config.ReleaseSHA},
		SelectedSize: liquidation.RepayAmount, TokenPath: []string{nativeUSDCAddress, wethAddress}, OriginRouter: aavePoolAddress,
		ExecutorAddress: s.config.ExecutorAddress, ExecutorCodeHash: s.config.ExecutorCodeHash,
		CalldataHash: simulation.CalldataHash, SimulationResultHash: simulation.SimulationResultHash,
		PinnedBlockNumber: record.Block, PinnedBlockHash: record.BlockHash,
		FlashAsset: liquidation.DebtAsset, FlashAmount: liquidation.RepayAmount, MaximumInputAmount: liquidation.RepayAmount,
		MinimumProfit: minimumProfit, ExpectedProfit: simulation.RealizedProfit,
		Deadline: deadline, Legs: []executionLeg{leg}, GasLimit: s.config.MaximumGasLimit,
		MaxFeePerGas: s.config.MaximumFeePerGasWei, MaxPriorityFeePerGas: s.config.MaximumPriorityFeeWei,
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

func equalLiquidation(first, second *exactLiquidation) bool {
	if first == nil || second == nil {
		return first == nil && second == nil
	}
	return *first == *second
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
	if hf.Cmp(big.NewInt(950_000_000_000_000_000)) < 0 {
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
	s.state.IncompleteCount++
	s.state.LastErrorClass = class
	_ = s.persistStateLocked()
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

func (s *Screener) applyHotSignal(record signal) {
	if record.Bucket == "liquidatable" || record.Bucket == "urgent" || record.Bucket == "watch" {
		s.hotBorrowers[record.Borrower] = record.HF
	} else {
		delete(s.hotBorrowers, record.Borrower)
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
