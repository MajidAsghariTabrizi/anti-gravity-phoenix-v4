package hunter

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/big"
	"net/http"
	"net/http/httptest"
	"os"
	"path/filepath"
	"sort"
	"strconv"
	"strings"
	"sync"
	"sync/atomic"
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
			if state.Counts[providerDegradationTotalKey] != 1 || state.Counts[providerRecoveryAttemptTotalKey] != 2 || state.Counts[providerDegradedSinceMillisKey] == 0 || state.Counts[providerCurrentFailureStreakKey] != 1 {
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

func TestProviderFailureStreakCountsReopenedCircuitAndResetsOnRecovery(t *testing.T) {
	now := time.Date(2026, 8, 6, 0, 0, 0, 0, time.UTC)
	screener := &Screener{
		config: Config{StateDir: t.TempDir()},
		state:  State{Schema: StateSchema, Counts: map[string]uint64{}},
		now:    func() time.Time { return now },
	}
	failure := &gatewayResponseError{statusCode: http.StatusBadGateway, class: "provider_disagreement"}
	if accepted, err := screener.RecordRetryableGatewayError(failure); err != nil || !accepted {
		t.Fatalf("first provider failure was not accepted: accepted=%t err=%v", accepted, err)
	}
	now = now.Add(providerCircuitCooldown + time.Second)
	if accepted, err := screener.RecordRetryableGatewayError(failure); err != nil || !accepted {
		t.Fatalf("reopened provider failure was not accepted: accepted=%t err=%v", accepted, err)
	}
	if got := screener.Snapshot().Counts[providerCurrentFailureStreakKey]; got != 2 {
		t.Fatalf("current provider failure streak=%d want=2", got)
	}
	screener.mu.Lock()
	for sample := 0; sample < 3; sample++ {
		now = now.Add(time.Second)
		screener.recordProviderRecoveryLocked(now, primaryProviderID)
	}
	screener.mu.Unlock()
	now = now.Add(time.Second)
	if accepted, err := screener.RecordRetryableGatewayError(failure); err != nil || !accepted {
		t.Fatalf("post-recovery provider failure was not accepted: accepted=%t err=%v", accepted, err)
	}
	if got := screener.Snapshot().Counts[providerCurrentFailureStreakKey]; got != 1 {
		t.Fatalf("post-recovery provider failure streak=%d want=1", got)
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
		case "/v1/aave/screen", "/v1/aave/screen-primary":
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
					Confirmation: nil,
					Quorum:       1,
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
				SchemaVersion:        "phoenix.rpc.aave-tail-response.v2",
				ChainID:              42161,
				RequestID:            input.RequestID,
				FinalizedBlockNumber: 491300000,
				FinalizedBlockHash:   "0x" + strings.Repeat("a", 64),
				PrimaryProviderID:    "production-nownodes-arbitrum",
				ConfirmationProvider: nil,
				Quorum:               1,
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
	if state.Cursor != 1 || state.ProviderCircuitOpenUntilUnixMillis == 0 || state.LastErrorClass != "provider_unavailable" {
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
		t.Fatalf("Atlas callback capability rejection emitted RPC work: requests=%d", requests)
	}
	state := screener.Snapshot()
	if state.ProviderCircuitSkippedTotal != 0 || state.Counts[atlasCallbackUnavailableKey] != 1 {
		t.Fatalf("Atlas callback capability rejection was misclassified: %+v", state)
	}
}

type recordingSignalSink struct {
	records                     []signal
	atlasCallbackUnavailableIDs []string
	atlasCallbackEvidenceHashes []string
	providerFailures            []string
	providerResets              []string
}

func (s *recordingSignalSink) RecordProviderFailure(_ context.Context, reason string, _ time.Time) error {
	s.providerFailures = append(s.providerFailures, reason)
	return nil
}

func (s *recordingSignalSink) ResetProviderRecoveryEvidence(_ context.Context, reason string, _ time.Time) error {
	s.providerResets = append(s.providerResets, reason)
	return nil
}

func (s *recordingSignalSink) RecordAaveSignal(_ context.Context, record signal) (signal, error) {
	s.records = append(s.records, record)
	return record, nil
}

func (s *recordingSignalSink) RecordAtlasCallbackUnavailable(_ context.Context, auctionID, evidenceHash string) error {
	s.atlasCallbackUnavailableIDs = append(s.atlasCallbackUnavailableIDs, auctionID)
	s.atlasCallbackEvidenceHashes = append(s.atlasCallbackEvidenceHashes, evidenceHash)
	return nil
}

type liveAuthoritySignalSink struct {
	recordingSignalSink
	maximum string
	err     error
}

func (s *liveAuthoritySignalSink) CurrentAaveLiveMaximumInputAmount(context.Context) (string, error) {
	return s.maximum, s.err
}

func TestProviderFailureClosesOnlyDurableExactGateAndRestartResetsSamples(t *testing.T) {
	now := time.Date(2026, 8, 12, 0, 0, 0, 0, time.UTC)
	sink := &recordingSignalSink{}
	screener := &Screener{
		config: Config{StateDir: t.TempDir(), SignalSink: sink},
		state: State{Schema: StateSchema, Counts: map[string]uint64{}, ProviderRecoverySamples: []ProviderRecoverySample{{
			ObservedAt: now.Add(-time.Second), PrimaryProvider: "primary", ConfirmationProvider: "confirmation",
		}}},
		now: func() time.Time { return now },
	}
	accepted, err := screener.RecordRetryableGatewayError(&gatewayResponseError{
		statusCode: http.StatusServiceUnavailable, class: "provider_unavailable", retryable: true,
	})
	if err != nil || !accepted || len(sink.providerFailures) != 1 || sink.providerFailures[0] != "provider_unavailable" {
		t.Fatalf("provider gate was not closed: accepted=%t err=%v sink=%+v", accepted, err, sink)
	}
	if len(screener.Snapshot().ProviderRecoverySamples) != 0 {
		t.Fatalf("pre-failure samples survived degradation: %+v", screener.Snapshot())
	}
	if err := screener.ResetProviderRecoveryEvidence(context.Background()); err != nil {
		t.Fatal(err)
	}
	state := screener.Snapshot()
	if len(sink.providerResets) != 1 || sink.providerResets[0] != "observer_restart" || state.LastDualAgreementAt != nil || len(state.ProviderRecoverySamples) != 0 {
		t.Fatalf("restart recovery evidence did not fail closed: state=%+v sink=%+v", state, sink)
	}
}

func TestAdaptiveExactBudgetRepresentativeEvidence(t *testing.T) {
	start := time.Date(2026, 8, 12, 0, 0, 0, 0, time.UTC)
	now := start
	screener := &Screener{
		config: Config{ExactStateBudgetPerMinute: 60, ExactDiscoveryReservePerMinute: 24},
		state:  State{Schema: StateSchema, Counts: map[string]uint64{}, ExactAverageStateRequestsMilli: 5_000},
		now:    func() time.Time { return now },
	}
	screener.initializeExactBudgetLocked(now)
	const eligiblePerHour = 275
	arrivals := make([]time.Time, eligiblePerHour)
	for index := range arrivals {
		arrivals[index] = start.Add(time.Duration(index) * time.Hour / eligiblePerHour)
	}
	queue := make([]time.Time, 0, eligiblePerHour)
	delays := make([]time.Duration, 0, eligiblePerHour)
	compute := make([]time.Duration, 0, eligiblePerHour)
	nextArrival := 0
	maxQueue := 0
	schedulerDeferrals := 0
	for now = start; now.Before(start.Add(time.Hour)) || len(queue) > 0; now = now.Add(100 * time.Millisecond) {
		for nextArrival < len(arrivals) && !arrivals[nextArrival].After(now) {
			queue = append(queue, arrivals[nextArrival])
			nextArrival++
		}
		if len(queue) > maxQueue {
			maxQueue = len(queue)
		}
		if len(queue) == 0 {
			continue
		}
		reserved, admitted := screener.admitExactLocked(now)
		if !admitted {
			schedulerDeferrals++
			continue
		}
		delays = append(delays, now.Sub(queue[0]))
		queue = queue[1:]
		actualCompute := 1580 * time.Millisecond
		if len(compute)%10 == 9 {
			actualCompute = 1880 * time.Millisecond
		}
		compute = append(compute, actualCompute)
		screener.settleExactBudgetLocked(reserved, 5)
	}
	percentile := func(values []time.Duration, numerator int) time.Duration {
		copyValues := append([]time.Duration(nil), values...)
		sort.Slice(copyValues, func(i, j int) bool { return copyValues[i] < copyValues[j] })
		index := (len(copyValues)*numerator + 99) / 100
		if index == 0 {
			index = 1
		}
		return copyValues[index-1]
	}
	evidence := map[string]any{
		"schema":                                        "phoenix.aave-exact-scheduler-evidence.v1",
		"workload_exact_eligible_per_hour":              eligiblePerHour,
		"before_observed_admitted_per_hour":             15,
		"before_observed_eligible_to_exact_p50_minutes": 102,
		"before_observed_eligible_to_exact_p95_minutes": 5.1 * 24 * 60,
		"after_eligible_to_exact_p50_millis":            percentile(delays, 50).Milliseconds(),
		"after_eligible_to_exact_p95_millis":            percentile(delays, 95).Milliseconds(),
		"after_exact_compute_p50_millis":                percentile(compute, 50).Milliseconds(),
		"after_exact_compute_p95_millis":                percentile(compute, 95).Milliseconds(),
		"after_admitted_per_hour":                       len(delays),
		"after_scheduler_capacity_deferrals":            schedulerDeferrals,
		"after_provider_deferrals":                      0,
		"after_primary_exact_state_requests":            len(delays) * 5,
		"after_confirmation_exact_state_requests":       len(delays) * 5,
		"after_rpc_budget_rejected":                     0,
		"after_max_actionable_queue":                    maxQueue,
		"after_actionable_queue_growth":                 len(queue),
	}
	encoded, _ := json.Marshal(evidence)
	t.Log(string(encoded))
	if len(delays) != eligiblePerHour || percentile(delays, 95) >= time.Second || maxQueue > 1 || len(queue) != 0 {
		t.Fatalf("adaptive scheduler did not clear the representative workload: %s", encoded)
	}
	if percentile(compute, 50) != 1580*time.Millisecond || percentile(compute, 95) != 1880*time.Millisecond {
		t.Fatalf("compute evidence drifted: %s", encoded)
	}
}

func TestAdaptiveExactBudgetPersistsAcrossRestartAndChargesObservedWork(t *testing.T) {
	now := time.Date(2026, 8, 12, 0, 0, 0, 0, time.UTC)
	stateDir := t.TempDir()
	screener := &Screener{
		config: Config{
			StateDir:                       stateDir,
			ExactStateBudgetPerMinute:      60,
			ExactDiscoveryReservePerMinute: 24,
		},
		state: State{
			Schema:                         StateSchema,
			Counts:                         map[string]uint64{},
			RouteIneligible:                map[string]string{},
			TailInvalidatedBlock:           map[string]uint64{},
			ExactAverageStateRequestsMilli: 5_000,
			ExactBudgetTokensMilli:         5_000,
			ExactBudgetUpdatedAt:           &now,
		},
		now: func() time.Time { return now },
	}
	reserved, admitted := screener.admitExactLocked(now)
	if !admitted || reserved != 5_000 {
		t.Fatalf("initial budget admission failed: admitted=%t reserved=%d", admitted, reserved)
	}
	screener.settleExactBudgetLocked(reserved, 7)
	if got := screener.state.ExactAverageStateRequestsMilli; got != 5_500 {
		t.Fatalf("observed request cost was not learned: got=%d want=5500", got)
	}
	if err := screener.persistStateLocked(); err != nil {
		t.Fatal(err)
	}

	reloaded := &Screener{
		config: screener.config,
		now:    func() time.Time { return now },
	}
	if err := reloaded.loadState(); err != nil {
		t.Fatal(err)
	}
	if _, admitted := reloaded.admitExactLocked(now); admitted {
		t.Fatal("restart reopened a spent Exact budget")
	}
	now = now.Add(9 * time.Second)
	if _, admitted := reloaded.admitExactLocked(now); admitted {
		t.Fatal("partial refill admitted before the learned request cost was available")
	}
	now = now.Add(time.Second)
	if reserved, admitted := reloaded.admitExactLocked(now); !admitted || reserved != 5_500 {
		t.Fatalf("learned budget did not refill deterministically: admitted=%t reserved=%d", admitted, reserved)
	}
}

func TestCurrentAaveLiveMaximumUsesDurableLaneAuthorityAndFailsClosed(t *testing.T) {
	for _, test := range []struct {
		name    string
		maximum string
		err     error
		want    string
		wantErr bool
	}{
		{name: "current level", maximum: "250000000000000", want: "250000000000000"},
		{name: "disarmed", maximum: "1", want: "1"},
		{name: "above executor ceiling", maximum: "10000000000000001", wantErr: true},
		{name: "authority unavailable", err: errors.New("lane state unavailable"), wantErr: true},
	} {
		t.Run(test.name, func(t *testing.T) {
			sink := &liveAuthoritySignalSink{maximum: test.maximum, err: test.err}
			screener := &Screener{config: Config{
				MaximumInputAmountWei: maximumReviewedInputWei,
				SignalSink:            sink,
			}}
			got, err := screener.currentAaveLiveMaximumInputAmount(context.Background())
			if test.wantErr {
				if err == nil {
					t.Fatalf("maximum=%s", got)
				}
				return
			}
			if err != nil || got != test.want {
				t.Fatalf("maximum=%s err=%v", got, err)
			}
		})
	}
}

func TestValidatedAaveLiveMaximumRequiresExactLaneEconomicAgreement(t *testing.T) {
	active := []liveSizeAuthorityRow{
		{lane: "aave_liquidation", armed: true, maximumInputAmount: "500000000000000", economicPhase: "LIVE_L2", economicInputAmount: "500000000000000"},
		{lane: "atlas_solver", armed: true, maximumInputAmount: "500000000000000", economicPhase: "LIVE_L2", economicInputAmount: "500000000000000"},
	}
	for index := range active {
		active[index].killSwitch = false
	}
	if got, err := validatedAaveLiveMaximumInputAmount(active); err != nil || got != "500000000000000" {
		t.Fatalf("maximum=%s err=%v", got, err)
	}
	closed := []liveSizeAuthorityRow{
		{lane: "aave_liquidation", killSwitch: true, maximumInputAmount: "500000000000000", economicPhase: "DISARMED_DEPLOY", economicInputAmount: "500000000000000"},
		{lane: "atlas_solver", killSwitch: true, maximumInputAmount: "500000000000000", economicPhase: "DISARMED_DEPLOY", economicInputAmount: "500000000000000"},
	}
	if got, err := validatedAaveLiveMaximumInputAmount(closed); err != nil || got != "1" {
		t.Fatalf("closed maximum=%s err=%v", got, err)
	}
	mutations := []func([]liveSizeAuthorityRow){
		func(rows []liveSizeAuthorityRow) { rows[1].maximumInputAmount = "100000000000000" },
		func(rows []liveSizeAuthorityRow) { rows[1].killSwitch = true; rows[1].armed = false },
		func(rows []liveSizeAuthorityRow) { rows[0].economicPhase = "DISARMED_DEPLOY" },
		func(rows []liveSizeAuthorityRow) { rows[0].economicInputAmount = "100000000000000" },
	}
	for index, mutate := range mutations {
		rows := append([]liveSizeAuthorityRow(nil), active...)
		mutate(rows)
		if got, err := validatedAaveLiveMaximumInputAmount(rows); err == nil || !errors.Is(err, errRevenueLaneAuthorityDiverged) {
			t.Fatalf("mutation %d accepted maximum %s", index, got)
		}
	}
}

func TestValidatedAaveLiveMaximumAcceptsExplicitMaximumReviewedLaneAuthority(t *testing.T) {
	active := []liveSizeAuthorityRow{
		{lane: "aave_liquidation", armed: true, maximumInputAmount: maximumReviewedInputWei, economicPhase: "DISARMED_EVIDENCE", economicInputAmount: maximumReviewedInputWei},
		{lane: "atlas_solver", armed: true, maximumInputAmount: maximumReviewedInputWei, economicPhase: "DISARMED_EVIDENCE", economicInputAmount: maximumReviewedInputWei},
	}
	if got, err := validatedAaveLiveMaximumInputAmount(active); err != nil || got != maximumReviewedInputWei {
		t.Fatalf("maximum=%s err=%v", got, err)
	}

	tests := []struct {
		name   string
		mutate func([]liveSizeAuthorityRow) []liveSizeAuthorityRow
	}{
		{name: "partially armed", mutate: func(rows []liveSizeAuthorityRow) []liveSizeAuthorityRow {
			rows[1].armed = false
			rows[1].killSwitch = true
			return rows
		}},
		{name: "one lane kill switched", mutate: func(rows []liveSizeAuthorityRow) []liveSizeAuthorityRow {
			rows[1].killSwitch = true
			return rows
		}},
		{name: "missing lane", mutate: func(rows []liveSizeAuthorityRow) []liveSizeAuthorityRow {
			return rows[:1]
		}},
		{name: "extra lane", mutate: func(rows []liveSizeAuthorityRow) []liveSizeAuthorityRow {
			return append(rows, liveSizeAuthorityRow{lane: "unexpected"})
		}},
		{name: "lane maximum mismatch", mutate: func(rows []liveSizeAuthorityRow) []liveSizeAuthorityRow {
			rows[1].maximumInputAmount = "500000000000000"
			return rows
		}},
		{name: "non maximum reviewed evidence authority", mutate: func(rows []liveSizeAuthorityRow) []liveSizeAuthorityRow {
			for index := range rows {
				rows[index].maximumInputAmount = "500000000000000"
				rows[index].economicInputAmount = "500000000000000"
			}
			return rows
		}},
		{name: "deploy phase armed", mutate: func(rows []liveSizeAuthorityRow) []liveSizeAuthorityRow {
			for index := range rows {
				rows[index].economicPhase = "DISARMED_DEPLOY"
			}
			return rows
		}},
		{name: "failure phase armed", mutate: func(rows []liveSizeAuthorityRow) []liveSizeAuthorityRow {
			for index := range rows {
				rows[index].economicPhase = "DISARMED_FAILURE"
			}
			return rows
		}},
		{name: "economic authority divergence", mutate: func(rows []liveSizeAuthorityRow) []liveSizeAuthorityRow {
			rows[1].economicInputAmount = "500000000000000"
			return rows
		}},
		{name: "above reviewed maximum", mutate: func(rows []liveSizeAuthorityRow) []liveSizeAuthorityRow {
			for index := range rows {
				rows[index].maximumInputAmount = "10000000000000001"
				rows[index].economicInputAmount = "10000000000000001"
			}
			return rows
		}},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			rows := append([]liveSizeAuthorityRow(nil), active...)
			rows = test.mutate(rows)
			if got, err := validatedAaveLiveMaximumInputAmount(rows); err == nil || !errors.Is(err, errRevenueLaneAuthorityDiverged) {
				t.Fatalf("accepted maximum %s", got)
			}
		})
	}
}

