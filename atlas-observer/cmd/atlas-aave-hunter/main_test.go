package main

import (
	"strings"
	"testing"
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/hunter"
	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
)

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
