package panels

import (
	"strings"
	"testing"
	"time"

	"anti-gravity-phoenix-v4/phoenix-telegram-ops/internal/opsstate"

	"database/sql"
)

func sampleSnapshot() opsstate.Snapshot {
	now := time.Date(2026, 8, 24, 12, 0, 0, 0, time.UTC)
	s := opsstate.Snapshot{
		TakenAt:     now,
		Phase:       "DISARMED_EVIDENCE",
		PhaseReason: "disarmed_evidence_started",
		ReleaseSHA:  "8ac529a9af7caeacb8883a51024e5970dae6f281",
		Lanes: []opsstate.LaneState{
			{Lane: "aave_liquidation", Armed: false, KillSwitch: true,
				MaxInputWei: "10000000000000000", MaxGasLimit: 600000,
				MaxFeePerGasWei: "50236000", MaxAtlasBidWei: "1000000000000000",
				DailyLossLimitWe: "600000000000000", ProfitFloorWei: "1000000000000",
				DisarmReason: "disarmed_deploy", ControlEpoch: 139},
			{Lane: "phoenix_dex", Armed: false, KillSwitch: false,
				MaxInputWei: "10000000000000", DisarmReason: "not_armed"},
		},
		Provider: opsstate.ProviderAuthority{ExactExecutionReady: true, GateReason: "ok", RecoveryStatus: "idle"},
		Windows:  map[string]opsstate.WindowStats{},
	}
	s.Windows["24H"] = opsstate.WindowStats{
		Signals: 42, Exact: 7, Fork: 5, ConservativePositive: 3, Candidates: 2,
		Requests: 1, Attempts: 1, Successes: 1, Losses: 0,
		RealizedNetPnlWei: "-25000000000000",
	}
	return s
}

func TestFormatWeiIntegerOnly(t *testing.T) {
	cases := []struct{ in, want string }{
		{"0", "0 ETH"},
		{"1000000000000000000", "1 ETH"},
		{"-25000000000000", "-0.000025 ETH"},
		{"1500000000000", "0.0000015 ETH"},
		{"123456789123456789123456789", "123456789.123456789123456789 ETH"},
		{"junk", "junk wei"},
	}
	for _, c := range cases {
		if got := FormatWei(c.in); got != c.want {
			t.Fatalf("FormatWei(%q) = %q want %q", c.in, got, c.want)
		}
	}
}

func TestModeReflectsLaneTruth(t *testing.T) {
	s := sampleSnapshot()
	if mode(s) != "DISARMED" {
		t.Fatalf("expected DISARMED got %s", mode(s))
	}
	s.Lanes[0].Armed = true
	s.Lanes[0].KillSwitch = false
	if mode(s) != "LIVE/ARMED" {
		t.Fatalf("expected LIVE/ARMED got %s", mode(s))
	}
}

func TestHomeShowsRealizedOnlyAndGenericDexClosed(t *testing.T) {
	out := Render(KeyHome, "24H", sampleSnapshot())
	if !strings.Contains(out, "Realized Net PnL (24H): -0.000025 ETH") {
		t.Fatalf("home must show realized pnl, got:\n%s", out)
	}
	if strings.Contains(out, "GENERIC") && strings.Contains(out, "OPEN") {
		t.Fatalf("generic dex must never read open")
	}
	if !strings.Contains(out, "REFRESH") {
		t.Fatalf("keyboard hint missing")
	}
}

func TestPnlPanelLabelsReconciliationSource(t *testing.T) {
	out := Render(KeyPnl, "72H", sampleSnapshot())
	if !strings.Contains(out, "reconciled execution outcomes only") {
		t.Fatalf("pnl panel must state reconciliation-only basis")
	}
	ws := sampleSnapshot().Windows["24H"]
	_ = ws
}

func TestFunnelPanelFields(t *testing.T) {
	out := Render(KeyFunnel, "24H", sampleSnapshot())
	for _, want := range []string{"signals: 42", "exact: 7", "fork: 5",
		"eligible/candidates: 2", "requests: 1", "attempts: 1", "successes: 1"} {
		if !strings.Contains(out, want) {
			t.Fatalf("funnel missing %q in:\n%s", want, out)
		}
	}
}

func TestIncidentsFlagsGateClosedAndStale(t *testing.T) {
	s := sampleSnapshot()
	s.Provider.ExactExecutionReady = false
	s.Provider.GateReason = "provider_unhealthy"
	old := time.Now().UTC().Add(-2 * time.Hour)
	s.LastSignalAt = sql.NullTime{Time: old, Valid: true}
	out := Render(KeyIncidents, "24H", s)
	if !strings.Contains(out, "! provider gate closed: provider_unhealthy") ||
		!strings.Contains(out, "! stale signals") {
		t.Fatalf("incidents missing expected flags:\n%s", out)
	}
}

func TestKeyboardRowsComplete(t *testing.T) {
	seen := map[PanelKey]bool{}
	for _, row := range KeyboardRows {
		for _, k := range row {
			seen[k] = true
		}
	}
	for _, k := range []PanelKey{KeySystem, KeyPnl, KeyFunnel, KeyLanes,
		KeyProviders, KeyGroundTruth, KeyIncidents, KeyHome} {
		if !seen[k] {
			t.Fatalf("panel %s missing from keyboard", k)
		}
	}
}