func TestCandidateAuthorityBindsDirectAndAtlasSizeToLockedMaximum(t *testing.T) {
	current := "500000000000000"
	valid := signal{
		ExecutionCandidate: &executionCandidate{SelectedSize: current, MaximumInputAmount: current},
		AtlasCandidate:     &atlasCandidate{SelectedSize: current, MaximumInputAmount: current},
	}
	if !candidateMatchesCurrentAuthority(valid, current) {
		t.Fatal("candidate matching the locked authority was rejected")
	}
	for name, mutate := range map[string]func(signal) signal{
		"direct evaluated at old maximum": func(value signal) signal {
			value.ExecutionCandidate.MaximumInputAmount = maximumReviewedInputWei
			return value
		},
		"direct selected above maximum": func(value signal) signal {
			value.ExecutionCandidate.SelectedSize = maximumReviewedInputWei
			return value
		},
		"Atlas evaluated at old maximum": func(value signal) signal {
			value.AtlasCandidate.MaximumInputAmount = maximumReviewedInputWei
			return value
		},
		"Atlas selected above maximum": func(value signal) signal {
			value.AtlasCandidate.SelectedSize = maximumReviewedInputWei
			return value
		},
	} {
		t.Run(name, func(t *testing.T) {
			value := signal{
				ExecutionCandidate: &executionCandidate{SelectedSize: current, MaximumInputAmount: current},
				AtlasCandidate:     &atlasCandidate{SelectedSize: current, MaximumInputAmount: current},
			}
			if candidateMatchesCurrentAuthority(mutate(value), current) {
				t.Fatal("stale candidate authority was accepted")
			}
		})
	}
}

func TestCandidateAuthorityNormalizationRemovesEveryMoneyPathArtifact(t *testing.T) {
	record := withoutCandidateAuthority(signal{
		Authority:          true,
		TerminalOutcome:    "candidate",
		ExecutionCandidate: &executionCandidate{},
		AtlasCandidate:     &atlasCandidate{},
	}, revenueLaneAuthorityDivergedClass)
	if record.Authority || record.ExecutionCandidate != nil || record.AtlasCandidate != nil ||
		record.TerminalOutcome != "exact_pending" ||
		record.ExactDeferredReason != revenueLaneAuthorityDivergedClass {
		t.Fatalf("candidate authority normalization was incomplete: %+v", record)
	}
}

func TestCoherentAuthorityChangesStripStaleCandidateWithoutReportingDivergence(t *testing.T) {
	record := signal{
		Authority:          true,
		TerminalOutcome:    "candidate",
		ExecutionCandidate: &executionCandidate{SelectedSize: maximumReviewedInputWei, MaximumInputAmount: maximumReviewedInputWei},
	}
	promoted, err := normalizeCandidateForCurrentAuthority(record, "500000000000000", nil, true)
	if err != nil || promoted.Authority || promoted.ExecutionCandidate != nil ||
		promoted.ExactDeferredReason != liveSizeAuthorityChangedReason ||
		promoted.ExactDeferredReason == revenueLaneAuthorityDivergedClass {
		t.Fatalf("coherent size change was misclassified: record=%+v err=%v", promoted, err)
	}
	closed, err := normalizeCandidateForCurrentAuthority(record, "1", nil, false)
	if err != nil || closed.Authority || closed.ExecutionCandidate != nil ||
		closed.ExactDeferredReason != revenueAuthorityClosedReason ||
		closed.ExactDeferredReason == revenueLaneAuthorityDivergedClass {
		t.Fatalf("coherent closed authority was misclassified: record=%+v err=%v", closed, err)
	}
	diverged, err := normalizeCandidateForCurrentAuthority(
		record,
		"",
		revenueLaneAuthorityError("partial pair"),
		false,
	)
	if err != nil || diverged.ExactDeferredReason != revenueLaneAuthorityDivergedClass {
		t.Fatalf("true divergence was not preserved: record=%+v err=%v", diverged, err)
	}
}

func TestDivergentRevenueAuthorityDefersExactWithoutStoppingDiscovery(t *testing.T) {
	directory := t.TempDir()
	now := time.Now().UTC()
	borrowerAccount := account{
		Borrower:                    "0x1111111111111111111111111111111111111111",
		TotalCollateralBase:         "2000000000000",
		TotalDebtBase:               "1000000000000",
		AvailableBorrowsBase:        "0",
		CurrentLiquidationThreshold: "8000",
		LoanToValueBPS:              "7500",
		HealthFactorWAD:             "900000000000000000",
	}
	screenCalls := 0
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		if request.URL.Path != "/v1/aave/screen" {
			t.Fatalf("divergent authority emitted Exact work: %s", request.URL.Path)
		}
		screenCalls++
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
				ProviderID: primaryProviderID, WETHPriceBase: "300000000000", Accounts: []account{borrowerAccount},
			},
			Confirmation: nil,
			Quorum:       1,
		})
	}))
	defer server.Close()
	sink := &liveAuthoritySignalSink{err: revenueLaneAuthorityError("Aave off while Atlas remains armed")}
	screener := &Screener{
		config: Config{
			StateDir: directory, GatewayURL: server.URL,
			RetainedProfitFloorWei: "1", MaximumInputAmountWei: maximumReviewedInputWei,
			SignalSink: sink,
		},
		client: server.Client(), state: State{Schema: StateSchema, Counts: map[string]uint64{}},
		debtBearing: make(map[string]bool), refreshKnown: make(map[string]bool),
		hotBorrowers: make(map[string]string), hotDebtBase: make(map[string]string),
		hotUpperPositive: make(map[string]bool), latestOutcome: make(map[string]string),
		lastExactAt: make(map[string]time.Time), firstLiquidatableAt: make(map[string]time.Time),
		now: func() time.Time { return now },
	}
	if err := screener.screen(context.Background(), []string{borrowerAccount.Borrower}, false, nil); err != nil {
		t.Fatalf("divergent authority stopped discovery: %v", err)
	}
	if err := screener.screen(context.Background(), []string{borrowerAccount.Borrower}, false, nil); err != nil {
		t.Fatalf("persisted divergent authority stopped discovery: %v", err)
	}
	if screenCalls != 2 || len(sink.records) != 2 {
		t.Fatalf("discovery did not remain alive: screens=%d records=%d", screenCalls, len(sink.records))
	}
	for _, record := range sink.records {
		if record.Authority || record.ExecutionCandidate != nil || record.AtlasCandidate != nil ||
			record.TerminalOutcome != "exact_pending" || record.ExactDeferredReason != revenueLaneAuthorityDivergedClass {
			t.Fatalf("divergent authority crossed the Candidate boundary: %+v", record)
		}
	}
	state := screener.Snapshot()
	if state.LastErrorClass != revenueLaneAuthorityDivergedClass || state.ExactEvaluationsInFlight != 0 ||
		state.Counts[revenueLaneAuthorityDivergedKey] != 1 || state.Counts[exactEvalCompletedKey] != 0 {
		t.Fatalf("divergent authority degradation was not durable: %+v", state)
	}

	// Control convergence is re-probed on the next fresh dual-provider screen,
	// even when no borrower remains liquidatable and no Exact call is admitted.
	sink.err = nil
	sink.maximum = "1"
	borrowerAccount.TotalDebtBase = "0"
	borrowerAccount.HealthFactorWAD = "2000000000000000000"
	now = now.Add(time.Minute)
	if err := screener.screen(context.Background(), []string{borrowerAccount.Borrower}, false, nil); err != nil {
		t.Fatalf("coherent closed authority did not recover discovery: %v", err)
	}
	state = screener.Snapshot()
	if screenCalls != 3 || len(sink.records) != 3 || state.LastErrorClass != "provider_recovery_requires_exact" ||
		state.ExactEvaluationsInFlight != 0 || state.Counts[exactEvalCompletedKey] != 0 {
		t.Fatalf("coherent authority bypassed the required Exact recovery window: %+v", state)
	}
	recovered := sink.records[2]
	if recovered.Authority || recovered.ExecutionCandidate != nil || recovered.AtlasCandidate != nil {
		t.Fatalf("authority recovery created money-path work: %+v", recovered)
	}
}

func TestCounterfactualPositivePersistsWithinExistingSchemaWithoutAuthority(t *testing.T) {
	record := signal{
		TerminalOutcome:          "counterfactual_positive",
		AuthorityRejectionReason: "live_size_authorization_required",
		ExpectedNetPnLWei:        "200",
		ConservativeNetPnLWei:    "150",
	}
	if got := persistedTerminalOutcome(record); got != "economic_rejection" {
		t.Fatalf("persisted outcome=%s", got)
	}
	if got := signalRejectionReason(record); got != "live_size_authorization_required" {
		t.Fatalf("rejection reason=%v", got)
	}
	if record.ExecutionCandidate != nil || record.AtlasCandidate != nil || record.Authority {
		t.Fatalf("counterfactual record carried authority: %+v", record)
	}
}

func TestProviderRecoveryRequiresThreeAuthorityFreePrimaryExactSamples(t *testing.T) {
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
			Confirmation: nil,
			Quorum:       1,
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
	if len(screener.Snapshot().ProviderRecoverySamples) != 0 {
		t.Fatal("matched discovery was incorrectly counted as Exact recovery evidence")
	}
	for sample := 1; sample <= 3; sample++ {
		now := time.Now().UTC().Add(time.Duration(sample) * time.Second)
		screener.mu.Lock()
		screener.recordProviderRecoveryLocked(now, primaryProviderID)
		screener.state.LastPrimaryExactAt = &now
		screener.mu.Unlock()
		state := screener.Snapshot()
		if len(state.ProviderRecoverySamples) != sample || state.LastPrimaryExactAt == nil || state.ExactQueueCount != 0 {
			t.Fatalf("recovery sample %d was not retained exactly: %+v", sample, state)
		}
		if sample < 3 && state.LastErrorClass == "" {
			t.Fatalf("recovery cleared before three samples: sample=%d state=%+v", sample, state)
		}
	}
	state := screener.Snapshot()
	if state.LastErrorClass != "" {
		t.Fatalf("three fresh primary samples did not recover cleanly: %+v", state)
	}
	if state.Counts[providerRecoverySuccessTotalKey] != 1 || state.Counts[providerLastRecoveryAtMillisKey] == 0 || state.Counts[providerLastDegradedDurationKey] == 0 || state.Counts[providerDegradedSinceMillisKey] != 0 {
		t.Fatalf("recovery evidence is incomplete: %+v", state.Counts)
	}
}

