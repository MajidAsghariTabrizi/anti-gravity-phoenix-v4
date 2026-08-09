package hunter

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"io"
	"math/big"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"testing"
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
)

func TestStartupWaitsForGatewayReadinessAndOffsetsFirstScreen(t *testing.T) {
	readyCalls := 0
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.URL.Path != "/readyz" {
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
		readyCalls++
		if readyCalls == 1 {
			writer.WriteHeader(http.StatusServiceUnavailable)
			return
		}
		writer.WriteHeader(http.StatusOK)
	}))
	defer server.Close()
	waits := make([]time.Duration, 0, 2)
	screener := &Screener{
		config: Config{GatewayURL: server.URL},
		client: server.Client(),
		wait: func(_ context.Context, delay time.Duration) bool {
			waits = append(waits, delay)
			return true
		},
	}
	if err := screener.waitForGatewayStartup(context.Background()); err != nil {
		t.Fatal(err)
	}
	if readyCalls != 2 || len(waits) != 2 || waits[0] != gatewayReadyPoll || waits[1] != initialScreenOffset {
		t.Fatalf("ready_calls=%d waits=%v", readyCalls, waits)
	}
}

func TestStartupRetriesOnlySanitizedTransientGatewayFailures(t *testing.T) {
	for name, first := range map[string]*gatewayResponseError{
		"cold budget":        {statusCode: http.StatusTooManyRequests, class: "upstream_call_budget_exhausted", retryable: true},
		"temporary provider": {statusCode: http.StatusServiceUnavailable, class: "provider_unavailable", retryable: true},
	} {
		t.Run(name, func(t *testing.T) {
			directory := t.TempDir()
			waits := make([]time.Duration, 0, 1)
			attempts := 0
			screener := &Screener{
				config: Config{StateDir: directory},
				state:  State{Schema: StateSchema, Counts: map[string]uint64{}},
				wait: func(_ context.Context, delay time.Duration) bool {
					waits = append(waits, delay)
					return true
				},
			}
			err := screener.screenWithStartupRetry(context.Background(), func() error {
				attempts++
				if attempts == 1 {
					return first
				}
				now := time.Now().UTC()
				screener.mu.Lock()
				screener.state.LastBatchAt = &now
				screener.mu.Unlock()
				return nil
			})
			if err != nil {
				t.Fatal(err)
			}
			state := screener.Snapshot()
			if attempts != 2 || len(waits) != 1 || waits[0] != 10*time.Second {
				t.Fatalf("attempts=%d waits=%v", attempts, waits)
			}
			if state.LastBatchAt == nil || state.LastAttemptAt == nil || state.StartupRetryCount != 1 || state.LastErrorClass != "" {
				t.Fatalf("unexpected state: %+v", state)
			}
			if state.ExactQueueCount != 0 {
				t.Fatal("startup retry granted Candidate authority")
			}
			var durable State
			data, readErr := os.ReadFile(filepath.Join(directory, "state.json"))
			if readErr != nil || json.Unmarshal(data, &durable) != nil || durable.StartupRetryCount != 1 || durable.LastAttemptAt == nil {
				t.Fatalf("durable retry evidence is incomplete: read=%v state=%+v", readErr, durable)
			}
		})
	}
}

func TestStartupProviderDisagreementFailsClosedWithoutRetry(t *testing.T) {
	directory := t.TempDir()
	attempts := 0
	waits := 0
	screener := &Screener{
		config: Config{StateDir: directory},
		state:  State{Schema: StateSchema, Counts: map[string]uint64{}},
		wait:   func(_ context.Context, _ time.Duration) bool { waits++; return true },
	}
	err := screener.screenWithStartupRetry(context.Background(), func() error {
		attempts++
		return &gatewayResponseError{statusCode: http.StatusConflict, class: "provider_disagreement", retryable: false}
	})
	if err == nil || attempts != 1 || waits != 0 {
		t.Fatalf("err=%v attempts=%d waits=%d", err, attempts, waits)
	}
	state := screener.Snapshot()
	if state.LastBatchAt != nil || state.LastErrorClass != "provider_disagreement" || state.ExactQueueCount != 0 {
		t.Fatalf("non-retryable disagreement was not fail-closed: %+v", state)
	}
}

func TestGatewayErrorContractIsBoundedAndSanitized(t *testing.T) {
	response := &http.Response{
		StatusCode: http.StatusTooManyRequests,
		Body:       io.NopCloser(strings.NewReader(`{"error_class":"upstream_call_budget_exhausted","retryable":true,"retry_after_seconds":10,"secret":"must-not-escape"}`)),
	}
	err := decodeGatewayError(response)
	var gatewayErr *gatewayResponseError
	if !errors.As(err, &gatewayErr) || gatewayErr.retryAfter != 10*time.Second || !gatewayErr.retryable {
		t.Fatalf("unexpected gateway error: %v", err)
	}
	if strings.Contains(err.Error(), "must-not-escape") || strings.Contains(err.Error(), "secret") {
		t.Fatalf("raw gateway body escaped: %v", err)
	}
}

func TestRetryableProviderFailureIsRecordedWithoutCandidateAuthority(t *testing.T) {
	tests := map[string]struct {
		errorClass string
		status     int
		retryable  bool
		wantClass  string
	}{
		"provider disagreement": {"provider_disagreement", http.StatusBadGateway, false, "provider_disagreement"},
		"provider unavailable":  {"provider_unavailable", http.StatusServiceUnavailable, true, "provider_unavailable"},
		"secondary timeout":     {"secondary_timeout", http.StatusGatewayTimeout, true, "provider_timeout"},
		"secondary rate limit":  {"secondary_rate_limited", http.StatusTooManyRequests, true, "provider_rate_limited"},
	}
	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			directory := t.TempDir()
			screener := &Screener{
				config: Config{StateDir: directory},
				state:  State{Schema: StateSchema, Counts: map[string]uint64{}},
			}
			providerErr := &gatewayResponseError{statusCode: test.status, class: test.errorClass, retryable: test.retryable}
			accepted, err := screener.RecordRetryableGatewayError(providerErr)
			if err != nil {
				t.Fatal(err)
			}
			acceptedAgain, err := screener.RecordRetryableGatewayError(providerErr)
			if err != nil {
				t.Fatal(err)
			}
			state := screener.Snapshot()
			if !accepted || !acceptedAgain || state.LastErrorClass != test.wantClass || state.LastAttemptAt == nil || state.ExactQueueCount != 0 {
				t.Fatalf("retryable provider recovery crossed an authority boundary: accepted=%t/%t state=%+v", accepted, acceptedAgain, state)
			}
			if state.Counts[providerDegradationTotalKey] != 1 || state.Counts[providerRecoveryAttemptTotalKey] != 2 || state.Counts[providerDegradedSinceMillisKey] == 0 {
				t.Fatalf("retryable recovery counters are invalid: %+v", state.Counts)
			}
		})
	}

	screener := &Screener{config: Config{StateDir: t.TempDir()}, state: State{Schema: StateSchema, Counts: map[string]uint64{}}}
	accepted, err := screener.RecordRetryableGatewayError(&gatewayResponseError{
		statusCode: http.StatusBadGateway,
		class:      "provider_integrity_failure",
		retryable:  false,
	})
	if err != nil || accepted {
		t.Fatalf("fatal integrity error was swallowed: accepted=%t err=%v", accepted, err)
	}
}

func TestRetryableProviderErrorOpensCircuitForFiveMinutes(t *testing.T) {
	now := time.Date(2026, 8, 6, 0, 0, 0, 0, time.UTC)
	screener := &Screener{
		config: Config{StateDir: t.TempDir()},
		state:  State{Schema: StateSchema, Counts: map[string]uint64{}},
		now:    func() time.Time { return now },
	}
	accepted, err := screener.RecordRetryableGatewayError(&gatewayResponseError{
		statusCode: http.StatusServiceUnavailable,
		class:      "provider_unavailable",
		retryable:  true,
	})
	if err != nil || !accepted {
		t.Fatalf("retryable class was not treated as circuit event: accepted=%t err=%v", accepted, err)
	}
	state := screener.Snapshot()
	if state.ProviderCircuitOpenTotal != 1 || state.ProviderCircuitOpenUntilUnixMillis != now.Add(providerCircuitCooldown).UnixMilli() || !screener.IsProviderCircuitOpen() {
		t.Fatalf("circuit did not open for expected duration: %+v", state)
	}
}

func TestNonRetryableProviderErrorDoesNotOpenCircuit(t *testing.T) {
	screener := &Screener{
		config: Config{StateDir: t.TempDir()},
		state:  State{Schema: StateSchema, Counts: map[string]uint64{}},
		now:    time.Now().UTC,
	}
	accepted, err := screener.RecordRetryableGatewayError(&gatewayResponseError{
		statusCode: http.StatusBadGateway,
		class:      "provider_integrity_failure",
		retryable:  false,
	})
	if err != nil || accepted {
		t.Fatalf("non-retryable provider error should not open the circuit: accepted=%t err=%v", accepted, err)
	}
	state := screener.Snapshot()
	if state.ProviderCircuitOpenTotal != 0 || state.ProviderCircuitOpenUntilUnixMillis != 0 {
		t.Fatalf("non-retryable failure unexpectedly mutated circuit counters: %+v", state)
	}
}

func TestProviderCircuitSkipsGatewayRequestsDuringCooldown(t *testing.T) {
	directory := t.TempDir()
	discovery := filepath.Join(directory, "discovery.json")
	if err := os.WriteFile(discovery, []byte(`{"borrowers":["0x1111111111111111111111111111111111111111"]}`), 0o600); err != nil {
		t.Fatal(err)
	}
	current := time.Date(2026, 8, 6, 0, 0, 0, 0, time.UTC)
	screenCalls := 0
	tailCalls := 0
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.URL.Path == "/v1/aave/screen" {
			screenCalls++
		}
		if request.URL.Path == "/v1/aave/tail" {
			tailCalls++
		}
		t.Fatalf("request was sent during cooldown: %s", request.URL.Path)
	}))
	defer server.Close()
	screener := &Screener{
		config: Config{DiscoveryPath: discovery, GatewayURL: server.URL, StateDir: directory, BatchSize: 1, Pace: time.Second},
		client: server.Client(),
		wait: func(_ context.Context, _ time.Duration) bool {
			return false
		},
		now: func() time.Time { return current },
		state: State{
			Schema:                             StateSchema,
			Cursor:                             0,
			Counts:                             map[string]uint64{},
			LastBatchAt:                        &time.Time{},
			ProviderCircuitOpenUntilUnixMillis: current.Add(providerCircuitCooldown).UnixMilli(),
		},
	}
	if err := screener.Run(context.Background()); err != nil {
		t.Fatalf("run should pause for provider circuit: %v", err)
	}
	if screenCalls != 0 || tailCalls != 0 || screener.Snapshot().ProviderCircuitSkippedTotal != 1 {
		t.Fatalf("requests or skips were not counted correctly: screen=%d tail=%d state=%+v", screenCalls, tailCalls, screener.Snapshot())
	}
}

