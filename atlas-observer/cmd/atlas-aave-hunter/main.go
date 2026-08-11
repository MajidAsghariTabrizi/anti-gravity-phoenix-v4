package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"net"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/hunter"
	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
)

func main() {
	var ledgerDir, discovery, discoveryHash, gateway, healthAddr string
	var startingCursor uint64
	var batch int
	var pace time.Duration
	var retainedProfitFloor, maximumInputAmount, maximumAtlasBidWei string
	var maximumGasLimit, flashPremiumBPS, economicReserveBPS uint64
	var maximumFeePerGasWei string
	var executorAddress, executorCodeHash, callerAddress, releaseSHA, maximumPriorityFeeWei string
	flag.StringVar(&ledgerDir, "ledger-dir", "", "absolute durable Atlas ledger directory")
	flag.StringVar(&discovery, "aave-discovery", "", "absolute immutable borrower discovery seed")
	flag.StringVar(&discoveryHash, "aave-discovery-sha256", "", "expected immutable discovery seed SHA-256")
	flag.StringVar(&gateway, "rpc-gateway-url", "http://rpc-gateway:9300", "internal Phoenix RPC Gateway")
	flag.StringVar(&healthAddr, "health-addr", "0.0.0.0:9700", "health listen address")
	flag.Uint64Var(&startingCursor, "aave-starting-cursor", 1100, "minimum preserved screening cursor")
	flag.IntVar(&batch, "aave-batch-size", hunter.DefaultBatch, "bounded Aave screen batch")
	flag.DurationVar(&pace, "aave-pace", 60*time.Second, "minimum time between screen batches")
	flag.StringVar(&retainedProfitFloor, "retained-profit-floor-wei", "", "strict Production retained-profit floor in WETH wei")
	flag.Uint64Var(&maximumGasLimit, "maximum-gas-limit", 0, "maximum liquidation gas charged by the conservative gate")
	flag.StringVar(&maximumFeePerGasWei, "maximum-fee-per-gas-wei", "", "maximum gas price charged by the conservative gate")
	bindFinancialCeilingFlags(flag.CommandLine, &maximumInputAmount, &maximumAtlasBidWei)
	flag.Uint64Var(&flashPremiumBPS, "flash-premium-bps", 9, "conservative flash premium in basis points")
	flag.Uint64Var(&economicReserveBPS, "economic-reserve-bps", 500, "combined failure, latency, drift, and model reserve")
	flag.StringVar(&executorAddress, "executor-address", "", "exact deployed PhoenixExecutor address")
	flag.StringVar(&executorCodeHash, "executor-code-hash", "", "exact deployed PhoenixExecutor SHA-256 code hash")
	flag.StringVar(&callerAddress, "caller-address", "", "signer-derived executor caller address")
	flag.StringVar(&releaseSHA, "release-sha", "", "exact protected release SHA")
	flag.StringVar(&maximumPriorityFeeWei, "maximum-priority-fee-per-gas-wei", "", "maximum priority fee for materialized liquidation requests")
	flag.Parse()
	if ledgerDir == "" || discovery == "" || discoveryHash == "" || !filepath.IsAbs(ledgerDir) {
		fmt.Fprintln(os.Stderr, "durable Atlas and immutable Aave inputs are required")
		os.Exit(2)
	}

	logger := log.New(os.Stdout, "atlas-aave-hunter ", log.LstdFlags|log.LUTC)
	sink, err := hunter.OpenPostgresSignalSink(context.Background(), os.Getenv("POSTGRES_DSN"), retainedProfitFloor)
	if err != nil {
		logger.Fatalf("open durable hunting sink: %v", err)
	}
	defer sink.Close()
	ledger, err := observer.OpenLedger(filepath.Join(ledgerDir, "atlas"), time.Now().UTC(), 0, 0)
	if err != nil {
		logger.Fatalf("open Atlas ledger: %v", err)
	}
	screener, err := hunter.New(hunter.Config{
		DiscoveryPath: discovery, DiscoverySHA256: discoveryHash,
		StateDir: filepath.Join(ledgerDir, "aave"), GatewayURL: gateway,
		StartingCursor: startingCursor, BatchSize: batch, Pace: pace,
		RetainedProfitFloorWei: retainedProfitFloor,
		MaximumInputAmountWei:  maximumInputAmount,
		MaximumGasLimit:        maximumGasLimit,
		MaximumFeePerGasWei:    maximumFeePerGasWei,
		MaximumAtlasBidWei:     maximumAtlasBidWei,
		FlashPremiumBPS:        flashPremiumBPS,
		EconomicReserveBPS:     economicReserveBPS,
		ExecutorAddress:        executorAddress,
		ExecutorCodeHash:       executorCodeHash,
		CallerAddress:          callerAddress,
		ReleaseSHA:             releaseSHA,
		MaximumPriorityFeeWei:  maximumPriorityFeeWei,
		SignalSink:             sink,
	})
	if err != nil {
		logger.Fatalf("open Aave screener: %v", err)
	}

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	atlasErrors := make(chan error, 1)
	aaveErrors := make(chan error, 1)
	atlasCandidateErrors := make(chan error, 1)
	go func() { atlasErrors <- observer.NewClientWithSink(ledger, logger, sink).Run(ctx) }()
	go func() { aaveErrors <- screener.Run(ctx) }()
	go func() { atlasCandidateErrors <- runAtlasCandidateLoop(ctx, sink.AtlasAuctions(), screener, logger) }()

	listener, err := net.Listen("tcp", healthAddr)
	if err != nil {
		logger.Fatalf("listen: %v", err)
	}
	server := &http.Server{Handler: healthHandler(ledger, screener), ReadHeaderTimeout: 2 * time.Second, ReadTimeout: 5 * time.Second, WriteTimeout: 5 * time.Second, IdleTimeout: 30 * time.Second}
	serverErrors := make(chan error, 1)
	go func() { serverErrors <- server.Serve(listener) }()
	logger.Printf("continuous lanes started cursor=%d batch=%d signer_present=false execution_request_materialization=true", screener.Snapshot().Cursor, batch)

	var runErr error
	select {
	case runErr = <-atlasErrors:
	case runErr = <-aaveErrors:
	case runErr = <-atlasCandidateErrors:
	case runErr = <-serverErrors:
		if runErr == http.ErrServerClosed {
			runErr = nil
		}
	}
	cancel()
	shutdown, shutdownCancel := context.WithTimeout(context.Background(), 5*time.Second)
	_ = server.Shutdown(shutdown)
	shutdownCancel()
	if runErr != nil {
		logger.Fatalf("hunter failed: %v", runErr)
	}
}