func TestProviderRecoveryExpiredWindowRestartsAtOneSample(t *testing.T) {
	now := time.Date(2026, 8, 12, 0, 0, 0, 0, time.UTC)
	screener := &Screener{state: State{
		Schema:         StateSchema,
		Counts:         map[string]uint64{providerDegradedSinceMillisKey: uint64(now.Add(-10 * time.Minute).UnixMilli())},
		LastErrorClass: "provider_unavailable",
		ProviderRecoverySamples: []ProviderRecoverySample{
			{ObservedAt: now.Add(-4 * time.Minute), PrimaryProvider: primaryProviderID, Quorum: 1},
			{ObservedAt: now.Add(-3 * time.Minute), PrimaryProvider: primaryProviderID, Quorum: 1},
			{ObservedAt: now.Add(-2*time.Minute - time.Second), PrimaryProvider: primaryProviderID, Quorum: 1},
		},
	}}
	screener.recordProviderRecoveryLocked(now, primaryProviderID)
	state := screener.Snapshot()
	if len(state.ProviderRecoverySamples) != 1 || !state.ProviderRecoverySamples[0].ObservedAt.Equal(now) {
		t.Fatalf("expired recovery window did not restart at one sample: %+v", state.ProviderRecoverySamples)
	}
	if state.LastErrorClass == "" || state.Counts[providerDegradedSinceMillisKey] == 0 {
		t.Fatalf("expired recovery window restored authority early: %+v", state)
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
				Primary:      providerScreen{ProviderID: primaryProviderID, WETHPriceBase: "300000000000", Accounts: []account{borrowerAccount}},
				Confirmation: nil,
				Quorum:       1,
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
		config: Config{StateDir: directory, GatewayURL: server.URL, RetainedProfitFloorWei: "1", MaximumInputAmountWei: maximumReviewedInputWei, SignalSink: sink},
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
	if state.LastExactAdmissionAt == nil || state.ExactBudgetUpdatedAt == nil || state.ExactAverageStateRequestsMilli == 0 {
		t.Fatalf("failed Exact did not durably consume its adaptive admission: %+v", state)
	}
	reloaded := &Screener{config: Config{StateDir: directory}}
	if err := reloaded.loadState(); err != nil {
		t.Fatal(err)
	}
	if reloaded.lastExactAdmissionAt.IsZero() || reloaded.state.ExactBudgetUpdatedAt == nil || reloaded.state.ExactAverageStateRequestsMilli == 0 {
		t.Fatalf("restart lost the failed Exact budget evidence: %+v", reloaded.state)
	}
	if reloaded.state.Counts[exactEvalStartedKey] != 1 {
		t.Fatalf("restart lost the durable Exact start count: %+v", reloaded.state.Counts)
	}
}

func TestDegradedAtlasAuctionDoesNotStartCompetingRecovery(t *testing.T) {
	requests := 0
	server := httptest.NewServer(http.HandlerFunc(func(http.ResponseWriter, *http.Request) {
		requests++
	}))
	defer server.Close()
	sink := &recordingSignalSink{}
	screener := &Screener{
		config:       Config{GatewayURL: server.URL, SignalSink: sink},
		client:       server.Client(),
		state:        State{Schema: StateSchema, Counts: map[string]uint64{}, LastErrorClass: "provider_unavailable"},
		hotBorrowers: map[string]string{"0x1111111111111111111111111111111111111111": "900000000000000000"},
	}
	evidenceHash := strings.Repeat("a", 64)
	if err := screener.HandleAtlasAuction(context.Background(), &observer.LedgerRecord{AuctionID: "auction-1", ChainID: 42161, RelevantAaveAuction: true, NotificationSHA256: evidenceHash}); err != nil {
		t.Fatal(err)
	}
	if requests != 0 || screener.Snapshot().ExactQueueCount != 0 || len(sink.records) != 0 || len(sink.atlasCallbackUnavailableIDs) != 1 || sink.atlasCallbackUnavailableIDs[0] != "auction-1" || sink.atlasCallbackEvidenceHashes[0] != evidenceHash {
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
			SchemaVersion: "phoenix.rpc.aave-tail-response.v2",
			ChainID:       42161, RequestID: input.RequestID,
			FinalizedBlockNumber: 491218648,
			FinalizedBlockHash:   "0x" + strings.Repeat("a", 64),
			PrimaryProviderID:    "production-nownodes-arbitrum",
			ConfirmationProvider: nil,
			Quorum:               1,
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

func TestLiquidatablePriorityUsesHealthFactorWithoutEconomicDebtOrdering(t *testing.T) {
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
	if len(batch) != 3 || batch[0] != small || batch[1] != large || batch[2] != urgent {
		t.Fatalf("unexpected hot borrower priority: %v", batch)
	}

	accounts := []account{
		{Borrower: small, TotalDebtBase: "100", HealthFactorWAD: "800000000000000000"},
		{Borrower: urgent, TotalDebtBase: "100000000", HealthFactorWAD: "1001000000000000000"},
		{Borrower: large, TotalDebtBase: "100000", HealthFactorWAD: "990000000000000000"},
	}
	order := prioritizedAccountOrder(accounts)
	if len(order) != 3 || order[0] != 0 || order[1] != 2 || order[2] != 1 {
		t.Fatalf("unexpected screen account priority: %v", order)
	}
}

func TestExactSchedulerPrefersNeverServedThenOldestEligibleAcrossCooldown(t *testing.T) {
	now := time.Date(2026, 8, 11, 0, 0, 0, 0, time.UTC)
	highDebt := "0x1111111111111111111111111111111111111111"
	lowDebt := "0x2222222222222222222222222222222222222222"
	newBorrower := "0x3333333333333333333333333333333333333333"
	screener := &Screener{
		hotBorrowers: map[string]string{
			highDebt: "900000000000000000",
			lowDebt:  "990000000000000000",
		},
		hotDebtBase: map[string]string{highDebt: "1000000", lowDebt: "1"},
		hotUpperPositive: map[string]bool{
			highDebt: true,
			lowDebt:  true,
		},
		lastExactAt: map[string]time.Time{highDebt: now.Add(-3 * time.Minute)},
		firstLiquidatableAt: map[string]time.Time{
			lowDebt: now.Add(-time.Minute),
		},
		state: State{RouteIneligible: map[string]string{}},
		now:   func() time.Time { return now },
	}
	if batch := screener.nextHotBatch(); len(batch) < 2 || batch[0] != lowDebt {
		t.Fatalf("never-served eligible borrower was starved by debt priority: %v", batch)
	}
	accounts := []account{
		{Borrower: highDebt, TotalDebtBase: "1000000", HealthFactorWAD: "900000000000000000"},
		{Borrower: lowDebt, TotalDebtBase: "1", HealthFactorWAD: "990000000000000000"},
	}
	if order := screener.schedulerAccountOrder(accounts); len(order) != 2 || order[0] != 1 {
		t.Fatalf("screen execution reordered the fair scheduler selection: %v", order)
	}
	newArrival := []account{
		{Borrower: newBorrower, TotalDebtBase: "1000000000", HealthFactorWAD: "900000000000000000"},
		{Borrower: lowDebt, TotalDebtBase: "1", HealthFactorWAD: "990000000000000000"},
	}
	if order := screener.schedulerAccountOrder(newArrival); len(order) != 2 || order[0] != 1 {
		t.Fatalf("new high-debt arrival leapfrogged an existing waiter: %v", order)
	}

	screener.lastExactAt[lowDebt] = now
	screener.hotBorrowers[newBorrower] = "995000000000000000"
	screener.hotDebtBase[newBorrower] = "1"
	screener.hotUpperPositive[newBorrower] = true
	screener.firstLiquidatableAt[newBorrower] = now
	now = now.Add(3 * time.Minute)
	batch := screener.nextHotBatch()
	if len(batch) != 3 || batch[0] != newBorrower || batch[1] != highDebt || batch[2] != lowDebt {
		t.Fatalf("never-served/oldest-eligible order drifted after cooldown: %v", batch)
	}
}

func TestExactSchedulerUsesTheCurrentLiquidationEpochAfterUrgentInterlude(t *testing.T) {
	now := time.Date(2026, 8, 11, 0, 10, 0, 0, time.UTC)
	reentered := "0x1111111111111111111111111111111111111111"
	continuouslyLiquidatable := "0x2222222222222222222222222222222222222222"
	screener := &Screener{
		hotBorrowers: map[string]string{
			reentered:                "900000000000000000",
			continuouslyLiquidatable: "990000000000000000",
		},
		hotDebtBase: map[string]string{
			reentered:                "1000000",
			continuouslyLiquidatable: "1",
		},
		hotUpperPositive: map[string]bool{
			reentered:                true,
			continuouslyLiquidatable: true,
		},
		lastExactAt: map[string]time.Time{
			reentered:                now.Add(-10 * time.Minute),
			continuouslyLiquidatable: now.Add(-3 * time.Minute),
		},
		firstLiquidatableAt: map[string]time.Time{
			reentered: now.Add(-30 * time.Second),
		},
		state: State{RouteIneligible: map[string]string{}},
		now:   func() time.Time { return now },
	}
	if batch := screener.nextHotBatch(); len(batch) != 2 || batch[0] != continuouslyLiquidatable {
		t.Fatalf("old cooldown epoch outranked the current liquidation epoch: %v", batch)
	}
	accounts := []account{
		{Borrower: reentered, TotalDebtBase: "1000000", HealthFactorWAD: "900000000000000000"},
		{Borrower: continuouslyLiquidatable, TotalDebtBase: "1", HealthFactorWAD: "990000000000000000"},
	}
	if order := screener.schedulerAccountOrder(accounts); len(order) != 2 || order[0] != 1 {
		t.Fatalf("screen reordered the current liquidation epoch: %v", order)
	}
	snapshot := screener.Snapshot()
	if snapshot.ExactEligibleNowCount != 2 || snapshot.OldestExactEligibleAgeMillis != uint64(time.Minute/time.Millisecond) {
		t.Fatalf("eligible-age gauge retained a stale liquidation epoch: %+v", snapshot)
	}
}

func TestDeferredScreenPreservesLastCompletedForkOutcome(t *testing.T) {
	borrower := "0x1111111111111111111111111111111111111111"
	screener := &Screener{
		config: Config{RetainedProfitFloorWei: "1"},
		state:  State{Counts: map[string]uint64{}, RouteIneligible: map[string]string{}},
	}
	screener.applyHotSignal(signal{
		ObservedAt: time.Date(2026, 8, 11, 0, 0, 0, 0, time.UTC),
		Borrower:   borrower, Bucket: "liquidatable", DebtBase: "1",
		HF: "900000000000000000", ZeroCostProfitUpperBoundWei: "2",
		TerminalOutcome: "fork_pending", StateRoot: "0x" + strings.Repeat("a", 64),
	})
	screener.applyHotSignal(signal{
		ObservedAt: time.Date(2026, 8, 11, 0, 0, 10, 0, time.UTC),
		Borrower:   borrower, Bucket: "liquidatable", DebtBase: "1",
		HF: "900000000000000000", ZeroCostProfitUpperBoundWei: "2",
		TerminalOutcome: "exact_pending", ExactDeferredReason: "borrower_cooldown",
	})
	if got := screener.latestOutcome[borrower]; got != "fork_pending" {
		t.Fatalf("cooldown signal erased unresolved fork outcome: %s", got)
	}
}

func TestHotReplayResetsLiquidationEpochAtEachExactCompletion(t *testing.T) {
	borrower := "0x1111111111111111111111111111111111111111"
	start := time.Date(2026, 8, 11, 0, 0, 0, 0, time.UTC)
	screener := &Screener{
		config: Config{RetainedProfitFloorWei: "1"},
		state:  State{Counts: map[string]uint64{}, RouteIneligible: map[string]string{}},
	}
	exact := func(observedAt time.Time, rootByte string) {
		completedAt := observedAt.Add(30 * time.Second)
		screener.applyHotSignal(signal{
			ObservedAt: observedAt, ExactCompletedAt: &completedAt,
			Borrower: borrower, Bucket: "liquidatable", DebtBase: "1",
			HF: "900000000000000000", ZeroCostProfitUpperBoundWei: "2",
			TerminalOutcome: "fork_pending", StateRoot: "0x" + strings.Repeat(rootByte, 64),
		})
		if _, present := screener.firstLiquidatableAt[borrower]; present {
			t.Fatalf("completed Exact retained a prior liquidation epoch: %+v", screener.firstLiquidatableAt)
		}
		if got := screener.lastExactAt[borrower]; !got.Equal(completedAt) {
			t.Fatalf("completion-based cooldown replayed from %s instead of %s", got, completedAt)
		}
	}
	deferred := func(observedAt time.Time) {
		screener.applyHotSignal(signal{
			ObservedAt: observedAt,
			Borrower:   borrower, Bucket: "liquidatable", DebtBase: "1",
			HF: "900000000000000000", ZeroCostProfitUpperBoundWei: "2",
			TerminalOutcome: "exact_pending", ExactDeferredReason: "borrower_cooldown",
		})
	}

	exact(start, "a")
	deferred(start.Add(40 * time.Second))
	if got := screener.firstLiquidatableAt[borrower]; !got.Equal(start.Add(40 * time.Second)) {
		t.Fatalf("first replay epoch=%s", got)
	}
	exact(start.Add(2*time.Minute), "b")
	deferred(start.Add(2*time.Minute + 40*time.Second))
	if got := screener.firstLiquidatableAt[borrower]; !got.Equal(start.Add(2*time.Minute + 40*time.Second)) {
		t.Fatalf("second replay epoch inherited historical latency: %s", got)
	}
}

func TestCompactExactSummaryUsesClosestMarginAndDoesNotDoubleCountAuthorityReason(t *testing.T) {
	record := signal{
		AuthorityRejectionReason: "live_size_authorization_required",
		SizeDiagnostics: []sizeDiagnostic{
			{ReviewedSize: "1", Route: "WETH_IDENTITY", GasLimit: 1, MarginToRetainedFloorWei: "100"},
			{ReviewedSize: "2", Route: "WETH_IDENTITY", GasLimit: 1, MarginToRetainedFloorWei: "-2"},
			{ReviewedSize: "3", Route: "WETH_IDENTITY", GasLimit: 1, MarginToRetainedFloorWei: "5", FinalRejectionReason: "live_size_authorization_required", Selected: true},
		},
	}
	summary := buildExactDiagnosticSummary(record, time.Second, time.Second)
	if summary.ClosestMarginToRetainedFloorWei != "-2" {
		t.Fatalf("closest margin=%s summary=%+v", summary.ClosestMarginToRetainedFloorWei, summary)
	}
	if summary.RejectionCounts["live_size_authorization_required"] != 1 {
		t.Fatalf("record-level authority reason was double counted: %+v", summary.RejectionCounts)
	}
	if len(summary.TopDiagnostics) != 3 || summary.SelectedDiagnostic == nil {
		t.Fatalf("bounded compact diagnostics are incomplete: %+v", summary)
	}
}

func TestCompactExactDiagnosticsRoundTripIsBounded(t *testing.T) {
	diagnostics := make([]sizeDiagnostic, 0, 5)
	for index, margin := range []string{"50", "40", "30", "20", "10"} {
		diagnostics = append(diagnostics, sizeDiagnostic{
			ReviewedSize: strconv.Itoa(index + 1), Route: "WETH_IDENTITY", GasLimit: 1,
			MarginToRetainedFloorWei: margin, LiveAuthorized: true,
		})
	}
	record := signal{
		Schema: "phoenix.atlas-aave-hunting-signal.v1", ObservedAt: time.Now().UTC(),
		Borrower: "0x1111111111111111111111111111111111111111", TerminalOutcome: "economic_rejection",
		SizeDiagnostics: diagnostics,
	}
	record.ExactDiagnostics = buildExactDiagnosticSummary(record, time.Second, 2*time.Second)
	encoded, err := json.Marshal(record)
	if err != nil {
		t.Fatal(err)
	}
	if len(encoded) >= 64*1024 || len(record.ExactDiagnostics.TopDiagnostics) != 3 {
		t.Fatalf("compact diagnostics exceeded their bound: bytes=%d summary=%+v", len(encoded), record.ExactDiagnostics)
	}
	var decoded signal
	if err := json.Unmarshal(encoded, &decoded); err != nil {
		t.Fatal(err)
	}
	if decoded.ExactDiagnostics == nil || decoded.ExactDiagnostics.Schema != "phoenix.aave-exact-diagnostics.v1" || len(decoded.ExactDiagnostics.TopDiagnostics) != 3 || decoded.ExactDiagnostics.ReviewedCombinationCount != 5 {
		t.Fatalf("compact diagnostics did not round trip: %+v", decoded.ExactDiagnostics)
	}
}

func TestCandidateArtifactsRequireAnAcceptedDurableSignalWrite(t *testing.T) {
	for name, record := range map[string]signal{
		"direct": {ExecutionCandidate: &executionCandidate{}},
		"atlas":  {AtlasCandidate: &atlasCandidate{}},
	} {
		t.Run(name, func(t *testing.T) {
			if err := requireAcceptedCandidateSignalWrite(0, record); err == nil {
				t.Fatal("conflicting terminal evidence permitted a candidate artifact")
			}
			if err := requireAcceptedCandidateSignalWrite(1, record); err != nil {
				t.Fatalf("accepted signal write was rejected: %v", err)
			}
		})
	}
	if err := requireAcceptedCandidateSignalWrite(0, signal{}); err != nil {
		t.Fatalf("a non-authoritative deferred replay must remain a harmless no-op: %v", err)
	}
}

func TestEmptyTailPriorityWindowPollsTailAndHotOnEveryTick(t *testing.T) {
	borrower := "0x1111111111111111111111111111111111111111"
	sequence := make([]string, 0, 6)
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/tail":
			sequence = append(sequence, "tail")
			var input tailRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			_ = json.NewEncoder(writer).Encode(tailResponse{
				SchemaVersion: "phoenix.rpc.aave-tail-response.v2", ChainID: 42161, RequestID: input.RequestID,
				FinalizedBlockNumber: input.FromBlock, FinalizedBlockHash: "0x" + strings.Repeat("a", 64),
				PrimaryProviderID: primaryProviderID, ConfirmationProvider: nil, Quorum: 1,
				FromBlock: input.FromBlock, ToBlock: input.FromBlock, NextBlock: input.FromBlock + 1,
			})
		case "/v1/aave/screen":
			sequence = append(sequence, "hot")
			var input screenRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			accounts := []account{{Borrower: borrower, TotalDebtBase: "1000", HealthFactorWAD: "1050000000000000000"}}
			_ = json.NewEncoder(writer).Encode(screenResponse{
				SchemaVersion: ResponseSchema, ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 100, BlockHash: "0x" + strings.Repeat("b", 64),
				Primary:      providerScreen{ProviderID: primaryProviderID, WETHPriceBase: "100000000", Accounts: accounts},
				Confirmation: nil,
				Quorum:       1,
			})
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()
	waits := make([]time.Duration, 0, 6)
	screener := &Screener{
		config:       Config{GatewayURL: server.URL, StateDir: t.TempDir(), RetainedProfitFloorWei: "1"},
		client:       server.Client(),
		state:        State{Schema: StateSchema, TailNextBlock: 100, Counts: map[string]uint64{}, RouteIneligible: map[string]string{}},
		hotBorrowers: map[string]string{borrower: "1050000000000000000"},
		hotDebtBase:  map[string]string{borrower: "1000"},
		debtBearing:  map[string]bool{borrower: true}, refreshKnown: map[string]bool{},
		wait: func(_ context.Context, delay time.Duration) bool {
			waits = append(waits, delay)
			return true
		},
	}
	if err := screener.runPriorityRecheckWindow(context.Background(), time.Minute); err != nil {
		t.Fatal(err)
	}
	want := []string{"tail", "tail", "hot", "tail", "hot", "tail", "hot", "tail", "hot", "tail", "hot"}
	if strings.Join(sequence, ",") != strings.Join(want, ",") || len(waits) != 6 {
		t.Fatalf("priority cadence drifted: sequence=%v waits=%v", sequence, waits)
	}
	if screener.Snapshot().Counts[hotRecheckTotalKey] != 5 {
		t.Fatalf("hot rechecks were not counted: %+v", screener.Snapshot().Counts)
	}
}

func TestTailBorrowerPreemptsTheOrdinaryHotBatch(t *testing.T) {
	tailBorrower := "0x1111111111111111111111111111111111111111"
	hotBorrower := "0x2222222222222222222222222222222222222222"
	screened := ""
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/tail":
			var input tailRequest
			_ = json.NewDecoder(request.Body).Decode(&input)
			_ = json.NewEncoder(writer).Encode(tailResponse{
				SchemaVersion: "phoenix.rpc.aave-tail-response.v2", ChainID: 42161, RequestID: input.RequestID,
				FinalizedBlockNumber: 100, FinalizedBlockHash: "0x" + strings.Repeat("a", 64),
				PrimaryProviderID: primaryProviderID, ConfirmationProvider: nil, Quorum: 1,
				FromBlock: 100, ToBlock: 100, NextBlock: 101, Borrowers: []string{tailBorrower},
			})
		case "/v1/aave/screen":
			var input screenRequest
			_ = json.NewDecoder(request.Body).Decode(&input)
			screened = input.Borrowers[0]
			accounts := []account{{Borrower: screened, TotalDebtBase: "0", HealthFactorWAD: "1100000000000000000"}}
			_ = json.NewEncoder(writer).Encode(screenResponse{
				SchemaVersion: ResponseSchema, ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 100, BlockHash: "0x" + strings.Repeat("b", 64),
				Primary:      providerScreen{ProviderID: primaryProviderID, WETHPriceBase: "100000000", Accounts: accounts},
				Confirmation: nil,
				Quorum:       1,
			})
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()
	screener := &Screener{
		config: Config{GatewayURL: server.URL, StateDir: t.TempDir()}, client: server.Client(),
		state:        State{Schema: StateSchema, TailNextBlock: 100, Counts: map[string]uint64{}, RouteIneligible: map[string]string{}},
		hotBorrowers: map[string]string{hotBorrower: "900000000000000000"},
		hotDebtBase:  map[string]string{hotBorrower: "1000"}, debtBearing: map[string]bool{}, refreshKnown: map[string]bool{},
	}
	worked, err := screener.runTailPriority(context.Background())
	if err != nil || !worked || screened != tailBorrower {
		t.Fatalf("tail did not preempt hot work: worked=%t screened=%s err=%v", worked, screened, err)
	}
}

func TestHotBudgetDeferralIsFailClosedAndObservable(t *testing.T) {
	borrower := "0x1111111111111111111111111111111111111111"
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		writer.WriteHeader(http.StatusTooManyRequests)
		_, _ = writer.Write([]byte(`{"error_class":"state_request_budget_exhausted","retryable":true}`))
	}))
	defer server.Close()
	screener := &Screener{
		config: Config{GatewayURL: server.URL}, client: server.Client(),
		state:        State{Schema: StateSchema, Counts: map[string]uint64{}},
		hotBorrowers: map[string]string{borrower: "900000000000000000"},
		hotDebtBase:  map[string]string{borrower: "1000"},
	}
	worked, err := screener.runHotPriority(context.Background())
	if err == nil || worked || screener.Snapshot().Counts[hotRecheckDeferredBudgetKey] != 1 {
		t.Fatalf("hot budget rejection did not defer fail-closed: worked=%t err=%v state=%+v", worked, err, screener.Snapshot())
	}
}