func TestFailedNormalBatchIsRetriedOnceAfterCircuitCooldown(t *testing.T) {
	directory := t.TempDir()
	discovery := filepath.Join(directory, "discovery.json")
	if err := os.WriteFile(discovery, []byte(`{"borrowers":["0x1111111111111111111111111111111111111111"]}`), 0o600); err != nil {
		t.Fatal(err)
	}
	now := time.Date(2026, 8, 6, 0, 0, 0, 0, time.UTC)
	clock := now
	var waits []time.Duration
	screenBatches := [][]string{}
	lastBatchAt := now
	screener := &Screener{
		config: Config{
			DiscoveryPath: discovery, StateDir: directory, GatewayURL: "unused",
			BatchSize: 1, Pace: time.Second, StartingCursor: 0,
		},
		state: State{
			Schema:      StateSchema,
			Counts:      map[string]uint64{},
			Cursor:      0,
			LastBatchAt: &lastBatchAt,
		},
		hotBorrowers: make(map[string]string),
		debtBearing:  make(map[string]bool),
		refreshKnown: make(map[string]bool),
		refreshOrder: nil,
	}
	screener.config.GatewayURL = ""
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/screen":
			if len(screenBatches) >= 3 {
				t.Fatalf("screen was retried too many times")
			}
			var input screenRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			screenBatches = append(screenBatches, input.Borrowers)
			switch len(screenBatches) {
			case 1:
				writer.WriteHeader(http.StatusServiceUnavailable)
				_, _ = writer.Write([]byte(`{"error_class":"provider_unavailable","retryable":true}`))
			case 2:
				_ = json.NewEncoder(writer).Encode(screenResponse{
					SchemaVersion: ResponseSchema,
					ChainID:       42161,
					RequestID:     input.RequestID,
					BlockNumber:   491300000,
					BlockHash:     "0x" + strings.Repeat("a", 64),
					Primary: providerScreen{
						ProviderID:    "production-nownodes-arbitrum",
						WETHPriceBase: "300000000000",
						Accounts:      []account{{Borrower: input.Borrowers[0], HealthFactorWAD: "1100000000000000000", TotalDebtBase: "1000000000000"}},
					},
					Secondary: providerScreen{
						ProviderID:    "production-slot-0",
						WETHPriceBase: "300000000000",
						Accounts:      []account{{Borrower: input.Borrowers[0], HealthFactorWAD: "1100000000000000000", TotalDebtBase: "1000000000000"}},
					},
				})
			default:
				t.Fatalf("unexpected screen call: %d", len(screenBatches))
			}
		case "/v1/aave/tail":
			var input tailRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			_ = json.NewEncoder(writer).Encode(tailResponse{
				SchemaVersion:        "phoenix.rpc.aave-tail-response.v1",
				ChainID:              42161,
				RequestID:            input.RequestID,
				FinalizedBlockNumber: 491300000,
				FinalizedBlockHash:   "0x" + strings.Repeat("a", 64),
				PrimaryProviderID:    "production-nownodes-arbitrum",
				SecondaryProviderID:  "production-slot-0",
				FromBlock:            491300001,
				ToBlock:              491300000,
				NextBlock:            491300001,
				Borrowers:            []string{},
			})
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()
	screener.client = server.Client()
	screener.config.GatewayURL = server.URL
	screener.now = func() time.Time { return clock }
	screener.wait = func(_ context.Context, delay time.Duration) bool {
		waits = append(waits, delay)
		if delay == providerCircuitCooldown {
			clock = clock.Add(delay)
			return true
		}
		return false
	}
	if err := screener.Run(context.Background()); err != nil {
		t.Fatalf("run did not execute cooldown retry as expected: %v", err)
	}
	if len(screenBatches) != 2 {
		t.Fatalf("expected one retry after cooldown: %v", screenBatches)
	}
	if screenBatches[0][0] != screenBatches[1][0] {
		t.Fatalf("failed batch was not preserved: %v", screenBatches)
	}
	state := screener.Snapshot()
	if state.Cursor != 1 || state.ProviderCircuitOpenUntilUnixMillis != 0 {
		t.Fatalf("circuit did not recover correctly after retry: %+v", state)
	}
	cooldownWaits := 0
	for _, delay := range waits {
		if delay == providerCircuitCooldown {
			cooldownWaits++
		}
	}
	if cooldownWaits != 1 {
		t.Fatalf("cooldown wait was not observed exactly once: %v", waits)
	}
	if state.ProviderCircuitOpenTotal != 1 || state.ProviderCircuitSkippedTotal != 1 {
		t.Fatalf("circuit counters are inconsistent: %+v", state)
	}
}

func TestSecondRetryableFailureReopensCircuitAfterCooldown(t *testing.T) {
	directory := t.TempDir()
	discovery := filepath.Join(directory, "discovery.json")
	if err := os.WriteFile(discovery, []byte(`{"borrowers":["0x1111111111111111111111111111111111111111"]}`), 0o600); err != nil {
		t.Fatal(err)
	}
	clock := time.Date(2026, 8, 6, 0, 0, 0, 0, time.UTC)
	screenAttempts := 0
	var waits []time.Duration
	screener := &Screener{
		config: Config{
			DiscoveryPath: discovery, StateDir: directory, GatewayURL: "unused",
			BatchSize: 1, Pace: time.Second, StartingCursor: 0,
		},
		state: State{
			Schema:      StateSchema,
			Counts:      map[string]uint64{},
			Cursor:      0,
			LastBatchAt: &clock,
		},
		hotBorrowers: make(map[string]string),
		debtBearing:  make(map[string]bool),
		refreshKnown: make(map[string]bool),
	}
	screener.config.GatewayURL = ""
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.URL.Path != "/v1/aave/screen" {
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
		screenAttempts++
		writer.WriteHeader(http.StatusServiceUnavailable)
		_, _ = writer.Write([]byte(`{"error_class":"provider_unavailable","retryable":true}`))
	}))
	defer server.Close()
	screener.client = server.Client()
	screener.config.GatewayURL = server.URL
	screener.now = func() time.Time { return clock }
	waitCount := 0
	screener.wait = func(_ context.Context, delay time.Duration) bool {
		waits = append(waits, delay)
		if delay != providerCircuitCooldown {
			return false
		}
		waitCount++
		if waitCount == 1 {
			clock = clock.Add(delay)
			return true
		}
		return false
	}
	if err := screener.Run(context.Background()); err != nil {
		t.Fatalf("run did not stop after second cooldown start: %v", err)
	}
	if screenAttempts != 2 {
		t.Fatalf("expected retryable failures to attempt exactly twice, got %d", screenAttempts)
	}
	state := screener.Snapshot()
	if state.ProviderCircuitOpenTotal != 2 {
		t.Fatalf("expected circuit reopen after second failure: %+v", state)
	}
	if len(waits) != 2 || waits[0] != providerCircuitCooldown || waits[1] != providerCircuitCooldown {
		t.Fatalf("expected two cooldown waits: %v", waits)
	}
	if state.Cursor != 0 {
		t.Fatalf("failed batch advanced during retry loop: %+v", state)
	}
}

func TestAtlasHotScreensAreSkippedWhileCircuitIsOpen(t *testing.T) {
	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		requests++
	}))
	defer server.Close()
	now := time.Date(2026, 8, 6, 0, 0, 0, 0, time.UTC)
	screener := &Screener{
		config: Config{GatewayURL: server.URL, StateDir: t.TempDir()},
		client: server.Client(),
		state: State{
			Schema:                             StateSchema,
			Counts:                             map[string]uint64{},
			ProviderCircuitOpenUntilUnixMillis: now.Add(providerCircuitCooldown).UnixMilli(),
		},
		hotBorrowers: map[string]string{"0x1111111111111111111111111111111111111111": "900000000000000000"},
		now:          func() time.Time { return now },
	}
	if err := screener.HandleAtlasAuction(context.Background(), &observer.LedgerRecord{ChainID: 42161, RelevantAaveAuction: true}); err != nil {
		t.Fatal(err)
	}
	if requests != 0 {
		t.Fatalf("atlas hot screen bypassed circuit guardrail: requests=%d", requests)
	}
	if screener.Snapshot().ProviderCircuitSkippedTotal != 1 {
		t.Fatalf("expected one skip, got %+v", screener.Snapshot())
	}
}

type recordingSignalSink struct {
	records []signal
}

func (s *recordingSignalSink) RecordAaveSignal(_ context.Context, record signal) error {
	s.records = append(s.records, record)
	return nil
}

func TestFreshAgreementRecoversAfterOneAuthorityFreeScreen(t *testing.T) {
	directory := t.TempDir()
	borrowerAccount := account{
		Borrower:                    "0x1111111111111111111111111111111111111111",
		TotalCollateralBase:         "2000000000000",
		TotalDebtBase:               "1000000000000",
		AvailableBorrowsBase:        "0",
		CurrentLiquidationThreshold: "8000",
		LoanToValueBPS:              "7500",
		HealthFactorWAD:             "900000000000000000",
	}
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.URL.Path != "/v1/aave/screen" {
			t.Fatalf("degraded recovery emitted an exact request: %s", request.URL.Path)
		}
		var input screenRequest
		if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
			t.Fatal(err)
		}
		_ = json.NewEncoder(writer).Encode(screenResponse{
			SchemaVersion: ResponseSchema,
			ChainID:       42161,
			RequestID:     input.RequestID,
			BlockNumber:   491300000,
			BlockHash:     "0x" + strings.Repeat("a", 64),
			Primary: providerScreen{
				ProviderID: "production-nownodes-arbitrum", WETHPriceBase: "300000000000", Accounts: []account{borrowerAccount},
			},
			Secondary: providerScreen{
				ProviderID: "production-slot-0", WETHPriceBase: "300000000000", Accounts: []account{borrowerAccount},
			},
		})
	}))
	defer server.Close()
	sink := &recordingSignalSink{}
	screener := &Screener{
		config:      Config{StateDir: directory, GatewayURL: server.URL, RetainedProfitFloorWei: "1", SignalSink: sink},
		client:      server.Client(),
		state:       State{Schema: StateSchema, Counts: map[string]uint64{}},
		debtBearing: make(map[string]bool), refreshKnown: make(map[string]bool), hotBorrowers: make(map[string]string),
	}
	accepted, err := screener.RecordRetryableGatewayError(&gatewayResponseError{
		statusCode: http.StatusServiceUnavailable, class: "provider_unavailable", retryable: true,
	})
	if err != nil || !accepted {
		t.Fatalf("degradation was not recorded: accepted=%t err=%v", accepted, err)
	}
	if err := screener.screen(context.Background(), []string{borrowerAccount.Borrower}, false, nil); err != nil {
		t.Fatal(err)
	}
	if len(sink.records) != 1 || sink.records[0].Authority || sink.records[0].ExecutionCandidate != nil || sink.records[0].TerminalOutcome != "exact_pending" {
		t.Fatalf("degraded screen emitted authority: %+v", sink.records)
	}
	state := screener.Snapshot()
	if state.LastErrorClass != "" || state.LastDualAgreementAt == nil || state.ExactQueueCount != 0 {
		t.Fatalf("fresh dual agreement did not recover cleanly: %+v", state)
	}
	if state.Counts[providerRecoverySuccessTotalKey] != 1 || state.Counts[providerLastRecoveryAtMillisKey] == 0 || state.Counts[providerLastDegradedDurationKey] == 0 || state.Counts[providerDegradedSinceMillisKey] != 0 {
		t.Fatalf("recovery evidence is incomplete: %+v", state.Counts)
	}
}