func bindFinancialCeilingFlags(flags *flag.FlagSet, maximumInputAmount, maximumAtlasBidWei *string) {
	flags.StringVar(maximumInputAmount, "maximum-input-amount", "", "maximum WETH-denominated liquidation input amount")
	flags.StringVar(maximumAtlasBidWei, "maximum-atlas-bid-wei", "", "maximum configured Atlas bid in WETH wei")
}

type atlasCandidateScreener interface {
	HandleAtlasAuction(context.Context, *observer.LedgerRecord) error
	RecordRetryableGatewayError(error) (bool, error)
	Snapshot() hunter.State
}

func runAtlasCandidateLoop(ctx context.Context, auctions <-chan *observer.LedgerRecord, screener atlasCandidateScreener, logger *log.Logger) error {
	for {
		select {
		case <-ctx.Done():
			return nil
		case auction, open := <-auctions:
			if !open {
				return fmt.Errorf("Atlas auction stream closed")
			}
			if err := screener.HandleAtlasAuction(ctx, auction); err != nil {
				if ctx.Err() != nil {
					return nil
				}
				retryable, recordErr := screener.RecordRetryableGatewayError(err)
				if recordErr != nil {
					return recordErr
				}
				if retryable {
					logger.Printf("Atlas candidate deferred error_class=%s exact_execution_ready=false", screener.Snapshot().LastErrorClass)
					continue
				}
				return err
			}
		}
	}
}

func healthHandler(ledger *observer.Ledger, screener *hunter.Screener) http.Handler {
	atlasHandler := observer.NewHealthHandler(ledger)
	mux := http.NewServeMux()
	mux.HandleFunc("/metrics", func(writer http.ResponseWriter, request *http.Request) {
		atlasHandler.ServeHTTP(writer, request)
		_, _ = fmt.Fprint(writer, screener.MetricsText())
	})
	mux.HandleFunc("/healthz", func(writer http.ResponseWriter, request *http.Request) {
		now := time.Now().UTC()
		state := screener.Snapshot()
		health := evaluateLaneHealth(now, ledger.Snapshot(now), state)
		writeHealthResponse(writer, health, state, false, false)
	})
	mux.HandleFunc("/readyz", func(writer http.ResponseWriter, request *http.Request) {
		now := time.Now().UTC()
		atlas := ledger.Snapshot(now)
		aave := screener.Snapshot()
		health := evaluateLaneHealth(now, atlas, aave)
		writeHealthResponse(writer, health, aave, atlas.Connected, true)
	})
	return mux
}

func writeHealthResponse(
	writer http.ResponseWriter,
	health laneHealth,
	state hunter.State,
	atlasConnected bool,
	requireHunting bool,
) {
	ok := health.ServiceHealthy
	if requireHunting {
		ok = health.HuntingHealthy
	}
	status := http.StatusOK
	if !ok {
		status = http.StatusServiceUnavailable
	}
	writer.Header().Set("Content-Type", "application/json")
	writer.WriteHeader(status)
	payload := healthPayload(health, state, atlasConnected)
	payload["ok"] = ok
	_ = json.NewEncoder(writer).Encode(payload)
}

const laneFreshnessWindow = 10 * time.Minute

type laneHealth struct {
	ServiceHealthy      bool
	HuntingHealthy      bool
	ExactExecutionReady bool
	DegradedReason      string
	RecoveryState       string
}

