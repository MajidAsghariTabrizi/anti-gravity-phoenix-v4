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
		MaximumGasLimit:        1,
		MaximumFeePerGasWei:    "1",
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