func TestExactProviderFailureBubblesToRecoveryBoundary(t *testing.T) {
	directory := t.TempDir()
	borrowerAccount := account{
		Borrower:                    "0x1111111111111111111111111111111111111111",
		TotalCollateralBase:         "2000000000000",
		TotalDebtBase:               "1000000000000",
		AvailableBorrowsBase:        "0",
		CurrentLiquidationThreshold: "8000",
		LoanToValueBPS:              "7500",
		HealthFactorWAD:             "900000000000000000",
	}
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/screen":
			var input screenRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			_ = json.NewEncoder(writer).Encode(screenResponse{
				SchemaVersion: ResponseSchema, ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 491300000, BlockHash: "0x" + strings.Repeat("a", 64),
				Primary:   providerScreen{ProviderID: "production-nownodes-arbitrum", WETHPriceBase: "300000000000", Accounts: []account{borrowerAccount}},
				Secondary: providerScreen{ProviderID: "production-slot-0", WETHPriceBase: "300000000000", Accounts: []account{borrowerAccount}},
			})
		case "/v1/aave/exact":
			writer.WriteHeader(http.StatusBadGateway)
			_, _ = writer.Write([]byte(`{"error_class":"provider_disagreement","retryable":false}`))
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()
	sink := &recordingSignalSink{}
	screener := &Screener{
		config: Config{StateDir: directory, GatewayURL: server.URL, RetainedProfitFloorWei: "1", SignalSink: sink},
		client: server.Client(), state: State{Schema: StateSchema, Counts: map[string]uint64{}},
		debtBearing: make(map[string]bool), refreshKnown: make(map[string]bool), hotBorrowers: make(map[string]string),
	}
	err := screener.screen(context.Background(), []string{borrowerAccount.Borrower}, false, nil)
	accepted, recordErr := screener.RecordRetryableGatewayError(err)
	if recordErr != nil || !accepted {
		t.Fatalf("exact provider error did not enter recovery: accepted=%t err=%v record_err=%v", accepted, err, recordErr)
	}
	state := screener.Snapshot()
	if state.LastErrorClass != "provider_disagreement" || state.LastAttemptAt == nil || state.ExactQueueCount != 0 || len(sink.records) != 0 {
		t.Fatalf("exact failure crossed authority or queue boundaries: state=%+v records=%+v", state, sink.records)
	}
}

func TestDegradedAtlasAuctionDoesNotStartCompetingRecovery(t *testing.T) {
	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		requests++
	}))
	defer server.Close()
	screener := &Screener{
		config:       Config{GatewayURL: server.URL},
		client:       server.Client(),
		state:        State{Schema: StateSchema, Counts: map[string]uint64{}, LastErrorClass: "provider_unavailable"},
		hotBorrowers: map[string]string{"0x1111111111111111111111111111111111111111": "900000000000000000"},
	}
	if err := screener.HandleAtlasAuction(context.Background(), &observer.LedgerRecord{ChainID: 42161, RelevantAaveAuction: true}); err != nil {
		t.Fatal(err)
	}
	if requests != 0 || screener.Snapshot().ExactQueueCount != 0 {
		t.Fatalf("Atlas started a competing recovery path: requests=%d state=%+v", requests, screener.Snapshot())
	}
}

func TestSuccessfulEmptyTailPersistsProgressWithoutClearingDegradation(t *testing.T) {
	directory := t.TempDir()
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		var input tailRequest
		if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
			t.Fatal(err)
		}
		_ = json.NewEncoder(writer).Encode(tailResponse{
			SchemaVersion: "phoenix.rpc.aave-tail-response.v1",
			ChainID:       42161, RequestID: input.RequestID,
			FinalizedBlockNumber: 491218648,
			FinalizedBlockHash:   "0x" + strings.Repeat("a", 64),
			PrimaryProviderID:    "production-nownodes-arbitrum",
			SecondaryProviderID:  "production-slot-0",
			FromBlock:            491218649, ToBlock: 491218648, NextBlock: 491218649,
			Borrowers: []string{},
		})
	}))
	defer server.Close()
	screener := &Screener{
		config:      Config{GatewayURL: server.URL, StateDir: directory},
		client:      server.Client(),
		state:       State{Schema: StateSchema, Counts: map[string]uint64{}, LastErrorClass: "provider_unavailable"},
		debtBearing: make(map[string]bool), refreshKnown: make(map[string]bool),
	}
	borrowers, err := screener.pollTail(context.Background())
	if err != nil || len(borrowers) != 0 {
		t.Fatalf("borrowers=%v err=%v", borrowers, err)
	}
	state := screener.Snapshot()
	if state.LastTailAt == nil || state.TailNextBlock != 491218649 || state.LastErrorClass != "provider_unavailable" {
		t.Fatalf("empty tail did not preserve fail-closed recovery state: %+v", state)
	}
}

func TestNewRequiresCanonicalGitReleaseSHA(t *testing.T) {
	directory := t.TempDir()
	discovery := filepath.Join(directory, "discovery.json")
	content := []byte(`{"borrowers":[]}`)
	if err := os.WriteFile(discovery, content, 0o600); err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256(content)
	config := Config{
		DiscoveryPath:          discovery,
		DiscoverySHA256:        hex.EncodeToString(digest[:]),
		StateDir:               filepath.Join(directory, "state"),
		GatewayURL:             "http://rpc-gateway:9300",
		StartingCursor:         1100,
		BatchSize:              1,
		Pace:                   time.Second,
		RetainedProfitFloorWei: "1",
		MaximumInputAmountWei:  "1",
		MaximumGasLimit:        1,
		MaximumFeePerGasWei:    "1",
		MaximumAtlasBidWei:     "1",
		FlashPremiumBPS:        9,
		EconomicReserveBPS:     500,
		ExecutorAddress:        "0x1111111111111111111111111111111111111111",
		ExecutorCodeHash:       strings.Repeat("a", 64),
		CallerAddress:          "0x2222222222222222222222222222222222222222",
		ReleaseSHA:             strings.Repeat("b", 40),
		MaximumPriorityFeeWei:  "1",
	}
	if _, err := New(config); err != nil {
		t.Fatalf("canonical Git release SHA was rejected: %v", err)
	}
	for name, releaseSHA := range map[string]string{
		"legacy 64-character digest": strings.Repeat("b", 64),
		"uppercase Git SHA":          strings.Repeat("B", 40),
	} {
		t.Run(name, func(t *testing.T) {
			invalid := config
			invalid.ReleaseSHA = releaseSHA
			if _, err := New(invalid); err == nil || err.Error() != "hunter execution identity is invalid" {
				t.Fatalf("unexpected error: %v", err)
			}
		})
	}
}

func TestLiquidatablePriorityPrefersLargerDebtBeforeLowerHealthFactor(t *testing.T) {
	small := "0x1111111111111111111111111111111111111111"
	large := "0x2222222222222222222222222222222222222222"
	urgent := "0x3333333333333333333333333333333333333333"

	screener := &Screener{
		hotBorrowers: map[string]string{
			small:  "800000000000000000",
			large:  "990000000000000000",
			urgent: "1001000000000000000",
		},
		hotDebtBase: map[string]string{
			small:  "100",
			large:  "100000",
			urgent: "100000000",
		},
	}
	batch := screener.nextHotBatch()
	if len(batch) != 3 || batch[0] != large || batch[1] != small || batch[2] != urgent {
		t.Fatalf("unexpected hot borrower priority: %v", batch)
	}

	accounts := []account{
		{Borrower: small, TotalDebtBase: "100", HealthFactorWAD: "800000000000000000"},
		{Borrower: urgent, TotalDebtBase: "100000000", HealthFactorWAD: "1001000000000000000"},
		{Borrower: large, TotalDebtBase: "100000", HealthFactorWAD: "990000000000000000"},
	}
	order := prioritizedAccountOrder(accounts)
	if len(order) != 3 || order[0] != 2 || order[1] != 0 || order[2] != 1 {
		t.Fatalf("unexpected screen account priority: %v", order)
	}
}

func TestRecentExactEvidenceDefersDuplicateBorrowerWithoutExactRPC(t *testing.T) {
	directory := t.TempDir()
	now := time.Date(2026, 8, 8, 8, 0, 0, 0, time.UTC)
	borrower := "0x1111111111111111111111111111111111111111"
	exactRequests := 0

	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/screen":
			var input screenRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			borrowerAccount := account{
				Borrower:        borrower,
				TotalDebtBase:   "1000000000000",
				HealthFactorWAD: "900000000000000000",
			}
			_ = json.NewEncoder(writer).Encode(screenResponse{
				SchemaVersion: ResponseSchema,
				ChainID:       42161,
				RequestID:     input.RequestID,
				BlockNumber:   491300000,
				BlockHash:     "0x" + strings.Repeat("a", 64),
				Primary: providerScreen{
					ProviderID:    "production-nownodes-arbitrum",
					WETHPriceBase: "300000000000",
					Accounts:      []account{borrowerAccount},
				},
				Secondary: providerScreen{
					ProviderID:    "production-slot-0",
					WETHPriceBase: "300000000000",
					Accounts:      []account{borrowerAccount},
				},
			})
		case "/v1/aave/exact":
			exactRequests++
			t.Fatal("duplicate borrower crossed the Exact cooldown")
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()

	sink := &recordingSignalSink{}
	screener := &Screener{
		config: Config{
			StateDir:               directory,
			GatewayURL:             server.URL,
			RetainedProfitFloorWei: "1",
			SignalSink:             sink,
		},
		client:       server.Client(),
		state:        State{Schema: StateSchema, Counts: map[string]uint64{}},
		debtBearing:  make(map[string]bool),
		refreshKnown: make(map[string]bool),
		hotBorrowers: make(map[string]string),
		hotDebtBase:  make(map[string]string),
		lastExactAt: map[string]time.Time{
			borrower: now.Add(-30 * time.Second),
		},
		now: func() time.Time { return now },
	}

	if err := screener.screen(context.Background(), []string{borrower}, false, nil); err != nil {
		t.Fatal(err)
	}
	if exactRequests != 0 {
		t.Fatalf("unexpected Exact requests: %d", exactRequests)
	}
	if len(sink.records) != 1 ||
		sink.records[0].TerminalOutcome != "exact_pending" ||
		sink.records[0].ExactDeferredReason != "borrower_cooldown" {
		t.Fatalf("duplicate Exact deferral evidence is incomplete: %+v", sink.records)
	}
	if screener.Snapshot().Counts[exactDeferredCooldownKey] != 1 {
		t.Fatalf("duplicate Exact deferral was not counted: %+v", screener.Snapshot().Counts)
	}
}

func TestClassificationIsIntegerAndFailClosed(t *testing.T) {
	tests := map[string]struct{ debt, hf, want string }{
		"no debt":      {"0", "0", "no_debt"},
		"liquidatable": {"1", "999999999999999999", "liquidatable"},
		"urgent":       {"1", "1000000000000000000", "urgent"},
		"watch":        {"1", "1020000000000000000", "watch"},
		"safe":         {"1", "1100000000000000000", "debt_safe"},
		"malformed":    {"1", "not-an-integer", "incomplete"},
	}
	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			if observed := classify(test.debt, test.hf); observed != test.want {
				t.Fatalf("classification=%s want=%s", observed, test.want)
			}
		})
	}
}

