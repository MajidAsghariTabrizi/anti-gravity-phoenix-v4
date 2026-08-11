package main

import (
	"context"
	"errors"
	"flag"
	"io"
	"log"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/hunter"
	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
)

func TestFinancialCeilingFlagsBindExactValues(t *testing.T) {
	flags := flag.NewFlagSet("financial-ceilings", flag.ContinueOnError)
	var maximumInputAmount, maximumAtlasBidWei string
	bindFinancialCeilingFlags(flags, &maximumInputAmount, &maximumAtlasBidWei)
	if err := flags.Parse([]string{
		"--maximum-input-amount=250000000000000000",
		"--maximum-atlas-bid-wei=1000000000000000",
	}); err != nil {
		t.Fatal(err)
	}
	if maximumInputAmount != "250000000000000000" || maximumAtlasBidWei != "1000000000000000" {
		t.Fatalf("financial ceilings were not bound exactly: input=%s atlas_bid=%s", maximumInputAmount, maximumAtlasBidWei)
	}
}

type boundaryError struct {
	class     string
	retryable bool
}

func (e boundaryError) Error() string { return e.class }

type boundaryScreener struct {
	errors      []error
	handled     chan int32
	calls       atomic.Int32
	active      atomic.Int32
	maximumLive atomic.Int32
	state       hunter.State
}

func (s *boundaryScreener) HandleAtlasAuction(_ context.Context, _ *observer.LedgerRecord) error {
	active := s.active.Add(1)
	defer s.active.Add(-1)
	for {
		maximum := s.maximumLive.Load()
		if active <= maximum || s.maximumLive.CompareAndSwap(maximum, active) {
			break
		}
	}
	call := s.calls.Add(1)
	s.handled <- call
	if int(call) <= len(s.errors) {
		return s.errors[call-1]
	}
	return nil
}

func (s *boundaryScreener) RecordRetryableGatewayError(err error) (bool, error) {
	var boundary boundaryError
	if !errors.As(err, &boundary) || !boundary.retryable {
		return false, nil
	}
	s.state.LastErrorClass = boundary.class
	return true, nil
}

func (s *boundaryScreener) Snapshot() hunter.State { return s.state }

func TestAtlasCandidateLoopKeepsSubscriptionActiveForRetryableProviderErrors(t *testing.T) {
	for _, class := range []string{"provider_disagreement", "provider_unavailable", "provider_timeout", "provider_rate_limited"} {
		t.Run(class, func(t *testing.T) {
			ctx, cancel := context.WithCancel(context.Background())
			auctions := make(chan *observer.LedgerRecord, 2)
			auctions <- &observer.LedgerRecord{}
			auctions <- &observer.LedgerRecord{}
			screener := &boundaryScreener{
				errors:  []error{boundaryError{class: class, retryable: true}, nil},
				handled: make(chan int32, 2),
			}
			result := make(chan error, 1)
			go func() {
				result <- runAtlasCandidateLoop(ctx, auctions, screener, log.New(io.Discard, "", 0))
			}()
			for observed := int32(1); observed <= 2; observed++ {
				select {
				case call := <-screener.handled:
					if call != observed {
						t.Fatalf("call=%d want=%d", call, observed)
					}
				case <-time.After(time.Second):
					t.Fatal("Atlas candidate loop stopped during retryable degradation")
				}
			}
			cancel()
			if err := <-result; err != nil {
				t.Fatal(err)
			}
			if screener.maximumLive.Load() != 1 || screener.calls.Load() != 2 {
				t.Fatalf("duplicate recovery workers or stopped hunting: max_live=%d calls=%d", screener.maximumLive.Load(), screener.calls.Load())
			}
		})
	}
}

func TestAtlasCandidateLoopTerminatesForUnknownFatalError(t *testing.T) {
	fatal := boundaryError{class: "provider_integrity_failure", retryable: false}
	auctions := make(chan *observer.LedgerRecord, 1)
	auctions <- &observer.LedgerRecord{}
	screener := &boundaryScreener{errors: []error{fatal}, handled: make(chan int32, 1)}
	err := runAtlasCandidateLoop(context.Background(), auctions, screener, log.New(io.Discard, "", 0))
	if !errors.Is(err, fatal) {
		t.Fatalf("fatal error was swallowed: %v", err)
	}
}

func TestTemporaryProviderFailureKeepsServiceAndHuntingHealthyWithoutAuthority(t *testing.T) {
	now := time.Now().UTC()
	health := evaluateLaneHealth(now, observer.LedgerState{
		Connected: true, LastSubscriptionAt: &now,
	}, hunter.State{
		Cursor: 1100, LastAttemptAt: &now, LastErrorClass: "provider_unavailable",
	})
	if !health.ServiceHealthy || !health.HuntingHealthy || health.ExactExecutionReady {
		t.Fatalf("temporary provider degradation crossed a lane boundary: %+v", health)
	}
	if health.DegradedReason != "provider_unavailable" || health.RecoveryState != "recovering" {
		t.Fatalf("provider recovery evidence is incomplete: %+v", health)
	}
}

