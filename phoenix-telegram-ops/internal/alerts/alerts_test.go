package alerts

import (
	"testing"
	"time"

	"anti-gravity-phoenix-v4/phoenix-telegram-ops/internal/opsstate"

	"database/sql"
)

func baseSnap() opsstate.Snapshot {
	s := opsstate.Snapshot{
		TakenAt:    time.Date(2026, 8, 24, 12, 0, 0, 0, time.UTC),
		Phase:      "DISARMED_EVIDENCE",
		ReleaseSHA: "8ac529a9af7caeacb8883a51024e5970dae6f281",
		Lanes: []opsstate.LaneState{
			{Lane: "aave_liquidation", Armed: false, KillSwitch: true, DisarmReason: "disarmed_deploy", ControlEpoch: 139},
			{Lane: "atlas_solver", Armed: false, KillSwitch: true, DisarmReason: "disarmed_deploy", ControlEpoch: 137},
		},
		Provider: opsstate.ProviderAuthority{ExactExecutionReady: true, GateReason: "ok"},
		Windows: map[string]opsstate.WindowStats{
			"24H": {RealizedNetPnlWei: "0"},
		},
	}
	return s
}

func TestFirstSnapshotNeverAlerts(t *testing.T) {
	cur := baseSnap()
	if got := Diff(nil, cur); len(got) != 0 {
		t.Fatalf("first snapshot must not alert, got %v", got)
	}
}

func TestArmAndKillSwitchTransitions(t *testing.T) {
	prev := baseSnap()
	cur := baseSnap()
	cur.Lanes[0].Armed = true
	cur.Lanes[0].KillSwitch = false
	got := Diff(&prev, cur)
	var arm, kill bool
	for _, a := range got {
		if a.Key == "arm/aave_liquidation" && contains(a.Text, "-> ARMED") {
			arm = true
		}
		if a.Key == "kill/aave_liquidation" && contains(a.Text, "RELEASED") {
			kill = true
		}
	}
	if !arm || !kill {
		t.Fatalf("expected arm+kill-release alerts, got %+v", got)
	}

	prev2 := baseSnap()
	cur2 := baseSnap()
	cur2.Lanes[1].KillSwitch = false
	cur2.Lanes[1].Armed = true
	got2 := Diff(&prev2, cur2)
	engaged := false
	for _, a := range got2 {
		_ = a
	}
	// inverse direction: kill switch engaged alert when going live->killed
	prev3 := baseSnap()
	prev3.Lanes[1].Armed = true
	prev3.Lanes[1].KillSwitch = false
	cur3 := baseSnap() // kill switch back on, disarmed
	got3 := Diff(&prev3, cur3)
	for _, a := range got3 {
		if a.Key == "kill/atlas_solver" && contains(a.Text, "ENGAGED") {
			engaged = true
		}
	}
	if !engaged {
		t.Fatalf("expected kill-switch-engaged alert, got %+v", got3)
	}
	_ = got2
}

func TestProviderDegradationAndRecovery(t *testing.T) {
	prev := baseSnap()
	cur := baseSnap()
	cur.Provider.ExactExecutionReady = false
	cur.Provider.GateReason = "provider_unhealthy"
	got := Diff(&prev, cur)
	if len(got) != 1 || got[0].Key != "provider/degraded" || !contains(got[0].Text, "provider_unhealthy") {
		t.Fatalf("expected single degradation alert, got %+v", got)
	}
	back := baseSnap()
	got2 := Diff(&cur, back)
	if len(got2) != 1 || got2[0].Key != "provider/recovered" {
		t.Fatalf("expected recovery alert, got %+v", got2)
	}
}

func TestUnresolvedSubmissionAndLock(t *testing.T) {
	prev := baseSnap()
	cur := baseSnap()
	cur.Unresolved = 1
	cur.Lock.ActiveLane = sql.NullString{String: "atlas_solver", Valid: true}
	got := Diff(&prev, cur)
	keys := map[string]bool{}
	for _, a := range got {
		keys[a.Key] = true
	}
	if !keys["unresolved"] || !keys["lock/acquired"] {
		t.Fatalf("expected unresolved+lock alerts, got %+v", got)
	}
}

func TestRealizedPnlMovementAlertsExactWeiOnly(t *testing.T) {
	prev := baseSnap()
	cur := baseSnap()
	cur.Windows["24H"] = opsstate.WindowStats{RealizedNetPnlWei: "-25000000000000"}
	cur.LastOutcomeN = prev.LastOutcomeN + 1
	got := Diff(&prev, cur)
	var pnl, outcome bool
	for _, a := range got {
		if a.Key == "pnl24h" && contains(a.Text, "-25000000000000") {
			pnl = true
		}
		if a.Key == "outcome" && contains(a.Text, "LOSS") {
			outcome = true
		}
	}
	if !pnl || !outcome {
		t.Fatalf("expected pnl24h+outcome(loss) alerts, got %+v", got)
	}
}

func TestStaleDataAlertsOnceNotEveryPoll(t *testing.T) {
	old := time.Now().UTC().Add(-45 * time.Minute)
	prev := baseSnap()
	prev.LastSignalAt = sql.NullTime{Time: old.Add(-time.Minute), Valid: true} // already stale
	cur := baseSnap()
	cur.LastSignalAt = sql.NullTime{Time: old, Valid: true}
	if got := Diff(&prev, cur); len(got) != 0 {
		t.Fatalf("stale must not re-alert while still stale, got %+v", got)
	}
	freshPrev := baseSnap()
	freshPrev.LastSignalAt = sql.NullTime{Time: time.Now().UTC().Add(-time.Minute), Valid: true}
	got2 := Diff(&freshPrev, cur)
	if len(got2) != 1 || got2[0].Key != "stale/signals" {
		t.Fatalf("expected one stale alert on fresh->stale transition, got %+v", got2)
	}
}

func TestReleaseMismatchAlertsWithExpectedConfigured(t *testing.T) {
	prev := baseSnap()
	cur := baseSnap()
	cur.ExpectedReleaseSHA = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	got := Diff(&prev, cur)
	if len(got) != 1 || got[0].Key != "release/mismatch" {
		t.Fatalf("expected release mismatch alert, got %+v", got)
	}
}

func contains(s, sub string) bool {
	return len(s) >= len(sub) && (s == sub || len(sub) == 0 || indexOf(s, sub) >= 0)
}

func indexOf(s, sub string) int {
	for i := 0; i+len(sub) <= len(s); i++ {
		if s[i:i+len(sub)] == sub {
			return i
		}
	}
	return -1
}