func TestDiscoveryStreamPreservesOrderAndRejectsSubstitution(t *testing.T) {
	directory := t.TempDir()
	path := filepath.Join(directory, "discovery.json")
	content := []byte(`{"schema":"fixture","borrowers":["0x1111111111111111111111111111111111111111","0x2222222222222222222222222222222222222222"],"content_sha256":"fixture"}`)
	if err := os.WriteFile(path, content, 0o600); err != nil {
		t.Fatal(err)
	}
	stream, err := streamBorrowers(path)
	if err != nil {
		t.Fatal(err)
	}
	defer stream.Close()
	first, err := stream.Next()
	if err != nil || first != "0x1111111111111111111111111111111111111111" {
		t.Fatalf("first=%s err=%v", first, err)
	}
	second, err := stream.Next()
	if err != nil || second != "0x2222222222222222222222222222222222222222" {
		t.Fatalf("second=%s err=%v", second, err)
	}
	if _, err := stream.Next(); err != io.EOF {
		t.Fatalf("expected EOF, got %v", err)
	}
}

func TestBorrowerIndexIsDurableAndFailClosed(t *testing.T) {
	directory := t.TempDir()
	first := &Screener{
		config:       Config{StateDir: directory},
		state:        State{},
		debtBearing:  make(map[string]bool),
		refreshKnown: make(map[string]bool),
	}
	borrower := "0x1111111111111111111111111111111111111111"
	if err := first.updateBorrowerActivityLocked(borrower, true); err != nil {
		t.Fatal(err)
	}
	if !first.debtBearing[borrower] || first.state.DebtBearingCount != 1 {
		t.Fatal("active borrower was not indexed")
	}
	second := &Screener{
		config:       Config{StateDir: directory},
		state:        State{},
		debtBearing:  make(map[string]bool),
		refreshKnown: make(map[string]bool),
	}
	if err := second.loadBorrowerIndex(); err != nil {
		t.Fatal(err)
	}
	if !second.debtBearing[borrower] || len(second.refreshOrder) != 1 {
		t.Fatal("durable borrower index did not replay")
	}
	if err := second.updateBorrowerActivityLocked(borrower, false); err != nil {
		t.Fatal(err)
	}
	third := &Screener{
		config:       Config{StateDir: directory},
		state:        State{},
		debtBearing:  make(map[string]bool),
		refreshKnown: make(map[string]bool),
	}
	if err := third.loadBorrowerIndex(); err != nil {
		t.Fatal(err)
	}
	if third.debtBearing[borrower] || third.state.DebtBearingCount != 0 {
		t.Fatal("inactive borrower remained debt-bearing")
	}
}

func TestGatewayBudgetExhaustionUsesShortCircuitCooldown(t *testing.T) {
	now := time.Date(2026, 8, 7, 0, 0, 0, 0, time.UTC)
	screener := &Screener{
		config: Config{StateDir: t.TempDir()},
		state:  State{Schema: StateSchema, Counts: map[string]uint64{}},
		now:    func() time.Time { return now },
	}
	accepted, err := screener.RecordRetryableGatewayError(&gatewayResponseError{
		statusCode: http.StatusTooManyRequests,
		class:      "upstream_call_budget_exhausted",
		retryable:  true,
	})
	if err != nil || !accepted {
		t.Fatalf("local gateway budget exhaustion was not accepted: accepted=%t err=%v", accepted, err)
	}
	state := screener.Snapshot()
	if gatewayBudgetCircuitCooldown >= providerCircuitCooldown {
		t.Fatalf("local budget cooldown must remain shorter than provider cooldown")
	}
	if state.ProviderCircuitOpenTotal != 1 ||
		state.ProviderCircuitOpenUntilUnixMillis != now.Add(gatewayBudgetCircuitCooldown).UnixMilli() ||
		state.LastErrorClass != "provider_rate_limited" {
		t.Fatalf("local gateway budget did not use bounded short cooldown: %+v", state)
	}
}

func TestSecondaryRateLimitStillUsesFiveMinuteProviderCooldown(t *testing.T) {
	now := time.Date(2026, 8, 7, 0, 0, 0, 0, time.UTC)
	screener := &Screener{
		config: Config{StateDir: t.TempDir()},
		state:  State{Schema: StateSchema, Counts: map[string]uint64{}},
		now:    func() time.Time { return now },
	}
	accepted, err := screener.RecordRetryableGatewayError(&gatewayResponseError{
		statusCode: http.StatusTooManyRequests,
		class:      "secondary_rate_limited",
		retryable:  true,
	})
	if err != nil || !accepted {
		t.Fatalf("secondary rate limit was not accepted: accepted=%t err=%v", accepted, err)
	}
	state := screener.Snapshot()
	if state.ProviderCircuitOpenUntilUnixMillis != now.Add(providerCircuitCooldown).UnixMilli() {
		t.Fatalf("external provider rate limit lost five-minute protection: %+v", state)
	}
}

func TestProfitEdgeReserveUsesPositiveEdgeInsteadOfGrossNotional(t *testing.T) {
	output := big.NewInt(1_050_000)
	repay := big.NewInt(1_000_000)
	flash := big.NewInt(1_000)
	gas := big.NewInt(9_000)
	expected := new(big.Int).Sub(new(big.Int).Set(output), repay)
	expected.Sub(expected, flash).Sub(expected, gas)

	reserve, conservative, minimumUnwind := profitEdgeReserve(expected, output, 500)
	if expected.String() != "40000" ||
		reserve.String() != "2000" ||
		conservative.String() != "38000" ||
		minimumUnwind.String() != "1048000" {
		t.Fatalf(
			"unexpected edge reserve economics: expected=%s reserve=%s conservative=%s minimum_unwind=%s",
			expected, reserve, conservative, minimumUnwind,
		)
	}

	legacyOutput := new(big.Int).Mul(new(big.Int).Set(output), big.NewInt(9_500))
	legacyOutput.Div(legacyOutput, big.NewInt(10_000))
	legacy := new(big.Int).Sub(legacyOutput, repay)
	legacy.Sub(legacy, flash).Sub(legacy, gas)
	if legacy.Sign() >= 0 {
		t.Fatalf("legacy notional haircut unexpectedly retained the positive edge: %s", legacy)
	}
}

func TestProfitEdgeReserveDoesNotAmplifyNegativeExpectedPnL(t *testing.T) {
	expected := big.NewInt(-7_000)
	output := big.NewInt(1_000_000)
	reserve, conservative, minimumUnwind := profitEdgeReserve(expected, output, 500)
	if reserve.Sign() != 0 || conservative.Cmp(expected) != 0 || minimumUnwind.Cmp(output) != 0 {
		t.Fatalf(
			"negative edge was mutated: reserve=%s conservative=%s minimum_unwind=%s",
			reserve, conservative, minimumUnwind,
		)
	}
}

func TestLiquidationVariantBoundsArePerCollateralAndMaximumInputBound(t *testing.T) {
	screener := &Screener{config: Config{MaximumInputAmountWei: "100", FlashPremiumBPS: 5}}
	variants := make([]exactLiquidation, 0, 8)
	for _, collateral := range []string{wethAddress, nativeUSDCAddress} {
		for _, amount := range []int64{25, 50, 75, 100} {
			variants = append(variants, boundedTestLiquidation(collateral, amount, 5))
		}
	}
	if err := screener.validateLiquidationVariants(variants, 5); err != nil {
		t.Fatalf("bounded per-collateral grids were rejected: %v", err)
	}

	oversized := append([]exactLiquidation(nil), variants...)
	oversized[3] = boundedTestLiquidation(wethAddress, 101, 5)
	if err := screener.validateLiquidationVariants(oversized, 5); err == nil {
		t.Fatal("oversized exact repay crossed the configured maximum input")
	}

	mismatchedActual := append([]exactLiquidation(nil), variants...)
	mismatchedActual[0].ActualRepayAmount = "24"
	if err := screener.validateLiquidationVariants(mismatchedActual, 5); err == nil {
		t.Fatal("requested and actual repay mismatch was accepted")
	}

	fifthSize := append([]exactLiquidation(nil), variants...)
	fifthSize = append(fifthSize, boundedTestLiquidation(wethAddress, 101, 5))
	screener.config.MaximumInputAmountWei = "1000"
	if err := screener.validateLiquidationVariants(fifthSize, 5); err == nil {
		t.Fatal("a fifth size crossed the per-collateral grid bound")
	}
}

func TestAaveFlashPremiumUsesHalfUpPercentageMath(t *testing.T) {
	if got := aavePercentMul(big.NewInt(1), 5); got.Sign() != 0 {
		t.Fatalf("Aave half-up premium rounded a sub-half unit upward: %s", got)
	}
	if got := aavePercentMul(big.NewInt(1_000), 5); got.String() != "1" {
		t.Fatalf("Aave half-up premium boundary mismatch: %s", got)
	}
	if got := ceilBasisPoints(big.NewInt(1), 5); got.String() != "1" {
		t.Fatalf("policy ceiling helper no longer differs intentionally: %s", got)
	}
}

func TestLiquidationWinnerOrderingUsesConservativeExpectedThenSmallerRepay(t *testing.T) {
	evaluation := func(repay string, conservative, expected int64) *liquidationEvaluation {
		return &liquidationEvaluation{
			Liquidation:  &exactLiquidation{RepayAmount: repay},
			Simulation:   &simulationResponse{},
			Conservative: big.NewInt(conservative),
			Expected:     big.NewInt(expected),
		}
	}
	small := evaluation("25", 100, 110)
	maximum := evaluation("100", 101, 102)
	if !betterLiquidationEvaluation(maximum, small) {
		t.Fatal("higher conservative PnL did not make maximum size win")
	}
	if !betterLiquidationEvaluation(evaluation("75", 100, 120), small) {
		t.Fatal("higher post-cost expected PnL did not break conservative tie")
	}
	if !betterLiquidationEvaluation(small, evaluation("50", 100, 110)) {
		t.Fatal("smaller repay did not win an exact economics tie")
	}
}