func TestDivergentRevenueAuthorityIsFailClosedWithoutFailingServiceLiveness(t *testing.T) {
	now := time.Now().UTC()
	health := evaluateLaneHealth(now, observer.LedgerState{
		Connected: true, LastSubscriptionAt: &now,
	}, hunter.State{
		Cursor: 1200, LastBatchAt: &now, LastTailAt: &now, LastAttemptAt: &now,
		LastDualAgreementAt: &now, LastErrorClass: "revenue_lane_authority_diverged",
		LastBlockNumber: 491273850, LastBlockHash: "0x" + strings.Repeat("a", 64),
		LastProviderPrimary: "production-primary", LastProviderSecond: "production-secondary",
	})
	if !health.ServiceHealthy || health.HuntingHealthy || health.ExactExecutionReady {
		t.Fatalf("divergent revenue authority crossed the readiness boundary: %+v", health)
	}
	if health.DegradedReason != "revenue_lane_authority_diverged" || health.RecoveryState != "recovering" {
		t.Fatalf("divergent revenue authority was not exposed clearly: %+v", health)
	}
}

func TestDivergentRevenueAuthorityKeepsHealthAliveAndReadinessClosed(t *testing.T) {
	health := laneHealth{
		ServiceHealthy:      true,
		HuntingHealthy:      false,
		ExactExecutionReady: false,
		DegradedReason:      "revenue_lane_authority_diverged",
		RecoveryState:       "recovering",
	}
	state := hunter.State{
		LastErrorClass: "revenue_lane_authority_diverged",
		Counts:         map[string]uint64{},
	}
	service := httptest.NewRecorder()
	writeHealthResponse(service, health, state, false, false)
	if service.Code != http.StatusOK || !strings.Contains(service.Body.String(), `"ok":true`) {
		t.Fatalf("service liveness did not remain healthy: code=%d body=%s", service.Code, service.Body.String())
	}
	readiness := httptest.NewRecorder()
	writeHealthResponse(readiness, health, state, true, true)
	if readiness.Code != http.StatusServiceUnavailable ||
		!strings.Contains(readiness.Body.String(), `"ok":false`) ||
		!strings.Contains(readiness.Body.String(), `"degraded_reason":"revenue_lane_authority_diverged"`) {
		t.Fatalf("divergent authority did not fail readiness closed: code=%d body=%s", readiness.Code, readiness.Body.String())
	}
}

func TestHealthPayloadPublishesCurrentProviderFailureStreak(t *testing.T) {
	payload := healthPayload(laneHealth{}, hunter.State{Counts: map[string]uint64{
		"provider_current_class_failure_streak": 2,
	}}, false)
	if got := payload["provider_current_class_failure_streak"]; got != uint64(2) {
		t.Fatalf("current provider failure streak=%v want=2", got)
	}
}

func TestExactReadinessRequiresFreshDualAgreement(t *testing.T) {
	now := time.Now().UTC()
	state := hunter.State{
		Cursor: 1200, LastBatchAt: &now, LastTailAt: &now, LastDualAgreementAt: &now,
		LastBlockNumber: 491273850, LastBlockHash: "0x" + strings.Repeat("a", 64),
		LastProviderPrimary: "production-nownodes-arbitrum",
		LastProviderSecond:  "production-slot-0",
	}
	health := evaluateLaneHealth(now, observer.LedgerState{
		Connected: true, LastSubscriptionAt: &now,
	}, state)
	if !health.ServiceHealthy || !health.HuntingHealthy || !health.ExactExecutionReady || health.RecoveryState != "ready" {
		t.Fatalf("fresh independent agreement was not exact-ready: %+v", health)
	}

	state.LastProviderSecond = state.LastProviderPrimary
	health = evaluateLaneHealth(now, observer.LedgerState{
		Connected: true, LastSubscriptionAt: &now,
	}, state)
	if health.ExactExecutionReady {
		t.Fatalf("one provider identity granted exact authority: %+v", health)
	}
}

func TestExactExecutionReadinessFalseWhileProviderCircuitIsOpen(t *testing.T) {
	now := time.Now().UTC()
	state := hunter.State{
		Cursor:                             1200,
		LastBatchAt:                        &now,
		LastTailAt:                         &now,
		LastDualAgreementAt:                &now,
		LastBlockNumber:                    491273850,
		LastBlockHash:                      "0x" + strings.Repeat("a", 64),
		LastProviderPrimary:                "production-nownodes-arbitrum",
		LastProviderSecond:                 "production-slot-0",
		ProviderCircuitOpenUntilUnixMillis: now.Add(2 * time.Minute).UnixMilli(),
	}
	health := evaluateLaneHealth(now, observer.LedgerState{
		Connected: true, LastSubscriptionAt: &now,
	}, state)
	if health.ExactExecutionReady {
		t.Fatalf("circuit-open exact readiness was not blocked: %+v", health)
	}
}

func TestStaleHuntingActivityFailsReadiness(t *testing.T) {
	now := time.Now().UTC()
	stale := now.Add(-laneFreshnessWindow)
	health := evaluateLaneHealth(now, observer.LedgerState{
		Connected: true, LastSubscriptionAt: &now,
	}, hunter.State{Cursor: 1100, LastAttemptAt: &stale})
	if !health.ServiceHealthy || health.HuntingHealthy || health.ExactExecutionReady {
		t.Fatalf("stale hunting activity was accepted: %+v", health)
	}
}