func evaluateLaneHealth(now time.Time, atlas observer.LedgerState, aave hunter.State) laneHealth {
	batchFresh := timestampFresh(now, aave.LastBatchAt)
	tailFresh := timestampFresh(now, aave.LastTailAt)
	attemptFresh := timestampFresh(now, aave.LastAttemptAt)
	dualFresh := timestampFresh(now, aave.LastDualAgreementAt)
	circuitOpen := aave.ProviderCircuitOpenUntilUnixMillis > 0 && aave.ProviderCircuitOpenUntilUnixMillis > now.UnixMilli()
	serviceHealthy := aave.Cursor >= 1100
	atlasHealthy := atlas.Connected && atlas.LastSubscriptionAt != nil && atlas.InvalidCount == 0 && !atlas.Completed
	authorityDiverged := aave.LastErrorClass == "revenue_lane_authority_diverged"
	huntingHealthy := serviceHealthy && atlasHealthy && (batchFresh || tailFresh || attemptFresh) && !authorityDiverged
	exactReady := huntingHealthy && batchFresh && tailFresh && dualFresh && aave.LastErrorClass == "" && !circuitOpen &&
		aave.LastBlockNumber > 0 && len(aave.LastBlockHash) == 66 && aave.LastProviderPrimary != "" &&
		aave.LastProviderSecond != "" && aave.LastProviderPrimary != aave.LastProviderSecond
	reason := ""
	recovery := "ready"
	if aave.LastErrorClass != "" {
		reason = aave.LastErrorClass
		recovery = "recovering"
	} else if !exactReady {
		reason = "exact_state_stale_or_incomplete"
		recovery = "initializing"
	}
	return laneHealth{
		ServiceHealthy:      serviceHealthy,
		HuntingHealthy:      huntingHealthy,
		ExactExecutionReady: exactReady,
		DegradedReason:      reason,
		RecoveryState:       recovery,
	}
}

func timestampFresh(now time.Time, observed *time.Time) bool {
	if observed == nil {
		return false
	}
	age := now.Sub(*observed)
	return age >= 0 && age < laneFreshnessWindow
}

func healthPayload(health laneHealth, state hunter.State, atlasConnected bool) map[string]any {
	return map[string]any{
		"service_health":                          health.ServiceHealthy,
		"hunting_health":                          health.HuntingHealthy,
		"exact_execution_readiness":               health.ExactExecutionReady,
		"degraded_reason":                         health.DegradedReason,
		"provider_recovery_state":                 health.RecoveryState,
		"atlas_connected":                         atlasConnected,
		"aave_cursor":                             state.Cursor,
		"aave_tail_next_block":                    state.TailNextBlock,
		"aave_debt_bearing":                       state.DebtBearingCount,
		"aave_exact_queue":                        state.ExactQueueCount,
		"aave_exact_queue_ledger_entries_total":   state.ExactQueueCount,
		"aave_exact_eligible_now":                 state.ExactEligibleNowCount,
		"aave_exact_scheduler_blocked":            state.SchedulerBlockedCount,
		"aave_exact_cooldown_blocked":             state.CooldownBlockedCount,
		"aave_route_ineligible_current":           state.RouteIneligibleCount,
		"aave_exact_provider_blocked":             state.ProviderBlockedCount,
		"aave_exact_authority_blocked":            state.AuthorityBlockedCount,
		"aave_exact_evaluations_in_flight":        state.ExactEvaluationsInFlight,
		"aave_oldest_exact_eligible_age_ms":       state.OldestExactEligibleAgeMillis,
		"aave_active_fork_pending":                state.ActiveForkPendingCount,
		"primary_provider_id":                     state.LastProviderPrimary,
		"secondary_provider_id":                   state.LastProviderSecond,
		"last_dual_agreement_at":                  state.LastDualAgreementAt,
		"last_recovery_attempt_at":                state.LastAttemptAt,
		"provider_retryable_degradation_total":    state.Counts["provider_retryable_degradation_total"],
		"provider_current_class_failure_streak":   state.Counts["provider_current_class_failure_streak"],
		"provider_recovery_attempt_total":         state.Counts["provider_recovery_attempt_total"],
		"provider_recovery_success_total":         state.Counts["provider_recovery_success_total"],
		"provider_circuit_open_total":             state.ProviderCircuitOpenTotal,
		"provider_circuit_skipped_total":          state.ProviderCircuitSkippedTotal,
		"provider_circuit_open_until_unix_millis": state.ProviderCircuitOpenUntilUnixMillis,
		"provider_degraded_since_unix_millis":     state.Counts["provider_degraded_since_unix_millis"],
		"provider_last_degraded_unix_millis":      state.Counts["provider_last_degraded_at_unix_millis"],
		"provider_last_recovery_unix_millis":      state.Counts["provider_last_recovery_at_unix_millis"],
		"provider_last_degraded_duration_millis":  state.Counts["provider_last_degraded_duration_millis"],
		"signer_present":                          false,
	}
}