func TestResolveExactContinuesPastFailedSizeAndBindsSelectedRepay(t *testing.T) {
	liquidations := []exactLiquidation{
		boundedTestLiquidation(wethAddress, 1_000, 5),
		boundedTestLiquidation(wethAddress, 2_000, 5),
		boundedTestLiquidation(wethAddress, 3_000, 5),
		boundedTestLiquidation(wethAddress, 4_000, 5),
	}
	liquidations[0].LiquidatorCollateral = "1111"
	liquidations[1].LiquidatorCollateral = "2401"
	liquidations[2].LiquidatorCollateral = "3212"
	liquidations[3].LiquidatorCollateral = "4152"
	realized := map[string]string{"1000": "110", "3000": "210", "4000": "150"}
	flash := map[string]string{"1000": "1", "2000": "1", "3000": "2", "4000": "2"}
	simulationCalls := 0
	simulationBatches := 0

	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/exact":
			var input exactRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			if input.MaximumInputAmount != "4000" {
				t.Fatalf("exact maximum input=%s", input.MaximumInputAmount)
			}
			primary := exactProvider{ProviderID: "primary", FlashPremiumBPS: 5, Liquidations: liquidations}
			secondary := primary
			secondary.ProviderID = "secondary"
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v2", ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
				Primary: primary, Secondary: secondary,
			})
		case "/v1/aave/simulate-batch":
			simulationBatches++
			var input simulationBatchRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			results := make([]simulationBatchResult, 0, len(input.Simulations))
			for _, simulation := range input.Simulations {
				simulationCalls++
				result := simulationBatchResult{RequestID: simulation.RequestID}
				if simulation.RepayAmount == "2000" {
					result.Error = &gatewayErrorContract{ErrorClass: "fork_simulation_failed", Retryable: false}
				} else {
					result.Response = testSimulationResponse(simulation, realized[simulation.RepayAmount], "10", "3", flash[simulation.RepayAmount])
				}
				results = append(results, result)
			}
			writeTestSimulationBatch(writer, input, results)
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()

	screener := economicTestScreener(server)
	record, err := screener.resolveExact(context.Background(), signal{
		Cursor: 1, Borrower: "0x1111111111111111111111111111111111111111",
	}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if simulationCalls != 8 || simulationBatches != 3 {
		t.Fatalf("independent size attempts=%d batches=%d want=8/3", simulationCalls, simulationBatches)
	}
	if !record.Authority || record.TerminalOutcome != "candidate" || record.ExecutionCandidate == nil {
		t.Fatalf("winner did not receive Candidate authority: %+v", record)
	}
	candidate := record.ExecutionCandidate
	if candidate.SelectedSize != "3000" || candidate.FlashAmount != "3000" || candidate.MaximumInputAmount != "4000" {
		t.Fatalf("selected repay identity drifted: size=%s flash=%s maximum=%s", candidate.SelectedSize, candidate.FlashAmount, candidate.MaximumInputAmount)
	}
	if candidate.MinimumProfit != "111" || candidate.RoutePayload.MinimumUnwindOutput != "3192" || candidate.GasLimit != 1 || candidate.MaxFeePerGas != "10" {
		t.Fatalf("bounded authority was not materialized: min_profit=%s min_unwind=%s gas=%d max_fee=%s", candidate.MinimumProfit, candidate.RoutePayload.MinimumUnwindOutput, candidate.GasLimit, candidate.MaxFeePerGas)
	}
	if record.ExpectedNetPnLWei != "200" || record.RiskReserveAmountWei != "20" || record.ConservativeNetPnLWei != "180" {
		t.Fatalf("corrected positive-edge authority mismatch: expected=%s reserve=%s conservative=%s", record.ExpectedNetPnLWei, record.RiskReserveAmountWei, record.ConservativeNetPnLWei)
	}
	if len(candidate.Legs) != 0 || len(candidate.TokenPath) != 1 || candidate.TokenPath[0] != wethAddress || record.SelectedRoute != "WETH_IDENTITY" {
		t.Fatalf("WETH identity route drifted: route=%s path=%v legs=%v", record.SelectedRoute, candidate.TokenPath, candidate.Legs)
	}
}

func TestResolveExactRanksConvergedVariantsAndContinuesPastFinalFailure(t *testing.T) {
	type variantSpec struct {
		collateral string
		repay      int64
		edge       int64
	}
	specs := []variantSpec{
		{wethAddress, 1_000, 200},
		{wethAddress, 2_000, 250},
		{wethAddress, 3_000, 300},
		{wethAddress, 4_000, 500},
		{nativeUSDCAddress, 1_000, 210},
		{nativeUSDCAddress, 2_000, 270},
		{nativeUSDCAddress, 3_000, 290},
		{nativeUSDCAddress, 4_000, 400},
	}
	liquidations := make([]exactLiquidation, 0, len(specs))
	realizedByVariant := make(map[string]string, len(specs))
	flashByVariant := make(map[string]string, len(specs))
	for _, spec := range specs {
		liquidation := boundedTestLiquidation(spec.collateral, spec.repay, 5)
		premium, _ := newBigUint(liquidation.FlashPremiumAmount)
		output := new(big.Int).Add(big.NewInt(spec.repay+spec.edge), premium).String()
		if spec.collateral == wethAddress {
			liquidation.LiquidatorCollateral = output
		} else {
			liquidation.UniswapFee500OutputWETH = output
			quotedOutput, _ := newBigUint(output)
			liquidation.UniswapFee3000OutputWETH = new(big.Int).Sub(quotedOutput, big.NewInt(1)).String()
		}
		key := spec.collateral + "|" + liquidation.RepayAmount
		realizedByVariant[key] = strconv.FormatInt(spec.edge, 10)
		flashByVariant[key] = liquidation.FlashPremiumAmount
		liquidations = append(liquidations, liquidation)
	}
	probeWinnerKey := wethAddress + "|4000"
	failedFinalKey := wethAddress + "|3000"
	finalWinnerKey := nativeUSDCAddress + "|4000"
	exactCalls := 0
	simulationCalls := 0
	simulationBatches := 0
	probeCalls := make(map[string]int)
	materializationCalls := make(map[string]int)
	freshWinnerHash := ""
	freshDeadline := uint64(0)
	priorDeadline := uint64(0)

	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/exact":
			exactCalls++
			var input exactRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			primary := exactProvider{ProviderID: "primary", FlashPremiumBPS: 5, Liquidations: liquidations}
			secondary := primary
			secondary.ProviderID = "secondary"
			blockNumber := uint64(100)
			blockHash := "0x" + strings.Repeat("a", 64)
			stateRoot := "0x" + strings.Repeat("b", 64)
			if exactCalls == 2 {
				blockNumber = 101
				blockHash = "0x" + strings.Repeat("e", 64)
				stateRoot = "0x" + strings.Repeat("f", 64)
			}
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v2", ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: blockNumber, BlockHash: blockHash, StateRoot: stateRoot,
				Primary: primary, Secondary: secondary,
			})
		case "/v1/aave/simulate-batch":
			simulationBatches++
			var input simulationBatchRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			if simulationBatches == 4 {
				last := input.Simulations[len(input.Simulations)-1]
				if key := strings.ToLower(last.CollateralAsset) + "|" + last.RepayAmount; key != finalWinnerKey {
					t.Fatalf("pre-fresh winner was not simulated last: last=%s want=%s", key, finalWinnerKey)
				}
				freshDeadline = last.DeadlineUnixSeconds
			} else if len(input.Simulations) > 0 {
				priorDeadline = input.Simulations[0].DeadlineUnixSeconds
			}
			results := make([]simulationBatchResult, 0, len(input.Simulations))
			for _, simulation := range input.Simulations {
				simulationCalls++
				key := strings.ToLower(simulation.CollateralAsset) + "|" + simulation.RepayAmount
				realized, ok := realizedByVariant[key]
				if !ok {
					t.Fatalf("unexpected simulated variant: %s", key)
				}
				cost := "10"
				result := simulationBatchResult{RequestID: simulation.RequestID}
				if simulation.MinimumUnwindOutput == "1" {
					probeCalls[key]++
					if simulation.MinimumProfit != "100" {
						t.Fatalf("probe minimum profit drifted for %s: %s", key, simulation.MinimumProfit)
					}
				} else {
					materializationCalls[key]++
					if key == failedFinalKey {
						result.Error = &gatewayErrorContract{ErrorClass: "fork_simulation_failed", Retryable: false}
						results = append(results, result)
						continue
					}
					if key == probeWinnerKey && simulationBatches != 4 {
						cost = "200"
					}
				}
				if simulationBatches == 4 && key == finalWinnerKey {
					result.Error = &gatewayErrorContract{ErrorClass: "fork_simulation_failed", Retryable: false}
					results = append(results, result)
					continue
				}
				result.Response = testSimulationResponse(simulation, realized, cost, "3", flashByVariant[key])
				if simulationBatches == 4 && key == probeWinnerKey {
					digest := sha256.Sum256([]byte("fresh-selected-evidence"))
					freshWinnerHash = hex.EncodeToString(digest[:])
					result.Response.SimulationResultHash = freshWinnerHash
				}
				results = append(results, result)
			}
			writeTestSimulationBatch(writer, input, results)
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()

	screener := economicTestScreener(server)
	baseTime := time.Now().UTC()
	clockCalls := 0
	screener.now = func() time.Time {
		clockCalls++
		return baseTime.Add(time.Duration(clockCalls) * time.Second)
	}
	record, err := screener.resolveExact(context.Background(), signal{
		Cursor: 1, Borrower: "0x1111111111111111111111111111111111111111",
	}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if !record.Authority || record.ExecutionCandidate == nil || record.ExecutionCandidate.SelectedSize != "4000" || record.SelectedRoute != "WETH_IDENTITY" {
		t.Fatalf("converged winner did not receive authority: %+v", record)
	}
	if exactCalls != 2 || simulationCalls != 24 || simulationBatches != 4 {
		t.Fatalf("independent materialization count drifted: exact=%d simulations=%d batches=%d", exactCalls, simulationCalls, simulationBatches)
	}
	if stateRequestsWithScreenAndTail := 1 + exactCalls + simulationBatches + 1; stateRequestsWithScreenAndTail != 8 || stateRequestsWithScreenAndTail >= 12 {
		t.Fatalf("Production state-request envelope drifted: requests=%d", stateRequestsWithScreenAndTail)
	}
	for key := range realizedByVariant {
		if probeCalls[key] != 1 {
			t.Fatalf("variant %s probe count=%d want=1", key, probeCalls[key])
		}
		wantMaterializations := 2
		if key == failedFinalKey {
			wantMaterializations = 1
		}
		if key == probeWinnerKey {
			wantMaterializations = 3
		}
		if materializationCalls[key] != wantMaterializations {
			t.Fatalf("variant %s materialization count=%d want=%d", key, materializationCalls[key], wantMaterializations)
		}
	}
	if len(materializationCalls) != len(specs) || materializationCalls[failedFinalKey] != 1 {
		t.Fatalf("viable variants were not independently materialized: %+v", materializationCalls)
	}
	if probeWinnerKey != strings.ToLower(record.ExecutionCandidate.RoutePayload.CollateralAsset)+"|"+record.ExecutionCandidate.SelectedSize || record.ExpectedNetPnLWei != "490" || record.ConservativeNetPnLWei != "441" || record.ExecutionCandidate.MinimumProfit != "301" || record.ExecutionCandidate.RoutePayload.MinimumUnwindOutput != "4472" || record.ExecutionCandidate.SimulationResultHash != freshWinnerHash || uint64(record.ExecutionCandidate.Deadline.Unix()) != freshDeadline {
		t.Fatalf("authority was not ranked and materialized from fresh economics/evidence: %+v", record)
	}
	if freshDeadline <= priorDeadline {
		t.Fatalf("selected evidence deadline was not refreshed: prior=%d fresh=%d", priorDeadline, freshDeadline)
	}
	if record.Block != 101 || record.BlockHash != "0x"+strings.Repeat("e", 64) || record.StateRoot != "0x"+strings.Repeat("f", 64) || record.ExecutionCandidate.PinnedBlockNumber != 101 || record.ExecutionCandidate.PinnedBlockHash != record.BlockHash || record.ExecutionCandidate.RoutePayload.StateRoot != record.StateRoot {
		t.Fatalf("Candidate was not rebound to the second Exact pin: %+v", record)
	}
}