func TestAaveMetricsExposeHotAndExactLatencyEvidence(t *testing.T) {
	now := time.Date(2026, 8, 11, 0, 0, 20, 0, time.UTC)
	liquidatable := "0x1111111111111111111111111111111111111111"
	urgent := "0x2222222222222222222222222222222222222222"
	screener := &Screener{
		state: State{Schema: StateSchema, Counts: map[string]uint64{
			hotRecheckTotalKey: 3, exactEvalStartedKey: 2, exactEvalCompletedKey: 2,
			exactCoalescedKey: 4, exactStaleInvalidatedKey: 5, exactDuplicateSuppressedKey: 6,
		}, ExactQueueCount: 42, RouteIneligible: map[string]string{urgent: "no_weth_debt"}},
		hotBorrowers: map[string]string{
			liquidatable: "900000000000000000",
			urgent:       "1010000000000000000",
		},
		hotDebtBase: map[string]string{
			liquidatable: "1000",
			urgent:       "2000",
		},
		hotUpperPositive:    map[string]bool{liquidatable: true},
		latestOutcome:       map[string]string{liquidatable: "fork_pending"},
		firstLiquidatableAt: map[string]time.Time{liquidatable: now.Add(-20 * time.Second)},
		exactInFlight:       1,
		now:                 func() time.Time { return now },
	}
	screener.mu.Lock()
	screener.observeDurationLocked(exactEvalLatencySumKey, exactEvalLatencyCountKey, "exact_eval_latency_millis_bucket_le_", 4*time.Second)
	screener.observeDurationLocked(liquidatableToExactSumKey, liquidatableToExactCountKey, "liquidatable_to_exact_millis_bucket_le_", 12*time.Second)
	screener.observeDurationLocked(signalPrefilterLatencySumKey, signalPrefilterLatencyCountKey, "signal_prefilter_latency_millis_bucket_le_", 10*time.Millisecond)
	screener.observeDurationLocked(exactEligibilityLatencySumKey, exactEligibilityLatencyCountKey, "exact_eligibility_latency_millis_bucket_le_", 5*time.Millisecond)
	screener.observeDurationLocked(exactQueueLatencySumKey, exactQueueLatencyCountKey, "exact_queue_latency_millis_bucket_le_", 25*time.Millisecond)
	screener.observeDurationLocked(exactDispatchLatencySumKey, exactDispatchLatencyCountKey, "exact_dispatch_latency_millis_bucket_le_", 10*time.Millisecond)
	screener.observeDurationLocked(exactFirstRPCLatencySumKey, exactFirstRPCLatencyCountKey, "exact_first_rpc_latency_millis_bucket_le_", 50*time.Millisecond)
	screener.observeDurationLocked(exactInitialLatencySumKey, exactInitialLatencyCountKey, "exact_initial_response_latency_millis_bucket_le_", 250*time.Millisecond)
	screener.observeDurationLocked(exactComputeLatencySumKey, exactComputeLatencyCountKey, "exact_compute_latency_millis_bucket_le_", 25*time.Millisecond)
	screener.observeDurationLocked(exactForkQueueSumKey, exactForkQueueCountKey, "exact_fork_queue_millis_bucket_le_", 10*time.Millisecond)
	screener.observeDurationLocked(exactForkRuntimeSumKey, exactForkRuntimeCountKey, "exact_fork_runtime_millis_bucket_le_", time.Second)
	screener.mu.Unlock()
	metrics := screener.MetricsText()
	for _, expected := range []string{
		"phoenix_aave_hot_queue_size 2",
		"phoenix_aave_liquidatable_hot_count 1",
		"phoenix_aave_urgent_hot_count 1",
		"phoenix_aave_exact_eligible_now 1",
		"phoenix_aave_exact_scheduler_blocked 0",
		"phoenix_aave_exact_cooldown_blocked 0",
		"phoenix_aave_route_ineligible_current 1",
		"phoenix_aave_exact_provider_blocked 0",
		"phoenix_aave_exact_evaluations_in_flight 1",
		"phoenix_aave_oldest_exact_eligible_age_ms 20000",
		"phoenix_aave_active_fork_pending 1",
		"phoenix_aave_exact_queue_ledger_entries_total 42",
		"phoenix_aave_hot_recheck_total 3",
		"phoenix_aave_exact_eval_latency_ms_bucket{le=\"5000\"} 1",
		"phoenix_aave_first_liquidatable_to_exact_eval_ms_bucket{le=\"15000\"} 1",
		"phoenix_exact_worker_permits_available 12",
		"phoenix_exact_queue_depth 1",
		"phoenix_exact_oldest_actionable_age_seconds 20",
		"phoenix_exact_coalesced_total 4",
		"phoenix_exact_stale_invalidated_total 5",
		"phoenix_exact_duplicate_suppressed_total 6",
		"phoenix_signal_to_prefilter_seconds_count 1",
		"phoenix_liquidatable_to_exact_enqueue_seconds_count 1",
		"phoenix_exact_queue_wait_seconds_count 1",
		"phoenix_exact_worker_dispatch_seconds_count 1",
		"phoenix_exact_first_rpc_dispatch_seconds_count 1",
		"phoenix_exact_rpc_state_fetch_seconds_count 1",
		"phoenix_exact_compute_seconds_count 1",
		"phoenix_exact_end_to_end_seconds_count 1",
		"phoenix_fork_queue_wait_seconds_count 1",
		"phoenix_fork_runtime_seconds_count 1",
	} {
		if !strings.Contains(metrics, expected) {
			t.Fatalf("metric %q is missing:\n%s", expected, metrics)
		}
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
				Confirmation: nil,
				Quorum:       1,
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

func TestGlobalExactAdmissionDefersAnotherBorrowerButStillRefreshesHF(t *testing.T) {
	now := time.Date(2026, 8, 8, 8, 0, 30, 0, time.UTC)
	borrower := "0x2222222222222222222222222222222222222222"
	exactRequests := 0
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/screen":
			var input screenRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			borrowerAccount := account{
				Borrower: borrower, TotalDebtBase: "1000000000000",
				HealthFactorWAD: "900000000000000000",
			}
			_ = json.NewEncoder(writer).Encode(screenResponse{
				SchemaVersion: ResponseSchema, ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 491300001, BlockHash: "0x" + strings.Repeat("a", 64),
				Primary:      providerScreen{ProviderID: primaryProviderID, WETHPriceBase: "300000000000", Accounts: []account{borrowerAccount}},
				Confirmation: nil,
				Quorum:       1,
			})
		case "/v1/aave/exact":
			exactRequests++
			t.Fatal("global Exact admission was bypassed")
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()
	sink := &recordingSignalSink{}
	screener := &Screener{
		config: Config{StateDir: t.TempDir(), GatewayURL: server.URL, RetainedProfitFloorWei: "1", SignalSink: sink},
		client: server.Client(), state: State{Schema: StateSchema, Counts: map[string]uint64{},
			ExactBudgetTokensMilli: 0, ExactBudgetUpdatedAt: &now, ExactAverageStateRequestsMilli: defaultExactRequestEstimateMilli},
		debtBearing: make(map[string]bool), refreshKnown: make(map[string]bool),
		hotBorrowers: make(map[string]string), hotDebtBase: make(map[string]string),
		lastExactAt: make(map[string]time.Time), firstLiquidatableAt: make(map[string]time.Time),
		lastExactAdmissionAt: now.Add(-5 * time.Second),
		now:                  func() time.Time { return now },
	}
	if err := screener.screen(context.Background(), []string{borrower}, false, nil); err != nil {
		t.Fatal(err)
	}
	if exactRequests != 0 || len(sink.records) != 1 || sink.records[0].ExactDeferredReason != "scheduler_capacity" {
		t.Fatalf("global Exact admission was not fail-closed: calls=%d records=%+v", exactRequests, sink.records)
	}
	if screener.hotBorrowers[borrower] != "900000000000000000" || screener.Snapshot().Counts[exactDeferredSchedulerKey] != 1 || screener.Snapshot().SchedulerBlockedCount != 1 {
		t.Fatalf("HF refresh or scheduler evidence was lost: %+v", screener.Snapshot())
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
		state.LastErrorClass != "gateway_budget_exhausted" {
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

func TestLiquidationVariantBoundsUseReviewedMaximumNotLiveMaximum(t *testing.T) {
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

	aboveLive := append([]exactLiquidation(nil), variants...)
	aboveLive[3] = boundedTestLiquidation(wethAddress, 101, 5)
	if err := screener.validateLiquidationVariants(aboveLive, 5); err != nil {
		t.Fatalf("reviewed counterfactual size was incorrectly bounded by the live cap: %v", err)
	}

	oversized := append([]exactLiquidation(nil), variants...)
	oversized[3] = boundedTestLiquidation(wethAddress, 10_000_000_000_000_001, 5)
	if err := screener.validateLiquidationVariants(oversized, 5); err == nil {
		t.Fatal("exact repay crossed the reviewed maximum input")
	}

	mismatchedActual := append([]exactLiquidation(nil), variants...)
	mismatchedActual[0].ActualRepayAmount = "24"
	if err := screener.validateLiquidationVariants(mismatchedActual, 5); err == nil {
		t.Fatal("requested and actual repay mismatch was accepted")
	}

	eighthSize := append([]exactLiquidation(nil), variants...)
	for amount := int64(101); amount <= 104; amount++ {
		eighthSize = append(eighthSize, boundedTestLiquidation(wethAddress, amount, 5))
	}
	if err := screener.validateLiquidationVariants(eighthSize, 5); err == nil {
		t.Fatal("an eighth size crossed the reviewed per-collateral grid bound")
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

func TestLiquidationWinnerOrderingUsesSmallestPositiveThenConservativeEconomics(t *testing.T) {
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
	if !betterLiquidationEvaluation(small, maximum) {
		t.Fatal("smallest positive reviewed size did not win")
	}
	if !betterLiquidationEvaluation(evaluation("25", 101, 102), small) {
		t.Fatal("higher conservative PnL did not break a same-size tie")
	}
	if !betterLiquidationEvaluation(evaluation("25", 100, 120), small) {
		t.Fatal("higher post-cost expected PnL did not break a same-size conservative tie")
	}
}

func TestUniswapDirectionIsDerivedFromActualTokenOrdering(t *testing.T) {
	if zeroForOne, ok := uniswapZeroForOne(nativeUSDCAddress, nativeUSDCAddress, wethAddress); !ok || !zeroForOne {
		t.Fatalf("token0 input direction was not zero-for-one: value=%t ok=%t", zeroForOne, ok)
	}
	if zeroForOne, ok := uniswapZeroForOne(nativeUSDCAddress, wethAddress, nativeUSDCAddress); !ok || zeroForOne {
		t.Fatalf("token1 input direction was not one-for-zero: value=%t ok=%t", zeroForOne, ok)
	}
	if _, ok := uniswapZeroForOne(nativeUSDCAddress, wethAddress, "0x1111111111111111111111111111111111111111"); ok {
		t.Fatal("unrelated token pair produced a swap direction")
	}
}

func TestCounterfactualPositiveSizeIsVisibleButCannotMaterializeAuthority(t *testing.T) {
	minimum := boundedTestLiquidation(wethAddress, 1_000, 5)
	minimum.LiquidatorCollateral = "1050"
	minimum.OracleUnwindOutputWETH = "1050"
	larger := boundedTestLiquidation(wethAddress, 5_000, 5)
	larger.LiquidatorCollateral = "5403"
	larger.OracleUnwindOutputWETH = "5403"
	liquidations := []exactLiquidation{minimum, larger}
	exactCalls := 0
	simulationCalls := 0
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/exact":
			exactCalls++
			var input exactRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			primary := exactProvider{ProviderID: primaryProviderID, FlashPremiumBPS: 5, Liquidations: liquidations}
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: uint64(99 + exactCalls), BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
				Primary: primary, Confirmation: nil, Quorum: 1,
			})
		case "/v1/aave/simulate-batch":
			var input simulationBatchRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			results := make([]simulationBatchResult, 0, len(input.Simulations))
			for _, simulation := range input.Simulations {
				simulationCalls++
				if !simulation.Counterfactual || simulation.MaximumInputAmount != maximumReviewedInputWei || simulation.LiveMaximumInputAmount != "4000" {
					t.Fatalf("counterfactual authority was not separated from live authority: %+v", simulation)
				}
				results = append(results, simulationBatchResult{
					RequestID: simulation.RequestID,
					Response:  testSimulationResponse(simulation, "400", "10", "3", larger.FlashPremiumAmount),
				})
			}
			writeTestSimulationBatch(writer, input, results)
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()

	record, err := economicTestScreener(server).resolveExact(context.Background(), signal{
		Cursor: 1, Borrower: "0x1111111111111111111111111111111111111111",
	}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if exactCalls != 2 || simulationCalls != 3 || record.Authority || record.ExecutionCandidate != nil || record.AtlasCandidate != nil || record.TerminalOutcome != "counterfactual_positive" || record.AuthorityRejectionReason != "live_size_authorization_required" {
		t.Fatalf("counterfactual positive crossed live authority: exact=%d simulations=%d record=%+v", exactCalls, simulationCalls, record)
	}
	if record.ExpectedNetPnLWei != "390" || record.ConservativeNetPnLWei != "351" || len(record.SizeDiagnostics) != 2 {
		t.Fatalf("counterfactual economics diagnostics are incomplete: %+v", record)
	}
	for _, diagnostic := range record.SizeDiagnostics {
		switch diagnostic.ReviewedSize {
		case "1000":
			if diagnostic.FinalRejectionReason != "gross_edge_below_retained_profit_gate" || !diagnostic.LiveAuthorized {
				t.Fatalf("minimum-size diagnostic drifted: %+v", diagnostic)
			}
		case "5000":
			if diagnostic.LiveAuthorized || diagnostic.FinalRejectionReason != "live_size_authorization_required" || diagnostic.ExecutionCostWei != "10" || diagnostic.MarginToRetainedFloorWei != "251" {
				t.Fatalf("larger-size diagnostic drifted: %+v", diagnostic)
			}
		default:
			t.Fatalf("unexpected reviewed size diagnostic: %+v", diagnostic)
		}
	}
}

func TestReviewedFeeRoutesAreComparedAtOnePinAndSelectBestPostCostOutcome(t *testing.T) {
	liquidation := boundedTestLiquidation(nativeUSDCAddress, 1_000, 5)
	liquidation.LiquidatorCollateral = "1600000"
	liquidation.OracleUnwindOutputWETH = "1500"
	liquidation.UnwindQuotes = []exactUnwindQuote{
		{Pool: "0x1111111111111111111111111111111111111111", Factory: uniswapFactoryAddress, Token0: wethAddress, Token1: nativeUSDCAddress, Fee: 100, ZeroForOne: false, OutputWETH: "1301"},
		{Pool: "0x2222222222222222222222222222222222222222", Factory: uniswapFactoryAddress, Token0: wethAddress, Token1: nativeUSDCAddress, Fee: 500, ZeroForOne: false, OutputWETH: "1401"},
		{Pool: "0x3333333333333333333333333333333333333333", Factory: uniswapFactoryAddress, Token0: wethAddress, Token1: nativeUSDCAddress, Fee: 3000, ZeroForOne: false, OutputWETH: "1451"},
	}
	realizedByFee := map[uint32]string{100: "300", 500: "400", 3000: "450"}
	costByFee := map[uint32]string{100: "10", 500: "20", 3000: "100"}
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
			primary := exactProvider{ProviderID: primaryProviderID, FlashPremiumBPS: 5, Liquidations: []exactLiquidation{liquidation}}
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
				Primary: primary, Confirmation: nil, Quorum: 1,
			})
		case "/v1/aave/simulate-batch":
			simulationBatches++
			var input simulationBatchRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			if len(input.Simulations) != 3 {
				t.Fatalf("reviewed routes were not compared together: %d", len(input.Simulations))
			}
			seen := map[uint32]bool{}
			results := make([]simulationBatchResult, 0, len(input.Simulations))
			for _, simulation := range input.Simulations {
				if simulation.BlockNumber != 100 || simulation.BlockHash != "0x"+strings.Repeat("a", 64) {
					t.Fatalf("route comparison crossed the exact pin: %+v", simulation)
				}
				seen[simulation.SelectedFee] = true
				results = append(results, simulationBatchResult{
					RequestID: simulation.RequestID,
					Response: testSimulationResponse(
						simulation,
						realizedByFee[simulation.SelectedFee],
						costByFee[simulation.SelectedFee],
						"3",
						liquidation.FlashPremiumAmount,
					),
				})
			}
			if !seen[100] || !seen[500] || !seen[3000] {
				t.Fatalf("reviewed fee coverage is incomplete: %+v", seen)
			}
			writeTestSimulationBatch(writer, input, results)
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()

	record, err := economicTestScreener(server).resolveExact(context.Background(), signal{
		Cursor: 1, Borrower: "0x1111111111111111111111111111111111111111",
	}, nil)
	if err != nil {
		t.Fatal(err)
	}
	if exactCalls != 2 || simulationBatches != 3 || !record.Authority || record.ExecutionCandidate == nil || record.ExecutionCandidate.Legs[0].Fee != 500 || record.SelectedRoute != "UNISWAP_V3_500" {
		t.Fatalf("best post-cost reviewed route was not selected: exact=%d batches=%d record=%+v", exactCalls, simulationBatches, record)
	}
	if record.ExpectedNetPnLWei != "380" || record.ConservativeNetPnLWei != "342" || len(record.SizeDiagnostics) != 3 {
		t.Fatalf("reviewed route economics were not retained: %+v", record)
	}
}

func TestAaveProfitPathCounterfactualReplay(t *testing.T) {
	type replayRoute struct {
		Fee              uint32 `json:"fee"`
		PostFlashEdgeWei string `json:"post_flash_edge_wei"`
		ExecutionCostWei string `json:"execution_cost_wei"`
	}
	type replaySize struct {
		ReviewedSize string        `json:"reviewed_size"`
		Routes       []replayRoute `json:"routes"`
	}
	type replayOpportunity struct {
		ID    string       `json:"id"`
		Sizes []replaySize `json:"sizes"`
	}
	var fixture struct {
		SchemaVersion          string `json:"schema_version"`
		RetainedProfitFloorWei string `json:"retained_profit_floor_wei"`
		EconomicReserveBPS     uint64 `json:"economic_reserve_bps"`
		Scheduler              struct {
			ColdRevisitMS       uint64 `json:"cold_revisit_ms"`
			MaximumHotRevisitMS uint64 `json:"maximum_hot_revisit_ms"`
		} `json:"scheduler"`
		Opportunities []replayOpportunity `json:"opportunities"`
	}
	data, err := os.ReadFile(filepath.Join("..", "..", "..", "fixtures", "replay", "aave_profit_path_counterfactual_v1.json"))
	if err != nil {
		t.Fatal(err)
	}
	if err := json.Unmarshal(data, &fixture); err != nil {
		t.Fatal(err)
	}
	if fixture.SchemaVersion != "phoenix.aave-profit-path-replay.v1" {
		t.Fatalf("unexpected replay schema: %s", fixture.SchemaVersion)
	}
	floor, floorOK := newBigUint(fixture.RetainedProfitFloorWei)
	if !floorOK || floor.Sign() <= 0 {
		t.Fatal("invalid replay floor")
	}
	breakEven := make(map[string]string)
	winners := make(map[string]uint32)
	positiveOpportunities := 0
	for _, opportunity := range fixture.Opportunities {
		for _, size := range opportunity.Sizes {
			var bestConservative *big.Int
			var bestFee uint32
			for _, route := range size.Routes {
				edge, edgeOK := newBigUint(route.PostFlashEdgeWei)
				cost, costOK := newBigUint(route.ExecutionCostWei)
				if !edgeOK || !costOK {
					t.Fatalf("invalid replay economics: %+v", route)
				}
				expected := new(big.Int).Sub(edge, cost)
				reserve, conservative, _ := profitEdgeReserve(expected, edge, fixture.EconomicReserveBPS)
				if reserve.Sign() < 0 || (bestConservative == nil || conservative.Cmp(bestConservative) > 0) {
					bestConservative = conservative
					bestFee = route.Fee
				}
			}
			if bestConservative != nil && bestConservative.Cmp(floor) > 0 {
				breakEven[opportunity.ID] = size.ReviewedSize
				winners[opportunity.ID] = bestFee
				positiveOpportunities++
				break
			}
		}
	}
	if positiveOpportunities != 2 || breakEven["fixed-cost-breaks-at-small"] != "250000000000000" || breakEven["fixed-cost-breaks-at-medium"] != "500000000000000" || breakEven["fixed-cost-still-dominates"] != "" || winners["fixed-cost-breaks-at-small"] != 500 || winners["fixed-cost-breaks-at-medium"] != 100 {
		t.Fatalf("counterfactual replay drifted: positive=%d break_even=%+v winners=%+v", positiveOpportunities, breakEven, winners)
	}
	if fixture.Scheduler.MaximumHotRevisitMS >= fixture.Scheduler.ColdRevisitMS {
		t.Fatalf("scheduler replay drifted: %+v", fixture.Scheduler)
	}
	summary, _ := json.Marshal(map[string]any{
		"fixture_only_not_production_alpha": true,
		"break_even_reviewed_sizes":         breakEven,
		"positive_opportunity_count":        positiveOpportunities,
		"winning_fee_tiers":                 winners,
		"maximum_hot_revisit_ms":            fixture.Scheduler.MaximumHotRevisitMS,
	})
	t.Log(string(summary))
}

func TestAaveExactSchedulerBoundedBeforeAfterModel(t *testing.T) {
	type replayMetrics struct {
		ExactStartAttempts         int
		CooldownDeferrals          int
		SchedulerDeferrals         int
		AdmittedExactSlots         int
		LowDebtFirstExactAtSeconds *int
	}
	start := time.Date(2026, 8, 11, 0, 0, 0, 0, time.UTC)
	high := "0x1111111111111111111111111111111111111111"
	middle := "0x2222222222222222222222222222222222222222"
	low := "0x3333333333333333333333333333333333333333"
	accounts := []account{
		{Borrower: high, TotalDebtBase: "1000000", HealthFactorWAD: "900000000000000000"},
		{Borrower: middle, TotalDebtBase: "1000", HealthFactorWAD: "950000000000000000"},
		{Borrower: low, TotalDebtBase: "1", HealthFactorWAD: "990000000000000000"},
	}

	// This synthetic reference models admission pressure when debt order retries
	// every eligible borrower until a single shared slot is consumed. It is not
	// a claim about the Gateway's continuous token-bucket implementation; the
	// HTTP-path and Rust cache tests below provide the measured code-path data.
	legacy := replayMetrics{}
	legacyLastExact := make(map[string]time.Time)
	legacyLastAdmission := time.Time{}
	for offset := 0; offset <= 180; offset += 10 {
		now := start.Add(time.Duration(offset) * time.Second)
		for _, index := range prioritizedAccountOrder(accounts) {
			borrower := accounts[index].Borrower
			if completedAt, served := legacyLastExact[borrower]; served && now.Sub(completedAt) < exactBorrowerCooldown {
				legacy.CooldownDeferrals++
				continue
			}
			legacy.ExactStartAttempts++
			if !legacyLastAdmission.IsZero() && now.Sub(legacyLastAdmission) < time.Minute {
				break
			}
			legacyLastAdmission = now
			legacyLastExact[borrower] = now
			legacy.AdmittedExactSlots++
			if borrower == low && legacy.LowDebtFirstExactAtSeconds == nil {
				value := offset
				legacy.LowDebtFirstExactAtSeconds = &value
			}
		}
	}

	after := replayMetrics{}
	now := start
	screener := &Screener{
		config:      Config{ExactStateBudgetPerMinute: 60, ExactDiscoveryReservePerMinute: 24},
		lastExactAt: map[string]time.Time{},
		firstLiquidatableAt: map[string]time.Time{
			high: start, middle: start, low: start,
		},
		now: func() time.Time { return now },
	}
	screener.initializeExactBudgetLocked(now)
	for offset := 0; offset <= 180; offset += 10 {
		now = start.Add(time.Duration(offset) * time.Second)
		for _, index := range screener.schedulerAccountOrder(accounts) {
			borrower := accounts[index].Borrower
			if completedAt, served := screener.lastExactAt[borrower]; served && now.Sub(completedAt) < exactBorrowerCooldown {
				after.CooldownDeferrals++
				continue
			}
			reserved, admitted := screener.admitExactLocked(now)
			if !admitted {
				after.SchedulerDeferrals++
				continue
			}
			screener.settleExactBudgetLocked(reserved, 5)
			screener.lastExactAt[borrower] = now
			after.ExactStartAttempts++
			after.AdmittedExactSlots++
			if borrower == low && after.LowDebtFirstExactAtSeconds == nil {
				value := offset
				after.LowDebtFirstExactAtSeconds = &value
			}
		}
	}

	if after.ExactStartAttempts != 6 || after.LowDebtFirstExactAtSeconds == nil || *after.LowDebtFirstExactAtSeconds != 0 {
		t.Fatalf("bounded fair scheduler replay drifted: starts=%d low_first=%d scheduler=%d", after.ExactStartAttempts, func() int {
			if after.LowDebtFirstExactAtSeconds == nil {
				return -1
			}
			return *after.LowDebtFirstExactAtSeconds
		}(), after.SchedulerDeferrals)
	}
	summary, _ := json.Marshal(map[string]any{
		"model_trace_seconds":                        180,
		"modeled_before_exact_start_attempts":        legacy.ExactStartAttempts,
		"modeled_after_exact_start_attempts":         after.ExactStartAttempts,
		"modeled_before_cooldown_deferrals":          legacy.CooldownDeferrals,
		"modeled_after_cooldown_deferrals":           after.CooldownDeferrals,
		"modeled_after_scheduler_deferrals":          after.SchedulerDeferrals,
		"modeled_before_low_debt_first_exact":        "not_reached",
		"modeled_after_low_debt_first_exact_seconds": *after.LowDebtFirstExactAtSeconds,
		"modeled_admitted_exact_slots":               after.AdmittedExactSlots,
	})
	t.Log(string(summary))
}

func TestAaveExactSchedulerHTTPPathIsBoundedAndSinglePrimary(t *testing.T) {
	start := time.Date(2026, 8, 11, 0, 0, 0, 0, time.UTC)
	now := start
	high := "0x1111111111111111111111111111111111111111"
	low := "0x2222222222222222222222222222222222222222"
	exactStarts := map[string]int{}
	var exactStartsMu sync.Mutex
	exactPrimaryChecks := 0
	lowFirstExactSeconds := -1

	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/screen", "/v1/aave/screen-primary":
			var input screenRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			accounts := make([]account, 0, len(input.Borrowers))
			for _, borrower := range input.Borrowers {
				debt := "1000000000000"
				if borrower == low {
					debt = "1000000000"
				}
				accounts = append(accounts, account{
					Borrower: borrower, TotalCollateralBase: "2000000000000",
					TotalDebtBase: debt, AvailableBorrowsBase: "0",
					CurrentLiquidationThreshold: "8000", LoanToValueBPS: "7500",
					HealthFactorWAD: "900000000000000000",
				})
			}
			schema := ResponseSchema
			if request.URL.Path == "/v1/aave/screen-primary" {
				schema = PrimaryScreenResponseSchema
			}
			_ = json.NewEncoder(writer).Encode(screenResponse{
				SchemaVersion: schema, ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 491300000, BlockHash: "0x" + strings.Repeat("a", 64),
				Primary:      providerScreen{ProviderID: primaryProviderID, WETHPriceBase: "300000000000", Accounts: accounts},
				Confirmation: nil,
				Quorum:       1,
			})
		case "/v1/aave/exact":
			var input exactRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			exactStartsMu.Lock()
			exactStarts[input.Borrower]++
			if input.Borrower == low && lowFirstExactSeconds < 0 {
				lowFirstExactSeconds = int(now.Sub(start) / time.Second)
			}
			exactStartsMu.Unlock()
			reserve := exactReserve{
				Asset: wethAddress, CurrentATokenBalance: "1", CurrentStableDebt: "0",
				CurrentVariableDebt: "1000", UsageAsCollateralEnabled: true,
			}
			primary := exactProvider{ProviderID: "production-nownodes-arbitrum", Reserves: []exactReserve{reserve}, FlashPremiumBPS: 5}
			exactPrimaryChecks++
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161,
				RequestID: input.RequestID, BlockNumber: 491300000,
				BlockHash: "0x" + strings.Repeat("b", 64), StateRoot: "0x" + strings.Repeat("c", 64),
				Primary: primary, Confirmation: nil, Quorum: 1,
			})
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()

	sink := &recordingSignalSink{}
	screener := &Screener{
		config: Config{
			GatewayURL: server.URL, StateDir: t.TempDir(), RetainedProfitFloorWei: "1",
			MaximumInputAmountWei: maximumReviewedInputWei, PrimaryDiscovery: true, SignalSink: sink,
			ExactStateBudgetPerMinute: 60, ExactDiscoveryReservePerMinute: 24,
		},
		client: server.Client(), now: func() time.Time { return now },
		state:       State{Schema: StateSchema, Counts: map[string]uint64{}, RouteIneligible: map[string]string{}},
		debtBearing: make(map[string]bool), refreshKnown: make(map[string]bool),
		hotBorrowers: make(map[string]string), hotDebtBase: make(map[string]string),
		lastExactAt: make(map[string]time.Time), firstLiquidatableAt: make(map[string]time.Time),
	}
	for offset := 0; offset <= 110; offset += 10 {
		now = start.Add(time.Duration(offset) * time.Second)
		if err := screener.screen(context.Background(), []string{high, low}, false, nil); err != nil {
			t.Fatalf("screen at +%ds failed: %v", offset, err)
		}
	}
	state := screener.Snapshot()
	if exactStarts[high] != 1 || exactStarts[low] != 1 || state.Counts[exactEvalStartedKey] != 2 || state.Counts[exactEvalCompletedKey] != 2 {
		t.Fatalf("real scheduler path duplicated Exact work: starts=%+v state=%+v", exactStarts, state.Counts)
	}
	if lowFirstExactSeconds != 0 || state.Counts[exactDeferredSchedulerKey] != 0 || state.Counts[exactDeferredCooldownKey] == 0 {
		t.Fatalf("real scheduler path lost bounded fairness evidence: first=%d state=%+v", lowFirstExactSeconds, state.Counts)
	}
	if exactPrimaryChecks != 2 {
		t.Fatalf("real Exact path did not use exactly one primary per evaluation: checks=%d", exactPrimaryChecks)
	}
	for _, record := range sink.records {
		if record.Authority || record.ExecutionCandidate != nil || record.AtlasCandidate != nil {
			t.Fatalf("scheduler timing created Candidate authority: %+v", record)
		}
	}
	if screener.hotBorrowers[low] != "900000000000000000" {
		t.Fatalf("HF refresh stopped while Exact was deferred: %+v", screener.hotBorrowers)
	}
	t.Logf("actual_path exact_starts=2 scheduler_deferrals=%d cooldown_deferrals=%d low_first_exact_seconds=%d primary_checks=%d candidate_authority=0",
		state.Counts[exactDeferredSchedulerKey], state.Counts[exactDeferredCooldownKey], lowFirstExactSeconds, exactPrimaryChecks)
}

func TestAaveExactWorkersDrainEligibleCohortWithBoundedConcurrency(t *testing.T) {
	borrowers := make([]string, 8)
	for index := range borrowers {
		borrowers[index] = fmt.Sprintf("0x%040x", index+1)
	}
	var active atomic.Int64
	var maximumActive atomic.Int64
	var exactCalls atomic.Int64
	release := make(chan struct{})
	started := make(chan struct{}, len(borrowers))
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/screen-primary":
			var input screenRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Error(err)
				return
			}
			accounts := make([]account, len(input.Borrowers))
			for index, borrower := range input.Borrowers {
				accounts[index] = account{
					Borrower: borrower, TotalCollateralBase: "2000000000000",
					TotalDebtBase: "1000000000000", HealthFactorWAD: "900000000000000000",
				}
			}
			_ = json.NewEncoder(writer).Encode(primaryScreenResponse{
				SchemaVersion: PrimaryScreenResponseSchema, ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 491300000, BlockHash: "0x" + strings.Repeat("a", 64),
				Primary: providerScreen{ProviderID: primaryProviderID, WETHPriceBase: "300000000000", Accounts: accounts},
			})
		case "/v1/aave/exact":
			var input exactRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Error(err)
				return
			}
			exactCalls.Add(1)
			current := active.Add(1)
			for current > maximumActive.Load() && !maximumActive.CompareAndSwap(maximumActive.Load(), current) {
			}
			started <- struct{}{}
			<-release
			active.Add(-1)
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161,
				RequestID: input.RequestID, BlockNumber: 491300001,
				BlockHash: "0x" + strings.Repeat("b", 64), StateRoot: "0x" + strings.Repeat("c", 64),
				Primary: exactProvider{ProviderID: primaryProviderID, FlashPremiumBPS: 5}, Quorum: 1,
			})
		default:
			t.Errorf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()

	sink := &recordingSignalSink{}
	screener := &Screener{
		config: Config{
			GatewayURL: server.URL, StateDir: t.TempDir(), RetainedProfitFloorWei: "1",
			MaximumInputAmountWei: maximumReviewedInputWei, PrimaryDiscovery: true, SignalSink: sink,
			ExactStateBudgetPerMinute: 64, ExactDiscoveryReservePerMinute: 24, ExactWorkers: 4,
		},
		client: server.Client(), state: State{
			Schema:          StateSchema,
			Counts:          map[string]uint64{providerDegradedSinceMillisKey: uint64(time.Now().Add(-time.Minute).UnixMilli())},
			RouteIneligible: map[string]string{}, LastErrorClass: "provider_timeout",
		},
		debtBearing: make(map[string]bool), refreshKnown: make(map[string]bool),
		hotBorrowers: make(map[string]string), hotDebtBase: make(map[string]string),
		lastExactAt: make(map[string]time.Time), firstLiquidatableAt: make(map[string]time.Time),
	}
	done := make(chan error, 1)
	go func() { done <- screener.screen(context.Background(), borrowers, false, nil) }()
	for index := 0; index < 4; index++ {
		select {
		case <-started:
		case <-time.After(2 * time.Second):
			t.Fatal("eligible Exact workers did not start immediately")
		}
	}
	select {
	case <-started:
		t.Fatal("observer exceeded its configured Exact worker bound")
	case <-time.After(100 * time.Millisecond):
	}
	close(release)
	if err := <-done; err != nil {
		t.Fatal(err)
	}
	state := screener.Snapshot()
	if exactCalls.Load() != 8 || maximumActive.Load() != 4 || state.Counts[exactEvalStartedKey] != 8 || state.Counts[exactEvalCompletedKey] != 8 {
		t.Fatalf("work-conserving Exact cohort drifted: calls=%d max=%d state=%+v", exactCalls.Load(), maximumActive.Load(), state.Counts)
	}
	if state.ExactEvaluationsInFlight != 0 || state.ExactWorkerQueueDepth != 0 || state.ExactWorkersRunning != 0 {
		t.Fatalf("Exact worker state did not drain: %+v", state)
	}
	if state.LastErrorClass != "" || len(state.ProviderRecoverySamples) != 3 || state.LastPrimaryExactAt == nil {
		t.Fatalf("concurrent successful Exact samples did not restore authority exactly after three samples: %+v", state)
	}
	for _, record := range sink.records {
		if record.Authority || record.ExecutionCandidate != nil || record.AtlasCandidate != nil {
			t.Fatalf("concurrency created Candidate authority: %+v", record)
		}
	}
}