func TestSnapshotDoesNotBlockWhileExactResolutionIsInFlight(t *testing.T) {
	borrower := "0x1111111111111111111111111111111111111111"
	exactStarted := make(chan struct{}, 1)
	releaseExact := make(chan struct{})
	released := false
	defer func() {
		if !released {
			close(releaseExact)
		}
	}()
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/screen":
			var input screenRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			accounts := []account{{
				Borrower: borrower, TotalDebtBase: "100000000", HealthFactorWAD: "900000000000000000",
			}}
			_ = json.NewEncoder(writer).Encode(screenResponse{
				SchemaVersion: ResponseSchema, ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64),
				Primary:   providerScreen{ProviderID: "primary", WETHPriceBase: "100000000", Accounts: accounts},
				Secondary: providerScreen{ProviderID: "secondary", WETHPriceBase: "100000000", Accounts: accounts},
			})
		case "/v1/aave/exact":
			var input exactRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			exactStarted <- struct{}{}
			select {
			case <-releaseExact:
			case <-request.Context().Done():
				return
			}
			primary := exactProvider{ProviderID: "primary"}
			secondary := primary
			secondary.ProviderID = "secondary"
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v2", ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
				Primary: primary, Secondary: secondary,
			})
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()

	screener := economicTestScreener(server)
	screener.config.StateDir = t.TempDir()
	screener.debtBearing = make(map[string]bool)
	screener.refreshKnown = make(map[string]bool)
	screenDone := make(chan error, 1)
	go func() {
		screenDone <- screener.screen(context.Background(), []string{borrower}, true, nil)
	}()
	select {
	case <-exactStarted:
	case <-time.After(2 * time.Second):
		t.Fatal("screen did not reach exact resolution")
	}
	snapshotDone := make(chan State, 1)
	go func() {
		snapshotDone <- screener.Snapshot()
	}()
	select {
	case snapshot := <-snapshotDone:
		if snapshot.Cursor != 0 {
			t.Fatalf("in-flight screen published its cursor early: %+v", snapshot)
		}
	case <-time.After(250 * time.Millisecond):
		close(releaseExact)
		released = true
		t.Fatal("Snapshot blocked behind exact gateway I/O")
	}
	close(releaseExact)
	released = true
	select {
	case err := <-screenDone:
		if err != nil {
			t.Fatal(err)
		}
	case <-time.After(2 * time.Second):
		t.Fatal("screen did not finish after exact resolution was released")
	}
}

func TestResolveExactNoProfitableSizeProducesNoCandidate(t *testing.T) {
	liquidation := boundedTestLiquidation(wethAddress, 1_000, 5)
	liquidation.LiquidatorCollateral = "1050"
	simulationCalls := 0
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.URL.Path == "/v1/aave/simulate-batch" {
			simulationCalls++
			t.Fatal("zero-cost economic rejection reached fork simulation")
		}
		var input exactRequest
		if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
			t.Fatal(err)
		}
		primary := exactProvider{ProviderID: "primary", FlashPremiumBPS: 5, Liquidations: []exactLiquidation{liquidation}}
		secondary := primary
		secondary.ProviderID = "secondary"
		_ = json.NewEncoder(writer).Encode(exactResponse{
			SchemaVersion: "phoenix.rpc.aave-exact-response.v2", ChainID: 42161, RequestID: input.RequestID,
			BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
			Primary: primary, Secondary: secondary,
		})
	}))
	defer server.Close()
	record, err := economicTestScreener(server).resolveExact(context.Background(), signal{Cursor: 1, Borrower: "0x1111111111111111111111111111111111111111"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if simulationCalls != 0 || record.Authority || record.ExecutionCandidate != nil || record.TerminalOutcome != "economic_rejection" {
		t.Fatalf("unprofitable grid emitted authority: calls=%d record=%+v", simulationCalls, record)
	}
}

func TestFreshExactQuoteChangeFailsClosedBeforeAuthority(t *testing.T) {
	original := boundedTestLiquidation(wethAddress, 1_000, 5)
	original.LiquidatorCollateral = "1201"
	exactCalls := 0
	simulationBatches := 0
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/exact":
			exactCalls++
			var input exactRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			liquidation := original
			if exactCalls == 2 {
				liquidation.LiquidatorCollateral = "1202"
			}
			primary := exactProvider{ProviderID: "primary", FlashPremiumBPS: 5, Liquidations: []exactLiquidation{liquidation}}
			secondary := primary
			secondary.ProviderID = "secondary"
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v2", ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: uint64(100 + exactCalls - 1), BlockHash: "0x" + strings.Repeat(strconv.Itoa(exactCalls), 64), StateRoot: "0x" + strings.Repeat(strconv.Itoa(exactCalls+2), 64),
				Primary: primary, Secondary: secondary,
			})
		case "/v1/aave/simulate-batch":
			simulationBatches++
			var input simulationBatchRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			results := make([]simulationBatchResult, 0, len(input.Simulations))
			for _, simulation := range input.Simulations {
				results = append(results, simulationBatchResult{RequestID: simulation.RequestID, Response: testSimulationResponse(simulation, "200", "10", "3", "1")})
			}
			writeTestSimulationBatch(writer, input, results)
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()
	record, err := economicTestScreener(server).resolveExact(context.Background(), signal{Cursor: 1, Borrower: "0x1111111111111111111111111111111111111111"}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if exactCalls != 2 || simulationBatches != 2 || record.Authority || record.ExecutionCandidate != nil || record.AtlasCandidate != nil || record.TerminalOutcome != "fork_pending" {
		t.Fatalf("changed fresh Exact quote crossed authority: exact=%d simulations=%d record=%+v", exactCalls, simulationBatches, record)
	}
}

func TestBoundedSimulationEconomicsIncludesL1ExactlyOnceAndFailsClosed(t *testing.T) {
	screener := &Screener{config: Config{MaximumGasLimit: 5, MaximumFeePerGasWei: "10"}}
	liquidation := &exactLiquidation{FlashPremiumAmount: "1"}
	simulation := &simulationResponse{
		RealizedProfit: "100", ConservativeNetPnL: "50", EstimatedGasLimit: 5,
		EstimatedMaxFeePerGasWei: "10", EstimatedExecutionCostWei: "50", EstimatedL1CostWei: "30", FlashPremiumWei: "1",
	}
	realized, cost, l1, err := screener.boundedSimulationEconomics(simulation, liquidation)
	if err != nil {
		t.Fatal(err)
	}
	net, err := authoritativeGatewayNet(simulation, realized, cost, big.NewInt(0))
	if err != nil || net.String() != "50" || l1.String() != "30" {
		t.Fatalf("bounded total/L1 attribution mismatch: net=%v l1=%s err=%v", net, l1, err)
	}
	invalid := *simulation
	invalid.EstimatedL1CostWei = "51"
	if _, _, _, err := screener.boundedSimulationEconomics(&invalid, liquidation); err == nil {
		t.Fatal("L1 component larger than included total cost was accepted")
	}
	invalid = *simulation
	invalid.ConservativeNetPnL = "49"
	if _, err := authoritativeGatewayNet(&invalid, realized, cost, big.NewInt(0)); err == nil {
		t.Fatal("gateway/observer economics drift was accepted")
	}
}

func TestGenerousUpperBoundUsesFullCloseAtExactThreshold(t *testing.T) {
	atThreshold, err := generousUpperBound(account{
		TotalDebtBase: "100", HealthFactorWAD: "950000000000000000",
	}, "100000000")
	if err != nil {
		t.Fatal(err)
	}
	aboveThreshold, err := generousUpperBound(account{
		TotalDebtBase: "100", HealthFactorWAD: "950000000000000001",
	}, "100000000")
	if err != nil {
		t.Fatal(err)
	}
	if atThreshold.String() != "500000000000" || aboveThreshold.String() != "250000000000" {
		t.Fatalf("close-factor boundary drifted: at=%s above=%s", atThreshold, aboveThreshold)
	}
}

func TestAtlasCandidateChargesBidAndMaximumSolverExposureOnce(t *testing.T) {
	simulationCalls := 0
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		simulationCalls++
		var batch simulationBatchRequest
		if err := json.NewDecoder(request.Body).Decode(&batch); err != nil {
			t.Fatal(err)
		}
		if len(batch.Simulations) != 1 {
			t.Fatalf("Atlas simulation batch size=%d", len(batch.Simulations))
		}
		input := batch.Simulations[0]
		if input.AtlasBid != "250" || input.MinimumProfit != "301" || input.MinimumUnwindOutput != "1945" {
			t.Fatalf("Atlas authority request drifted: bid=%s min_profit=%s min_unwind=%s", input.AtlasBid, input.MinimumProfit, input.MinimumUnwindOutput)
		}
		result := testSimulationResponse(input, "1000", "50", "20", "1")
		result.EvidenceMode = atlasCallbackEvidenceMode
		_ = json.NewEncoder(writer).Encode(simulationBatchResponse{
			SchemaVersion: "phoenix.rpc.aave-simulate-batch-response.v1", ChainID: 42161, RequestID: batch.RequestID,
			BlockNumber: input.BlockNumber, BlockHash: input.BlockHash, StateRoot: input.StateRoot,
			PrimaryProviderID: "primary", SecondaryProviderID: "secondary", EvidenceMode: atlasCallbackEvidenceMode,
			Results: []simulationBatchResult{{RequestID: input.RequestID, Response: result}},
		})
	}))
	defer server.Close()
	screener := economicTestScreener(server)
	screener.config.MaximumInputAmountWei = "10000"
	screener.config.MaximumGasLimit = 100
	screener.config.MaximumFeePerGasWei = "10"
	screener.config.MaximumPriorityFeeWei = "10"
	screener.config.MaximumAtlasBidWei = "500"
	liquidation := boundedTestLiquidation(wethAddress, 1_000, 5)
	liquidation.LiquidatorCollateral = "2000"
	selected := &liquidationEvaluation{
		Liquidation:   liquidationPtr(liquidation),
		Route:         liquidationRoute{Name: "WETH_IDENTITY", Output: big.NewInt(2_000), SelectedPool: zeroAddress, Factory: zeroAddress, TokenPath: []string{wethAddress}},
		Simulation:    &simulationResponse{RealizedProfit: "1000", EvidenceMode: directForkEvidenceMode},
		ExecutionCost: big.NewInt(50), MinimumCollateral: "2000", MinimumUnwind: "1900",
	}
	auction := &observer.LedgerRecord{
		RelevantAaveAuction: true, AuctionID: "auction", AuctionDeadlineBlock: "500000000",
		SolverGasLimit: 20, OracleGasPriceWei: "10", Atlas: "0x3333333333333333333333333333333333333333",
		DappControl: "0x4444444444444444444444444444444444444444", UserOpHash: "0x" + strings.Repeat("c", 64),
		ObservedAt: time.Now().UTC(),
	}
	record := signal{
		Cursor: 1, Borrower: "0x1111111111111111111111111111111111111111", Block: 100,
		BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
	}
	if rejected, err := screener.buildAtlasCandidate(context.Background(), record, selected, auction); err != nil || rejected != nil || simulationCalls != 0 {
		t.Fatalf("direct-wrapper evidence authorized Atlas: candidate=%+v calls=%d err=%v", rejected, simulationCalls, err)
	}
	selected.Simulation.EvidenceMode = atlasCallbackEvidenceMode
	candidate, err := screener.buildAtlasCandidate(context.Background(), record, selected, auction)
	if err != nil {
		t.Fatal(err)
	}
	if candidate == nil || candidate.MaximumBid != "500" || candidate.SelectedBid != "250" || candidate.ExpectedNetPnL != "550" || candidate.ConservativeNetPnL != "495" {
		t.Fatalf("Atlas bid/gas/bond economics were counted incorrectly: %+v", candidate)
	}
	if candidate.Operation.Gas != 20 || candidate.Operation.MaxFeePerGas != "10" || candidate.Operation.BidAmount != "250" {
		t.Fatalf("Atlas operation bounds drifted: %+v", candidate.Operation)
	}

	selected.Simulation.EstimatedGasLimit = 21
	if rejected, err := screener.buildAtlasCandidate(context.Background(), record, selected, auction); err == nil || rejected != nil {
		t.Fatalf("Atlas solver gas below the fork-verified requirement was not rejected: candidate=%+v err=%v", rejected, err)
	}
	selected.Simulation.EstimatedGasLimit = 0

	auction.OracleGasPriceWei = "11"
	if rejected, err := screener.buildAtlasCandidate(context.Background(), record, selected, auction); err == nil || rejected != nil {
		t.Fatalf("Atlas oracle price above priority ceiling was not rejected: candidate=%+v err=%v", rejected, err)
	}
	screener.config.MaximumAtlasBidWei = "0"
	auction.OracleGasPriceWei = "10"
	if disabled, err := screener.buildAtlasCandidate(context.Background(), record, selected, auction); err != nil || disabled != nil {
		t.Fatalf("zero Atlas cap did not disable only Atlas authority: candidate=%+v err=%v", disabled, err)
	}
}