func TestAaveExactWorkerReplayImprovesLatencyWithoutSemanticDrift(t *testing.T) {
	borrowers := make([]string, 8)
	for index := range borrowers {
		borrowers[index] = fmt.Sprintf("0x%040x", index+1)
	}
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/screen-primary":
			var input screenRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Error(err)
				return
			}
			accounts := make([]account, len(input.Borrowers))
			for index, borrower := range input.Borrowers {
				accounts[index] = account{
					Borrower: borrower, TotalCollateralBase: "2000000000000",
					TotalDebtBase: "1000000000000", HealthFactorWAD: "900000000000000000",
				}
			}
			_ = json.NewEncoder(writer).Encode(primaryScreenResponse{
				SchemaVersion: PrimaryScreenResponseSchema, ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 491300000, BlockHash: "0x" + strings.Repeat("a", 64),
				Primary: providerScreen{ProviderID: primaryProviderID, WETHPriceBase: "300000000000", Accounts: accounts},
			})
		case "/v1/aave/exact":
			var input exactRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Error(err)
				return
			}
			time.Sleep(25 * time.Millisecond)
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161,
				RequestID: input.RequestID, BlockNumber: 491300001,
				BlockHash: "0x" + strings.Repeat("b", 64), StateRoot: "0x" + strings.Repeat("c", 64),
				Primary: exactProvider{ProviderID: primaryProviderID, FlashPremiumBPS: 5}, Quorum: 1,
			})
		default:
			t.Errorf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()

	type replayResult struct {
		elapsed time.Duration
		p95     uint64
		records []string
	}
	run := func(workers int) replayResult {
		sink := &recordingSignalSink{}
		screener := &Screener{
			config: Config{
				GatewayURL: server.URL, StateDir: t.TempDir(), RetainedProfitFloorWei: "1",
				MaximumInputAmountWei: maximumReviewedInputWei, PrimaryDiscovery: true, SignalSink: sink,
				ExactStateBudgetPerMinute: 64, ExactDiscoveryReservePerMinute: 24, ExactWorkers: workers,
			},
			client: server.Client(), state: State{Schema: StateSchema, Counts: map[string]uint64{}, RouteIneligible: map[string]string{}},
			debtBearing: make(map[string]bool), refreshKnown: make(map[string]bool),
			hotBorrowers: make(map[string]string), hotDebtBase: make(map[string]string),
			lastExactAt: make(map[string]time.Time), firstLiquidatableAt: make(map[string]time.Time),
		}
		started := time.Now()
		if err := screener.screen(context.Background(), borrowers, false, nil); err != nil {
			t.Fatal(err)
		}
		result := replayResult{elapsed: time.Since(started)}
		latencies := make([]uint64, 0, len(sink.records))
		for _, record := range sink.records {
			result.records = append(result.records, fmt.Sprintf(
				"%s|%s|%t|%s|%t|%t|%s",
				record.Borrower,
				record.TerminalOutcome,
				record.Authority,
				record.ExactPrimaryProvider,
				record.ExactConfirmationProvider == nil,
				record.ExecutionCandidate == nil && record.AtlasCandidate == nil,
				record.ExactRouteIneligibleReason,
			))
			if record.ExactDiagnostics != nil {
				latencies = append(latencies, record.ExactDiagnostics.LiquidatableToExactLatencyMillis)
			}
		}
		sort.Strings(result.records)
		sort.Slice(latencies, func(left, right int) bool { return latencies[left] < latencies[right] })
		if len(latencies) != len(borrowers) {
			t.Fatalf("replay lost Exact diagnostics: %+v", sink.records)
		}
		result.p95 = latencies[(len(latencies)*95-1)/100]
		return result
	}

	before := run(1)
	after := run(4)
	if strings.Join(before.records, "\n") != strings.Join(after.records, "\n") {
		t.Fatalf("bounded worker replay changed economics/authority semantics:\nbefore=%v\nafter=%v", before.records, after.records)
	}
	if after.elapsed*5 >= before.elapsed*4 || after.p95*5 >= before.p95*4 {
		t.Fatalf("bounded workers did not materially reduce latency: before=%s/%dms after=%s/%dms", before.elapsed, before.p95, after.elapsed, after.p95)
	}
	evidence, _ := json.Marshal(map[string]any{
		"borrowers": 8, "before_workers": 1, "after_workers": 4,
		"before_elapsed_ms": before.elapsed.Milliseconds(), "after_elapsed_ms": after.elapsed.Milliseconds(),
		"before_p95_liquidatable_to_exact_ms": before.p95, "after_p95_liquidatable_to_exact_ms": after.p95,
		"semantic_drift": false, "candidate_authority": 0,
	})
	t.Log(string(evidence))
}

func TestForkSaturationDoesNotBlockIndependentExactStateFetch(t *testing.T) {
	record := signal{
		Schema: "phoenix.atlas-aave-hunting-signal.v1", Cursor: 1, Block: 491300000,
		BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
		Borrower: "0x1111111111111111111111111111111111111111",
	}
	var activeForks atomic.Int64
	var maximumForks atomic.Int64
	forkStarted := make(chan struct{}, 3)
	releaseForks := make(chan struct{})
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/simulate-batch":
			var input simulationBatchRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Error(err)
				return
			}
			active := activeForks.Add(1)
			for {
				current := maximumForks.Load()
				if active <= current || maximumForks.CompareAndSwap(current, active) {
					break
				}
			}
			forkStarted <- struct{}{}
			<-releaseForks
			activeForks.Add(-1)
			results := make([]simulationBatchResult, len(input.Simulations))
			for index := range input.Simulations {
				results[index] = simulationBatchResult{
					RequestID: input.Simulations[index].RequestID,
					Error:     &gatewayErrorContract{ErrorClass: "provider_unavailable", Retryable: true},
				}
			}
			_ = json.NewEncoder(writer).Encode(simulationBatchResponse{
				SchemaVersion: "phoenix.rpc.aave-simulate-batch-response.v3", ChainID: 42161,
				RequestID: input.RequestID, BlockNumber: record.Block, BlockHash: record.BlockHash,
				StateRoot: record.StateRoot, PrimaryProviderID: primaryProviderID, Quorum: 1,
				EvidenceMode: directForkEvidenceMode, Results: results,
			})
		case "/v1/aave/exact":
			var input exactRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Error(err)
				return
			}
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161,
				RequestID: input.RequestID, BlockNumber: record.Block, BlockHash: record.BlockHash,
				StateRoot: record.StateRoot,
				Primary:   exactProvider{ProviderID: primaryProviderID, FlashPremiumBPS: 5}, Quorum: 1,
			})
		default:
			t.Errorf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()

	screener := &Screener{
		config: Config{
			GatewayURL: server.URL, MaximumInputAmountWei: maximumReviewedInputWei,
			RetainedProfitFloorWei: "1",
		},
		client: server.Client(), batchClient: server.Client(),
	}
	simulation := simulationRequest{RequestID: "fork", AtlasMode: false}
	forkDone := make(chan error, 3)
	for index := 0; index < 2; index++ {
		go func() {
			_, err := screener.simulateExactBatch(context.Background(), record, []simulationRequest{simulation})
			forkDone <- err
		}()
	}
	for index := 0; index < 2; index++ {
		select {
		case <-forkStarted:
		case <-time.After(2 * time.Second):
			t.Fatal("fork permits were not used work-conservingly")
		}
	}
	go func() {
		_, err := screener.simulateExactBatch(context.Background(), record, []simulationRequest{simulation})
		forkDone <- err
	}()
	select {
	case <-forkStarted:
		t.Fatal("fork concurrency exceeded its independent bound")
	case <-time.After(50 * time.Millisecond):
	}

	exactDone := make(chan error, 1)
	go func() {
		_, err := screener.resolveExact(context.Background(), record, nil)
		exactDone <- err
	}()
	select {
	case err := <-exactDone:
		if err != nil {
			t.Fatalf("independent Exact state fetch failed during fork saturation: %v", err)
		}
	case <-time.After(time.Second):
		t.Fatal("fork saturation blocked an independent Exact state fetch")
	}

	close(releaseForks)
	for index := 0; index < 3; index++ {
		if err := <-forkDone; err != nil {
			t.Fatalf("bounded fork did not drain: %v", err)
		}
	}
	if maximumForks.Load() != defaultExactForkWorkers {
		t.Fatalf("fork concurrency drifted: max=%d", maximumForks.Load())
	}
}