func TestAuctionWithDirectWrapperEvidencePersistsNoLaneArtifact(t *testing.T) {
	liquidation := boundedTestLiquidation(wethAddress, 1_000, 5)
	liquidation.LiquidatorCollateral = "1201"
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/exact":
			var input exactRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			primary := exactProvider{ProviderID: "primary", FlashPremiumBPS: 5, Liquidations: []exactLiquidation{liquidation}}
			secondary := primary
			secondary.ProviderID = "secondary"
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v2", ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
				Primary: primary, Secondary: secondary,
			})
		case "/v1/aave/simulate-batch":
			var input simulationBatchRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			results := make([]simulationBatchResult, 0, len(input.Simulations))
			for _, simulation := range input.Simulations {
				results = append(results, simulationBatchResult{
					RequestID: simulation.RequestID,
					Response:  testSimulationResponse(simulation, "200", "10", "3", "1"),
				})
			}
			writeTestSimulationBatch(writer, input, results)
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()
	screener := economicTestScreener(server)
	record, err := screener.resolveExact(context.Background(), signal{
		Schema: "phoenix.atlas-aave-hunting-signal.v1", Cursor: 1,
		Borrower: "0x1111111111111111111111111111111111111111",
	}, &observer.LedgerRecord{RelevantAaveAuction: true})
	if err != nil {
		t.Fatal(err)
	}
	if record.Authority || record.ExecutionCandidate != nil || record.AtlasCandidate != nil || record.TerminalOutcome != "atlas_evidence_rejection" || record.AuthorityRejectionReason != "atlas_callback_evidence_unavailable" {
		t.Fatalf("auction emitted a bypass/duplicate lane artifact: %+v", record)
	}
	sink := &recordingSignalSink{}
	if err := sink.RecordAaveSignal(context.Background(), record); err != nil {
		t.Fatal(err)
	}
	if len(sink.records) != 1 || sink.records[0].ExecutionCandidate != nil || sink.records[0].AtlasCandidate != nil || rejectionReason(sink.records[0].TerminalOutcome) != "atlas_callback_evidence_unavailable" {
		t.Fatalf("fail-closed auction outcome drifted at the signal/DB boundary: %+v", sink.records)
	}
}

func boundedTestLiquidation(collateral string, amount int64, premiumBPS uint64) exactLiquidation {
	repay := strconv.FormatInt(amount, 10)
	premium := aavePercentMul(big.NewInt(amount), premiumBPS).String()
	return exactLiquidation{
		DebtAsset: wethAddress, CollateralAsset: collateral,
		RequestedRepayAmount: repay, ActualRepayAmount: repay, RepayAmount: repay,
		FlashPremiumAmount: premium, LiquidatorCollateral: strconv.FormatInt(amount+1, 10),
		UniswapFee500OutputWETH:  strconv.FormatInt(amount+1, 10),
		UniswapFee3000OutputWETH: strconv.FormatInt(amount, 10),
	}
}

func economicTestScreener(server *httptest.Server) *Screener {
	return &Screener{
		config: Config{
			GatewayURL: server.URL, RetainedProfitFloorWei: "100", MaximumInputAmountWei: "4000",
			MaximumGasLimit: 100, MaximumFeePerGasWei: "10", MaximumPriorityFeeWei: "10",
			MaximumAtlasBidWei: "500", FlashPremiumBPS: 5, EconomicReserveBPS: 1_000,
			ExecutorAddress: "0x2222222222222222222222222222222222222222", ExecutorCodeHash: strings.Repeat("c", 64),
			CallerAddress: "0x3333333333333333333333333333333333333333", ReleaseSHA: strings.Repeat("d", 40),
		},
		client: server.Client(), state: State{Schema: StateSchema, Counts: map[string]uint64{}}, now: time.Now().UTC,
	}
}

func testSimulationResponse(input simulationRequest, realizedText, costText, l1Text, flashText string) *simulationResponse {
	realized, _ := newBigUint(realizedText)
	cost, _ := newBigUint(costText)
	bid, _ := newBigUint(input.AtlasBid)
	maximumFee, _ := newBigUint(input.MaxFeePerGas)
	estimatedGas := new(big.Int).Add(new(big.Int).Set(cost), new(big.Int).Sub(maximumFee, big.NewInt(1)))
	estimatedGas.Div(estimatedGas, maximumFee)
	conservative := new(big.Int).Sub(realized, cost)
	conservative.Sub(conservative, bid)
	if conservative.Sign() < 0 {
		conservative.SetInt64(0)
	}
	calldata := []byte("bound|" + input.RepayAmount + "|" + input.MinimumProfit + "|" + input.AtlasBid)
	calldataHash := sha256.Sum256(calldata)
	routeHash := sha256.Sum256([]byte("route|" + input.RepayAmount + "|" + input.MinimumProfit + "|" + input.AtlasBid))
	resultHash := sha256.Sum256([]byte("result|" + input.RepayAmount + "|" + input.MinimumProfit + "|" + input.AtlasBid))
	return &simulationResponse{
		SchemaVersion: "phoenix.rpc.aave-simulate-response.v2", ChainID: 42161, RequestID: input.RequestID,
		BlockNumber: input.BlockNumber, BlockHash: input.BlockHash, StateRoot: input.StateRoot,
		PrimaryProviderID: "primary", SecondaryProviderID: "secondary", EvidenceMode: "DUAL_PROVIDER_FORK_VERIFIED",
		RouteID: "0x" + hex.EncodeToString(routeHash[:]), CalldataHex: "0x" + hex.EncodeToString(calldata),
		CalldataHash: hex.EncodeToString(calldataHash[:]), SimulationResultHash: hex.EncodeToString(resultHash[:]),
		RealizedProfit: realizedText, ConservativeNetPnL: conservative.String(), EstimatedGasLimit: estimatedGas.Uint64(),
		EstimatedMaxFeePerGasWei: input.MaxFeePerGas, EstimatedExecutionCostWei: costText, EstimatedL1CostWei: l1Text, FlashPremiumWei: flashText,
		DeadlineUnixSeconds: input.DeadlineUnixSeconds,
	}
}

func writeTestSimulationBatch(writer http.ResponseWriter, input simulationBatchRequest, results []simulationBatchResult) {
	first := input.Simulations[0]
	_ = json.NewEncoder(writer).Encode(simulationBatchResponse{
		SchemaVersion: "phoenix.rpc.aave-simulate-batch-response.v1", ChainID: 42161, RequestID: input.RequestID,
		BlockNumber: first.BlockNumber, BlockHash: first.BlockHash, StateRoot: first.StateRoot,
		PrimaryProviderID: "primary", SecondaryProviderID: "secondary", EvidenceMode: "DUAL_PROVIDER_FORK_VERIFIED",
		Results: results,
	})
}

func liquidationPtr(liquidation exactLiquidation) *exactLiquidation { return &liquidation }

func TestRouteAwareExactBudgetLearnsPairIneligibility(t *testing.T) {
	eligible := []exactReserve{
		{
			Asset:                wethAddress,
			CurrentATokenBalance: "0",
			CurrentStableDebt:    "0",
			CurrentVariableDebt:  "2500000000000000",
		},
		{
			Asset:                    nativeUSDCAddress,
			CurrentATokenBalance:     "1000000000",
			UsageAsCollateralEnabled: true,
		},
	}
	if got := exactRouteIneligibleReason(eligible); got != "" {
		t.Fatalf("eligible pair unexpectedly rejected: %q", got)
	}

	noWETHDebt := append([]exactReserve(nil), eligible...)
	noWETHDebt[0].CurrentVariableDebt = "0"
	if got := exactRouteIneligibleReason(noWETHDebt); got != "no_weth_debt" {
		t.Fatalf("unexpected no-WETH-debt diagnostic: %q", got)
	}

	noUSDC := append([]exactReserve(nil), eligible...)
	noUSDC[1].CurrentATokenBalance = "0"
	if got := exactRouteIneligibleReason(noUSDC); got != "no_supported_collateral" {
		t.Fatalf("unexpected no-USDC-collateral diagnostic: %q", got)
	}

	notCollateral := append([]exactReserve(nil), eligible...)
	notCollateral[1].UsageAsCollateralEnabled = false
	if got := exactRouteIneligibleReason(notCollateral); got != "supported_collateral_disabled" {
		t.Fatalf("unexpected collateral-disabled diagnostic: %q", got)
	}

	wethIdentity := append([]exactReserve(nil), noUSDC...)
	wethIdentity[0].CurrentATokenBalance = "1000000000000000"
	wethIdentity[0].UsageAsCollateralEnabled = true
	if got := exactRouteIneligibleReason(wethIdentity); got != "" {
		t.Fatalf("WETH identity collateral was not recognized: %q", got)
	}

	stableDebt := append([]exactReserve(nil), eligible...)
	stableDebt[0].CurrentStableDebt = "1"
	if got := exactRouteIneligibleReason(stableDebt); got != "unsupported_stable_weth_debt" {
		t.Fatalf("stable WETH debt did not fail closed borrower-locally: %q", got)
	}
}