func TestAaveExactSanitizedLoadProfilesDrainWithBoundedBackpressure(t *testing.T) {
	var startedAt atomic.Int64
	var active atomic.Int64
	var maximumActive atomic.Int64
	var latencyMu sync.Mutex
	var dispatchLatencies []time.Duration
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/screen-primary":
			var input screenRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Error(err)
				return
			}
			accounts := make([]account, len(input.Borrowers))
			for index, borrower := range input.Borrowers {
				accounts[index] = account{
					Borrower: borrower, TotalCollateralBase: "2000000000000",
					TotalDebtBase: "1000000000000", HealthFactorWAD: "900000000000000000",
				}
			}
			_ = json.NewEncoder(writer).Encode(primaryScreenResponse{
				SchemaVersion: PrimaryScreenResponseSchema, ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 491300000, BlockHash: "0x" + strings.Repeat("a", 64),
				Primary: providerScreen{ProviderID: primaryProviderID, WETHPriceBase: "300000000000", Accounts: accounts},
			})
		case "/v1/aave/exact":
			var input exactRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Error(err)
				return
			}
			latency := time.Since(time.Unix(0, startedAt.Load()))
			latencyMu.Lock()
			dispatchLatencies = append(dispatchLatencies, latency)
			latencyMu.Unlock()
			current := active.Add(1)
			for {
				maximum := maximumActive.Load()
				if current <= maximum || maximumActive.CompareAndSwap(maximum, current) {
					break
				}
			}
			time.Sleep(25 * time.Millisecond)
			active.Add(-1)
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161,
				RequestID: input.RequestID, BlockNumber: 491300001,
				BlockHash: "0x" + strings.Repeat("b", 64), StateRoot: "0x" + strings.Repeat("c", 64),
				Primary: exactProvider{ProviderID: primaryProviderID, FlashPremiumBPS: 5}, Quorum: 1,
			})
		default:
			t.Errorf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer server.Close()

	for _, profile := range []struct {
		name       string
		borrowers  int
		admitted   int
		maximumP95 time.Duration
		maximumP99 time.Duration
	}{
		{name: "observed", borrowers: 6, admitted: 6, maximumP95: 100 * time.Millisecond, maximumP99: 250 * time.Millisecond},
		{name: "two_x", borrowers: 12, admitted: 12, maximumP95: 100 * time.Millisecond, maximumP99: 250 * time.Millisecond},
		{name: "five_x_burst", borrowers: 30, admitted: 16, maximumP95: 250 * time.Millisecond, maximumP99: 500 * time.Millisecond},
	} {
		t.Run(profile.name, func(t *testing.T) {
			addresses := make([]string, profile.borrowers)
			for index := range addresses {
				addresses[index] = fmt.Sprintf("0x%040x", index+1)
			}
			sink := &recordingSignalSink{}
			screener := &Screener{
				config: Config{
					GatewayURL: server.URL, StateDir: t.TempDir(), RetainedProfitFloorWei: "1",
					MaximumInputAmountWei: maximumReviewedInputWei, PrimaryDiscovery: true, SignalSink: sink,
					ExactStateBudgetPerMinute: 120, ExactDiscoveryReservePerMinute: 36, ExactWorkers: 12,
				},
				client: server.Client(), state: State{Schema: StateSchema, Counts: map[string]uint64{}, RouteIneligible: map[string]string{}},
				debtBearing: make(map[string]bool), refreshKnown: make(map[string]bool),
				hotBorrowers: make(map[string]string), hotDebtBase: make(map[string]string),
				lastExactAt: make(map[string]time.Time), firstLiquidatableAt: make(map[string]time.Time),
			}
			// Production Exact work comes from the durable debt-bearing cohort.
			// Seed that already-persisted identity here so this benchmark measures
			// scheduler/worker dispatch rather than test-host filesystem fsync time
			// for first-ever borrower discovery.
			for _, borrower := range addresses {
				screener.debtBearing[borrower] = true
				screener.refreshKnown[borrower] = true
				screener.refreshOrder = append(screener.refreshOrder, borrower)
			}
			screener.state.DebtBearingCount = uint64(len(addresses))
			latencyMu.Lock()
			dispatchLatencies = nil
			latencyMu.Unlock()
			maximumActive.Store(0)
			startedAt.Store(time.Now().UnixNano())
			if err := screener.screen(context.Background(), addresses, false, nil); err != nil {
				t.Fatal(err)
			}
			state := screener.Snapshot()
			latencyMu.Lock()
			latencies := append([]time.Duration(nil), dispatchLatencies...)
			latencyMu.Unlock()
			sort.Slice(latencies, func(left, right int) bool { return latencies[left] < latencies[right] })
			if len(latencies) != profile.admitted || state.Counts[exactEvalCompletedKey] != uint64(profile.admitted) {
				t.Fatalf("load profile admission drifted: latencies=%d completed=%d state=%+v", len(latencies), state.Counts[exactEvalCompletedKey], state.Counts)
			}
			if maximumActive.Load() > 12 || state.ExactEvaluationsInFlight != 0 || state.ExactWorkerQueueDepth != 0 {
				t.Fatalf("load profile exceeded or failed to drain bounded workers: max=%d state=%+v", maximumActive.Load(), state)
			}
			p95 := latencies[(len(latencies)*95-1)/100]
			p99 := latencies[(len(latencies)*99-1)/100]
			if p95 > profile.maximumP95 || p99 > profile.maximumP99 {
				t.Fatalf("controlled dispatch SLO failed: p95=%s p99=%s", p95, p99)
			}
			deferred := profile.borrowers - profile.admitted
			if state.Counts[exactDeferredSchedulerKey] != uint64(deferred) {
				t.Fatalf("bounded overload evidence drifted: want=%d got=%d", deferred, state.Counts[exactDeferredSchedulerKey])
			}
			for _, persisted := range sink.records {
				if persisted.Authority || persisted.ExecutionCandidate != nil || persisted.AtlasCandidate != nil {
					t.Fatalf("load replay created authority: %+v", persisted)
				}
			}
			t.Logf("profile=%s borrowers=%d admitted=%d p95_dispatch_ms=%d p99_dispatch_ms=%d deferred=%d", profile.name, profile.borrowers, profile.admitted, p95.Milliseconds(), p99.Milliseconds(), deferred)
		})
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
			if input.MaximumInputAmount != maximumReviewedInputWei {
				t.Fatalf("exact maximum input=%s", input.MaximumInputAmount)
			}
			primary := exactProvider{ProviderID: primaryProviderID, FlashPremiumBPS: 5, Liquidations: liquidations}
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
				Primary: primary, Confirmation: nil, Quorum: 1,
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
			liquidation.UnwindQuotes[0].OutputWETH = output
		}
		key := spec.collateral + "|" + liquidation.RepayAmount
		realizedByVariant[key] = strconv.FormatInt(spec.edge, 10)
		flashByVariant[key] = liquidation.FlashPremiumAmount
		liquidations = append(liquidations, liquidation)
	}
	probeWinnerKey := wethAddress + "|4000"
	failedFinalKey := wethAddress + "|3000"
	finalWinnerKey := nativeUSDCAddress + "|1000"
	freshSelectedKey := wethAddress + "|1000"
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
			primary := exactProvider{ProviderID: primaryProviderID, FlashPremiumBPS: 5, Liquidations: liquidations}
			blockNumber := uint64(100)
			blockHash := "0x" + strings.Repeat("a", 64)
			stateRoot := "0x" + strings.Repeat("b", 64)
			if exactCalls == 2 {
				blockNumber = 101
				blockHash = "0x" + strings.Repeat("e", 64)
				stateRoot = "0x" + strings.Repeat("f", 64)
			}
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: blockNumber, BlockHash: blockHash, StateRoot: stateRoot,
				Primary: primary, Confirmation: nil, Quorum: 1,
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
				if simulationBatches == 4 && key == freshSelectedKey {
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
	if !record.Authority || record.ExecutionCandidate == nil || record.ExecutionCandidate.SelectedSize != "1000" || record.SelectedRoute != "WETH_IDENTITY" {
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
	if freshSelectedKey != strings.ToLower(record.ExecutionCandidate.RoutePayload.CollateralAsset)+"|"+record.ExecutionCandidate.SelectedSize || record.ExpectedNetPnLWei != "190" || record.ConservativeNetPnLWei != "171" || record.ExecutionCandidate.MinimumProfit != "111" || record.ExecutionCandidate.RoutePayload.MinimumUnwindOutput != "1182" || record.ExecutionCandidate.SimulationResultHash != freshWinnerHash || uint64(record.ExecutionCandidate.Deadline.Unix()) != freshDeadline {
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
				Primary:      providerScreen{ProviderID: primaryProviderID, WETHPriceBase: "100000000", Accounts: accounts},
				Confirmation: nil,
				Quorum:       1,
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
			primary := exactProvider{ProviderID: primaryProviderID}
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
				Primary: primary, Confirmation: nil, Quorum: 1,
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
		primary := exactProvider{ProviderID: primaryProviderID, FlashPremiumBPS: 5, Liquidations: []exactLiquidation{liquidation}}
		_ = json.NewEncoder(writer).Encode(exactResponse{
			SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161, RequestID: input.RequestID,
			BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
			Primary: primary, Confirmation: nil, Quorum: 1,
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
			primary := exactProvider{ProviderID: primaryProviderID, FlashPremiumBPS: 5, Liquidations: []exactLiquidation{liquidation}}
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: uint64(100 + exactCalls - 1), BlockHash: "0x" + strings.Repeat(strconv.Itoa(exactCalls), 64), StateRoot: "0x" + strings.Repeat(strconv.Itoa(exactCalls+2), 64),
				Primary: primary, Confirmation: nil, Quorum: 1,
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

func TestMoneyEquationChargesFlashGasBidAndReserveExactlyOnce(t *testing.T) {
	screener := &Screener{config: Config{MaximumGasLimit: 5, MaximumFeePerGasWei: "10"}}
	liquidation := &exactLiquidation{FlashPremiumAmount: "7"}
	direct := &simulationResponse{
		RealizedProfit: "100", ConservativeNetPnL: "50", EstimatedGasLimit: 5,
		EstimatedMaxFeePerGasWei: "10", EstimatedExecutionCostWei: "50", EstimatedL1CostWei: "30", FlashPremiumWei: "7",
	}
	realized, cost, _, err := screener.boundedSimulationEconomics(direct, liquidation)
	if err != nil {
		t.Fatal(err)
	}
	net, err := authoritativeGatewayNet(direct, realized, cost, big.NewInt(0))
	if err != nil || net.String() != "50" {
		t.Fatalf("post-premium realization did not charge total gas exactly once: net=%v err=%v", net, err)
	}
	if incorrectlyChargedAgain := new(big.Int).Sub(new(big.Int).Set(net), big.NewInt(7)); incorrectlyChargedAgain.String() != "43" || incorrectlyChargedAgain.Cmp(net) == 0 {
		t.Fatalf("flash premium double-charge mutation was not detected: %s", incorrectlyChargedAgain)
	}

	atlas := *direct
	atlas.ConservativeNetPnL = "40"
	if net, err = authoritativeGatewayNet(&atlas, realized, cost, big.NewInt(10)); err != nil || net.String() != "40" {
		t.Fatalf("Atlas bid was not charged once: net=%v err=%v", net, err)
	}
	if _, err := authoritativeGatewayNet(&atlas, realized, cost, big.NewInt(0)); err == nil {
		t.Fatal("omitted Atlas bid mutation was accepted")
	}

	reserve, conservative, _ := profitEdgeReserve(net, big.NewInt(1_000), 1_000)
	if reserve.String() != "4" || conservative.String() != "36" {
		t.Fatalf("risk reserve was not charged once: reserve=%s conservative=%s", reserve, conservative)
	}
	if minimum := strictMinimumProfit(big.NewInt(100), cost); minimum.String() != "151" {
		t.Fatalf("strict minProfit omitted cost exposure or strict edge: %s", minimum)
	}

	doubleGas := *direct
	doubleGas.EstimatedExecutionCostWei = "100"
	if _, _, _, err := screener.boundedSimulationEconomics(&doubleGas, liquidation); err == nil {
		t.Fatal("double execution-cost mutation was accepted")
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
			SchemaVersion: "phoenix.rpc.aave-simulate-batch-response.v3", ChainID: 42161, RequestID: batch.RequestID,
			BlockNumber: input.BlockNumber, BlockHash: input.BlockHash, StateRoot: input.StateRoot,
			PrimaryProviderID: primaryProviderID, ConfirmationProviderID: nil, Quorum: 1, EvidenceMode: atlasCallbackEvidenceMode,
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
			primary := exactProvider{ProviderID: primaryProviderID, FlashPremiumBPS: 5, Liquidations: []exactLiquidation{liquidation}}
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
				Primary: primary, Confirmation: nil, Quorum: 1,
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
	if _, err := sink.RecordAaveSignal(context.Background(), record); err != nil {
		t.Fatal(err)
	}
	if len(sink.records) != 1 || sink.records[0].ExecutionCandidate != nil || sink.records[0].AtlasCandidate != nil || signalRejectionReason(sink.records[0]) != "atlas_callback_evidence_unavailable" {
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
		OracleUnwindOutputWETH: strconv.FormatInt(amount+1, 10),
		UnwindQuotes: []exactUnwindQuote{{
			Pool: "0xc6962004f452be9203591991d15f6b388e09e8d0", Factory: uniswapFactoryAddress,
			Token0: wethAddress, Token1: nativeUSDCAddress, Fee: 500, ZeroForOne: false,
			OutputWETH: strconv.FormatInt(amount+1, 10),
		}},
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
		SchemaVersion: "phoenix.rpc.aave-simulate-response.v4", ChainID: 42161, RequestID: input.RequestID,
		BlockNumber: input.BlockNumber, BlockHash: input.BlockHash, StateRoot: input.StateRoot,
		PrimaryProviderID: primaryProviderID, ConfirmationProviderID: nil, Quorum: 1, EvidenceMode: testEvidenceMode(input),
		RouteID: "0x" + hex.EncodeToString(routeHash[:]), CalldataHex: "0x" + hex.EncodeToString(calldata),
		CalldataHash: hex.EncodeToString(calldataHash[:]), SimulationResultHash: hex.EncodeToString(resultHash[:]),
		RealizedProfit: realizedText, ConservativeNetPnL: conservative.String(), EstimatedGasLimit: estimatedGas.Uint64(),
		EstimatedMaxFeePerGasWei: input.MaxFeePerGas, EstimatedExecutionCostWei: costText, EstimatedL1CostWei: l1Text, FlashPremiumWei: flashText,
		DeadlineUnixSeconds: input.DeadlineUnixSeconds,
	}
}

func testEvidenceMode(input simulationRequest) string {
	if input.Counterfactual {
		return counterfactualForkEvidenceMode
	}
	return directForkEvidenceMode
}

func writeTestSimulationBatch(writer http.ResponseWriter, input simulationBatchRequest, results []simulationBatchResult) {
	first := input.Simulations[0]
	_ = json.NewEncoder(writer).Encode(simulationBatchResponse{
		SchemaVersion: "phoenix.rpc.aave-simulate-batch-response.v3", ChainID: 42161, RequestID: input.RequestID,
		BlockNumber: first.BlockNumber, BlockHash: first.BlockHash, StateRoot: first.StateRoot,
		PrimaryProviderID: primaryProviderID, ConfirmationProviderID: nil, Quorum: 1, EvidenceMode: directForkEvidenceMode,
		Results: results,
	})
}

func TestMultiChunkReviewedRouteEvaluationRefreshesDeadlineWithoutChangingPin(t *testing.T) {
	deadlines := make([]uint64, 0, 2)
	server := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		var input simulationBatchRequest
		if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
			t.Fatal(err)
		}
		deadlines = append(deadlines, input.Simulations[0].DeadlineUnixSeconds)
		results := make([]simulationBatchResult, 0, len(input.Simulations))
		for _, simulation := range input.Simulations {
			if simulation.BlockNumber != 100 || simulation.BlockHash != "0x"+strings.Repeat("a", 64) || simulation.StateRoot != "0x"+strings.Repeat("b", 64) {
				t.Fatalf("chunk changed the exact pin: %+v", simulation)
			}
			results = append(results, simulationBatchResult{
				RequestID: simulation.RequestID,
				Response:  testSimulationResponse(simulation, "1000", "10", "3", "1"),
			})
		}
		writeTestSimulationBatch(writer, input, results)
	}))
	defer server.Close()
	screener := economicTestScreener(server)
	base := time.Unix(1_800_000_000, 0).UTC()
	nowCalls := 0
	screener.now = func() time.Time {
		value := base.Add(time.Duration(nowCalls) * 30 * time.Second)
		nowCalls++
		return value
	}
	record := signal{
		Cursor: 1, Borrower: "0x1111111111111111111111111111111111111111", Block: 100,
		BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
	}
	requests := make([]simulationRequest, 0, 9)
	for index := 0; index < 9; index++ {
		liquidation := boundedTestLiquidation(wethAddress, int64(1_000+index), 5)
		requests = append(requests, screener.newSimulationRequest(record, &liquidation, simulationRequest{
			MinimumCollateralReceived: liquidation.LiquidatorCollateral,
			MinimumUnwindOutput:       "1",
			MinimumProfit:             "1",
			ExpectedProfit:            "1000",
			SelectedPool:              zeroAddress,
			SelectedFactory:           zeroAddress,
		}, uint64(base.Unix()), "4000"))
	}
	outcomes, err := screener.simulateExactBatch(context.Background(), record, requests)
	if err != nil || len(outcomes) != 9 {
		t.Fatalf("outcomes=%d err=%v", len(outcomes), err)
	}
	if len(deadlines) != 2 || deadlines[0] != uint64(base.Add(60*time.Second).Unix()) || deadlines[1] != uint64(base.Add(90*time.Second).Unix()) {
		t.Fatalf("chunk deadlines=%v", deadlines)
	}
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
			providers := providerScreen{ProviderID: primaryProviderID, WETHPriceBase: "100000000", Accounts: accounts}
			_ = json.NewEncoder(writer).Encode(screenResponse{
				SchemaVersion: ResponseSchema, ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), Primary: providers, Confirmation: nil, Quorum: 1,
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
			primary := exactProvider{ProviderID: primaryProviderID, Reserves: []exactReserve{
				{Asset: wethAddress, CurrentATokenBalance: "0", CurrentStableDebt: stable, CurrentVariableDebt: variable},
				{Asset: nativeUSDCAddress, CurrentATokenBalance: "1000", UsageAsCollateralEnabled: true},
			}}
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64),
				Primary: primary, Confirmation: nil, Quorum: 1,
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
	if len(exactCalls) != 1 || exactCalls[0] != stableBorrower {
		t.Fatalf("screen-wide Exact capacity was not bounded deterministically: %v", exactCalls)
	}
	state := screener.Snapshot()
	if state.RouteIneligible[stableBorrower] != "unsupported_stable_weth_debt" || state.RouteIneligible[otherBorrower] != "" || len(sink.records) != 2 || sink.records[1].Borrower != otherBorrower || sink.records[1].ExactDeferredReason != "scheduler_capacity" {
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
			primary := providerScreen{ProviderID: primaryProviderID, WETHPriceBase: "100000000", Accounts: accounts}
			_ = json.NewEncoder(writer).Encode(screenResponse{SchemaVersion: ResponseSchema, ChainID: 42161, RequestID: input.RequestID, BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), Primary: primary, Confirmation: nil, Quorum: 1})
		case "/v1/aave/exact":
			exactCalls++
			var input exactRequest
			if err := json.NewDecoder(request.Body).Decode(&input); err != nil {
				t.Fatal(err)
			}
			primary := exactProvider{ProviderID: primaryProviderID, FlashPremiumBPS: 5, Liquidations: []exactLiquidation{liquidation}}
			_ = json.NewEncoder(writer).Encode(exactResponse{SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161, RequestID: input.RequestID, BlockNumber: 100, BlockHash: "0x" + strings.Repeat("a", 64), StateRoot: "0x" + strings.Repeat("b", 64), Primary: primary, Confirmation: nil, Quorum: 1})
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
	oldObservedAt := time.Now().UTC().Add(-time.Hour)
	oldCompletedAt := oldObservedAt.Add(time.Second)
	if err := appendJSON(filepath.Join(directory, "signals.ndjson"), signal{
		Schema: "phoenix.atlas-aave-hunting-signal.v1", ObservedAt: oldObservedAt,
		ExactCompletedAt: &oldCompletedAt, Borrower: borrower, DebtBase: "1000",
		HF: "900000000000000000", Bucket: "liquidatable", StateRoot: "0x" + strings.Repeat("c", 64),
		ZeroCostProfitUpperBoundWei: "2", TerminalOutcome: "fork_pending",
	}); err != nil {
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
			SchemaVersion:        "phoenix.rpc.aave-tail-response.v2",
			ChainID:              42161,
			RequestID:            input.RequestID,
			FinalizedBlockNumber: 100,
			FinalizedBlockHash:   "0x" + strings.Repeat("b", 64),
			FromBlock:            100,
			ToBlock:              100,
			NextBlock:            101,
			PrimaryProviderID:    "production-nownodes-arbitrum",
			ConfirmationProvider: nil,
			Quorum:               1,
			Borrowers:            []string{borrower},
		})
	}))
	defer server.Close()

	second.config.GatewayURL = server.URL
	second.client = server.Client()
	second.state.TailNextBlock = 100
	second.debtBearing = make(map[string]bool)
	second.refreshKnown = make(map[string]bool)
	second.hotBorrowers = map[string]string{borrower: "900000000000000000"}
	second.hotDebtBase = map[string]string{borrower: "1000"}
	second.hotUpperPositive = map[string]bool{borrower: true}
	second.latestOutcome = map[string]string{borrower: "fork_pending"}
	second.firstLiquidatableAt = map[string]time.Time{borrower: time.Now().UTC().Add(-time.Hour)}

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
	if _, exists := second.firstLiquidatableAt[borrower]; exists || second.latestOutcome[borrower] != "" || second.hotUpperPositive[borrower] {
		t.Fatalf("tail event retained stale Exact evidence: epoch=%v outcome=%q upper_positive=%t", second.firstLiquidatableAt[borrower], second.latestOutcome[borrower], second.hotUpperPositive[borrower])
	}
	if second.Snapshot().ActiveForkPendingCount != 0 {
		t.Fatalf("tail-invalidated fork evidence remained active: %+v", second.Snapshot())
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
	if durable.TailInvalidatedBlock[borrower] != 100 {
		t.Fatalf("tail Exact invalidation was not block-bound: %+v", durable.TailInvalidatedBlock)
	}

	// A future wall-clock timestamp cannot make pre-tail block evidence newer,
	// and a lagging dual screen must not clear the tombstone or start Exact.
	second.applyHotSignal(signal{
		ObservedAt: time.Now().UTC().Add(24 * time.Hour), Block: 99,
		Borrower: borrower, Bucket: "liquidatable", DebtBase: "1000",
		HF: "900000000000000000", ZeroCostProfitUpperBoundWei: "2",
		TerminalOutcome: "fork_pending", StateRoot: "0x" + strings.Repeat("c", 64),
	})
	if second.latestOutcome[borrower] != "" || second.state.TailInvalidatedBlock[borrower] != 100 {
		t.Fatalf("pre-tail block evidence bypassed the tombstone: outcome=%q tombstones=%+v", second.latestOutcome[borrower], second.state.TailInvalidatedBlock)
	}
	screenBlock := uint64(99)
	exactRequests := 0
	screenServer := httptest.NewServer(http.HandlerFunc(func(writer http.ResponseWriter, request *http.Request) {
		switch request.URL.Path {
		case "/v1/aave/screen":
			var input screenRequest
			_ = json.NewDecoder(request.Body).Decode(&input)
			borrowerAccount := account{
				Borrower: borrower, TotalCollateralBase: "2000000000000",
				TotalDebtBase: "1000000000000", CurrentLiquidationThreshold: "8000",
				HealthFactorWAD: "900000000000000000",
			}
			_ = json.NewEncoder(writer).Encode(screenResponse{
				SchemaVersion: ResponseSchema, ChainID: 42161, RequestID: input.RequestID,
				BlockNumber: screenBlock, BlockHash: "0x" + strings.Repeat("d", 64),
				Primary:      providerScreen{ProviderID: primaryProviderID, WETHPriceBase: "300000000000", Accounts: []account{borrowerAccount}},
				Confirmation: nil,
				Quorum:       1,
			})
		case "/v1/aave/exact":
			exactRequests++
			var input exactRequest
			_ = json.NewDecoder(request.Body).Decode(&input)
			primary := exactProvider{ProviderID: primaryProviderID}
			_ = json.NewEncoder(writer).Encode(exactResponse{
				SchemaVersion: "phoenix.rpc.aave-exact-response.v4", ChainID: 42161,
				RequestID: input.RequestID, BlockNumber: 99,
				BlockHash: "0x" + strings.Repeat("e", 64), StateRoot: "0x" + strings.Repeat("f", 64),
				Primary: primary, Confirmation: nil, Quorum: 1,
			})
		default:
			t.Fatalf("unexpected path: %s", request.URL.Path)
		}
	}))
	defer screenServer.Close()
	second.config.GatewayURL = screenServer.URL
	second.config.RetainedProfitFloorWei = "1"
	second.config.MaximumInputAmountWei = maximumReviewedInputWei
	second.client = screenServer.Client()
	if err := second.screen(context.Background(), []string{borrower}, false, nil); err == nil || exactRequests != 0 {
		t.Fatalf("pre-tail screen was not rejected before Exact: calls=%d err=%v", exactRequests, err)
	}
	screenBlock = 100
	if err := second.screen(context.Background(), []string{borrower}, false, nil); err == nil || exactRequests != 1 {
		t.Fatalf("Exact evidence older than its screen was not rejected: calls=%d err=%v", exactRequests, err)
	}
	restarted := &Screener{
		config: Config{StateDir: directory, DiscoverySHA256: discoveryHash, StartingCursor: 0, RetainedProfitFloorWei: "1"},
	}
	if err := restarted.loadState(); err != nil {
		t.Fatal(err)
	}
	if err := restarted.loadHotSignals(); err != nil {
		t.Fatal(err)
	}
	if restarted.latestOutcome[borrower] != "" || !restarted.lastExactAt[borrower].IsZero() || !restarted.firstLiquidatableAt[borrower].IsZero() || restarted.hotUpperPositive[borrower] || restarted.Snapshot().ActiveForkPendingCount != 0 {
		t.Fatalf("restart resurrected tail-invalidated Exact evidence: outcome=%q exact=%v epoch=%v", restarted.latestOutcome[borrower], restarted.lastExactAt[borrower], restarted.firstLiquidatableAt[borrower])
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