func TestStableDebtIsBorrowerScopedAndDoesNotAbortScreenBatch(t *testing.T) {
	stableBorrower := "0x1111111111111111111111111111111111111111"
	otherBorrower := "0x2222222222222222222222222222222222222222"
	exactCalls := make([]string, 0, 2)
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/screen":
			var input screenRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			accounts := []account{
				{Borrower: stableBorrower, TotalDebtBase: "100000000", HealthFactorWAD: "900000000000000000"},
				{Borrower: otherBorrower, TotalDebtBase: "100000000", HealthFactorWAD: "900000000000000000"},
			}
			providers := providerScreen{ProviderID: "primary", WETHPriceBase: "100000000", Accounts: accounts}
			secondary := providers
			secondary.ProviderID = "secondary"
			_ = json.NewEncoder(writer).Encode(screenResponse{
				SchemaVersion: ResponseSchema, ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), Primary: providers, Secondary: secondary,
			})
		case "/v1/aave/exact":
			var input exactRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			exactCalls = append(exactCalls, input.Borrower)
			stable := "0"
			variable := "0"
			if input.Borrower == stableBorrower {
				stable = "1"
				variable = "10"
			}
			primary := exactProvider{ProviderID: "primary", Reserves: []exactReserve{
				{Asset: wethAddress, CurrentATokenBalance: "0", CurrentStableDebt: stable, CurrentVariableDebt: variable},
				{Asset: nativeUSDCAddress, CurrentATokenBalance: "1000", UsageAsCollateralEnabled: true},
			}}
			secondary := primary
			secondary.ProviderID = "secondary"
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v2", ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
				Primary: primary, Secondary: secondary,
			})
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()
	screener := economicTestScreener(server)
	sink := &recordingSignalSink{}
	screener.config.SignalSink = sink
	screener.config.StateDir = t.TempDir()
	screener.debtBearing = make(map[string]bool)
	screener.refreshKnown = make(map[string]bool)
	screener.hotBorrowers = make(map[string]string)
	if err := screener.screen(context.Background(), []string{stableBorrower, otherBorrower}, false, nil); err != nil {
		t.Fatal(err)
	}
	if len(exactCalls) != 2 || exactCalls[0] != stableBorrower || exactCalls[1] != otherBorrower {
		t.Fatalf("stable debt aborted or reordered the borrower batch: %v", exactCalls)
	}
	state := screener.Snapshot()
	if state.RouteIneligible[stableBorrower] != "unsupported_stable_weth_debt" || state.RouteIneligible[otherBorrower] != "no_weth_debt" {
		t.Fatalf("borrower-scoped route diagnoses were not persisted: routes=%+v signals=%+v", state.RouteIneligible, sink.records)
	}
}

func TestCollateralRouteIneligibilityExpiresAndSupportedExactClearsIt(t *testing.T) {
	borrower := "0x1111111111111111111111111111111111111111"
	now := time.Date(2026, 8, 9, 0, 0, 0, 0, time.UTC)
	exactCalls := 0
	liquidation := boundedTestLiquidation(wethAddress, 1_000, 5)
	liquidation.LiquidatorCollateral = "1200"
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/screen":
			var input screenRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			accounts := []account{{Borrower: borrower, TotalDebtBase: "100000000", HealthFactorWAD: "900000000000000000"}}
			primary := providerScreen{ProviderID: "primary", WETHPriceBase: "100000000", Accounts: accounts}
			secondary := primary
			secondary.ProviderID = "secondary"
			_ = json.NewEncoder(writer).Encode(screenResponse{SchemaVersion: ResponseSchema, ChainID: 42161, RequestID: input.RequestID, BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), Primary: primary, Secondary: secondary})
		case "/v1/aave/exact":
			exactCalls++
			var input exactRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			primary := exactProvider{ProviderID: "primary", FlashPremiumBPS: 5, Liquidations: []exactLiquidation{liquidation}}
			secondary := primary
			secondary.ProviderID = "secondary"
			_ = json.NewEncoder(writer).Encode(exactResponse{SchemaVersion: "phoenix.rpc.aave-exact-response.v2", ChainID: 42161, RequestID: input.RequestID, BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64), Primary: primary, Secondary: secondary})
		case "/v1/aave/simulate-batch":
			var input simulationBatchRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			results := make([]simulationBatchResult, len(input.Simulations))
			for index, simulation := range input.Simulations {
				results[index] = simulationBatchResult{RequestID: simulation.RequestID, Error: &gatewayErrorContract{ErrorClass: "fork_simulation_failed", Retryable: false}}
			}
			writeTestSimulationBatch(writer, input, results)
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()
	screener := economicTestScreener(server)
	screener.config.StateDir = t.TempDir()
	screener.now = func() time.Time { return now }
	screener.debtBearing = make(map[string]bool)
	screener.refreshKnown = make(map[string]bool)
	screener.hotBorrowers = make(map[string]string)
	screener.lastExactAt = map[string]time.Time{borrower: now.Add(-time.Minute)}
	screener.state.RouteIneligible = map[string]string{borrower: "no_supported_collateral"}
	if err := screener.screen(context.Background(), []string{borrower}, false, nil); err != nil {
		t.Fatal(err)
	}
	if exactCalls != 0 || screener.Snapshot().RouteIneligible[borrower] != "no_supported_collateral" {
		t.Fatalf("collateral route bypassed cooldown: calls=%d state=%+v", exactCalls, screener.Snapshot().RouteIneligible)
	}
	now = now.Add(3 * time.Minute)
	if err := screener.screen(context.Background(), []string{borrower}, false, nil); err != nil {
		t.Fatal(err)
	}
	if exactCalls != 1 {
		t.Fatalf("expired collateral route was not re-probed: calls=%d", exactCalls)
	}
	if _, exists := screener.Snapshot().RouteIneligible[borrower]; exists {
		t.Fatalf("supported Exact did not clear stale route diagnosis: %+v", screener.Snapshot().RouteIneligible)
	}
}

func TestRouteAwareExactBudgetDeprioritizesKnownIneligibleBorrower(t *testing.T) {
	ineligible := "0x1111111111111111111111111111111111111111"
	unknown := "0x2222222222222222222222222222222222222222"

	screener := &Screener{
		hotBorrowers: map[string]string{
			ineligible: "900000000000000000",
			unknown:    "990000000000000000",
		},
		hotDebtBase: map[string]string{
			ineligible: "1000000000",
			unknown:    "100",
		},
		state: State{
			RouteIneligible: map[string]string{
				ineligible: "no_weth_debt",
			},
		},
	}
	batch := screener.nextHotBatch()
	if len(batch) != 2 || batch[0] != unknown || batch[1] != ineligible {
		t.Fatalf("route-ineligible borrower was not deferred: %v", batch)
	}
}
func TestRouteIneligibleStatePersistsAndTailInvalidatesDurably(t *testing.T) {
	directory := t.TempDir()
	borrower := "0x1111111111111111111111111111111111111111"
	discoveryHash := strings.Repeat("a", 64)

	first := &Screener{
		config: Config{
			StateDir:        directory,
			DiscoverySHA256: discoveryHash,
			StartingCursor:  0,
		},
		state: State{
			Schema:          StateSchema,
			DiscoverySHA256: discoveryHash,
			Counts:          map[string]uint64{},
			RouteIneligible: map[string]string{borrower: "no_weth_debt"},
		},
	}
	if err := first.persistState(); err != nil {
		t.Fatal(err)
	}

	second := &Screener{
		config: Config{
			StateDir:        directory,
			DiscoverySHA256: discoveryHash,
			StartingCursor:  0,
		},
		state: State{},
	}
	if err := second.loadState(); err != nil {
		t.Fatal(err)
	}
	if second.state.RouteIneligible[borrower] != "no_weth_debt" {
		t.Fatalf("route-ineligible state did not survive restart: %+v", second.state.RouteIneligible)
	}

	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.URL.Path != "/v1/aave/tail" {
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
		var input tailRequest
		if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
			t.Fatal(err)
		}
		_ = json.NewEncoder(writer).Encode(tailResponse{
			SchemaVersion:        "phoenix.rpc.aave-tail-response.v1",
			ChainID:              42161,
			RequestID:            input.RequestID,
			FinalizedBlockNumber: 100,
			FinalizedBlockHash:   "0x" + strings.Repeat("b", 64),
			FromBlock:            100,
			ToBlock:              100,
			NextBlock:            101,
			PrimaryProviderID:    "production-nownodes-arbitrum",
			SecondaryProviderID:  "production-slot-0",
			Borrowers:            []string{borrower},
		})
	}))
	defer server.Close()

	second.config.GatewayURL = server.URL
	second.client = server.Client()
	second.state.TailNextBlock = 100
	second.debtBearing = make(map[string]bool)
	second.refreshKnown = make(map[string]bool)

	borrowers, err := second.pollTail(context.Background())
	if err != nil {
		t.Fatal(err)
	}
	if len(borrowers) != 1 || borrowers[0] != borrower {
		t.Fatalf("unexpected tail borrowers: %v", borrowers)
	}
	if _, exists := second.state.RouteIneligible[borrower]; exists {
		t.Fatalf("tail event did not clear route-ineligible state: %+v", second.state.RouteIneligible)
	}

	var durable State
	data, err := os.ReadFile(filepath.Join(directory, "state.json"))
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(data, &durable); err != nil {
		t.Fatal(err)
	}
	if _, exists := durable.RouteIneligible[borrower]; exists {
		t.Fatalf("tail invalidation was not persisted: %+v", durable.RouteIneligible)
	}
}

func TestCurrentRouteIneligibleReasonsSurviveRestart(t *testing.T) {
	directory := t.TempDir()
	discoveryHash := strings.Repeat("a", 64)
	firstBorrower := "0x1111111111111111111111111111111111111111"
	secondBorrower := "0x2222222222222222222222222222222222222222"
	first := &Screener{
		config: Config{StateDir: directory, DiscoverySHA256: discoveryHash},
		state: State{
			Schema: StateSchema, DiscoverySHA256: discoveryHash, Counts: map[string]uint64{},
			RouteIneligible: map[string]string{
				firstBorrower:  "no_supported_collateral",
				secondBorrower: "supported_collateral_disabled",
			},
		},
	}
	if err := first.persistState(); err != nil {
		t.Fatal(err)
	}
	second := &Screener{config: Config{StateDir: directory, DiscoverySHA256: discoveryHash}}
	if err := second.loadState(); err != nil {
		t.Fatal(err)
	}
	if second.state.RouteIneligible[firstBorrower] != "no_supported_collateral" ||
		second.state.RouteIneligible[secondBorrower] != "supported_collateral_disabled" {
		t.Fatalf("current route-ineligible reasons did not survive restart: %+v", second.state.RouteIneligible)
	}
}

func TestLegacyUSDCCollateralReasonsAreRevalidatedOnRestart(t *testing.T) {
	directory := t.TempDir()
	discoveryHash := strings.Repeat("a", 64)
	firstBorrower := "0x1111111111111111111111111111111111111111"
	secondBorrower := "0x2222222222222222222222222222222222222222"
	first := &Screener{
		config: Config{StateDir: directory, DiscoverySHA256: discoveryHash},
		state: State{
			Schema: StateSchema, DiscoverySHA256: discoveryHash, Counts: map[string]uint64{},
			RouteIneligible: map[string]string{
				firstBorrower:  "no_native_usdc_collateral",
				secondBorrower: "native_usdc_not_collateral",
			},
		},
	}
	if err := first.persistState(); err != nil {
		t.Fatal(err)
	}
	second := &Screener{config: Config{StateDir: directory, DiscoverySHA256: discoveryHash}}
	if err := second.loadState(); err != nil {
		t.Fatal(err)
	}
	if len(second.state.RouteIneligible) != 0 {
		t.Fatalf("legacy USDC-only route reasons were not invalidated: %+v", second.state.RouteIneligible)
	}
}
