// Package panels renders Telegram Ops panels from read-only snapshots.
//
// Panels are pure: they take a snapshot plus a window and return text.
// Only reconciled Realized Net PnL is ever labelled as PnL. Expected,
// conservative, or shadow values are labelled as such and never presented
// as realized profit.
package panels

import (
	"database/sql"
	"fmt"
	"math/big"
	"strings"
	"time"

	"anti-gravity-phoenix-v4/phoenix-telegram-ops/internal/opsstate"
)

// PanelKey identifies a renderable panel (Telegram callback data).
type PanelKey string

const (
	KeyHome        PanelKey = "home"
	KeySystem      PanelKey = "system"
	KeyPnl         PanelKey = "pnl"
	KeyFunnel      PanelKey = "funnel"
	KeyLanes       PanelKey = "lanes"
	KeyProviders   PanelKey = "providers"
	KeyGroundTruth PanelKey = "ground_truth"
	KeyIncidents   PanelKey = "incidents"
)

// KeyboardRows is the fixed owner-facing button layout.
var KeyboardRows = [][]PanelKey{
	{KeySystem, KeyPnl},
	{KeyFunnel, KeyLanes},
	{KeyProviders, KeyGroundTruth},
	{KeyIncidents, KeyHome},
}

// WindowKeys are the selectable reporting windows.
var WindowKeys = []string{"24H", "72H", "7D"}

func mode(s opsstate.Snapshot) string {
	live := false
	for _, l := range s.Lanes {
		if l.Armed && !l.KillSwitch {
			live = true
		}
	}
	if live {
		return "LIVE/ARMED"
	}
	return "DISARMED"
}

// FormatWei renders an exact wei decimal string as a decimal ETH amount
// using integer arithmetic only (no floats).
func FormatWei(wei string) string {
	v, ok := new(big.Int).SetString(strings.TrimSpace(wei), 10)
	if !ok {
		return wei + " wei"
	}
	neg := v.Sign() < 0
	if neg {
		v = new(big.Int).Neg(v)
	}
	s := v.String()
	intPart := s
	frac := ""
	if len(s) > 18 {
		intPart = s[:len(s)-18]
		frac = s[len(s)-18:]
	} else {
		frac = strings.Repeat("0", 18-len(s)) + s
		intPart = "0"
	}
	frac = strings.TrimRight(frac, "0")
	out := intPart
	if frac != "" {
		out += "." + frac
	}
	if neg {
		out = "-" + out
	}
	return out + " ETH"
}

func fmtNullTime(nt sql.NullTime) string {
	if !nt.Valid {
		return "never"
	}
	return nt.Time.UTC().Format("2006-01-02 15:04:05Z")
}

func laneLine(l opsstate.LaneState) string {
	state := "DISARMED"
	if l.Armed && !l.KillSwitch {
		state = "LIVE/ARMED"
	} else if l.KillSwitch {
		state = "KILL/SAFE"
	}
	return fmt.Sprintf("%s: %s (epoch %d, reason=%s)", l.Lane, state, l.ControlEpoch, l.DisarmReason)
}

func caps(l opsstate.LaneState) string {
	return fmt.Sprintf("cap_in=%s gas=%d feepg=%s bid=%s loss_limit=%s floor=%s",
		l.MaxInputWei, l.MaxGasLimit, l.MaxFeePerGasWei, l.MaxAtlasBidWei,
		l.DailyLossLimitWe, l.ProfitFloorWei)
}

func header(title string, s opsstate.Snapshot) string {
	return fmt.Sprintf("PHOENIX OPS | %s\nmode: %s | phase: %s\nupdated: %s\n",
		title, mode(s), s.Phase, s.TakenAt.Format("2006-01-02 15:04:05Z"))
}

// Render renders the panel identified by key for the given window.
func Render(key PanelKey, window string, s opsstate.Snapshot) string {
	switch key {
	case KeySystem:
		return system(s)
	case KeyPnl:
		return pnl(window, s)
	case KeyFunnel:
		return funnel(window, s)
	case KeyLanes:
		return lanes(s)
	case KeyProviders:
		return providers(s)
	case KeyGroundTruth:
		return groundTruth(s)
	case KeyIncidents:
		return incidents(s)
	default:
		return home(s)
	}
}

func home(s opsstate.Snapshot) string {
	var b strings.Builder
	b.WriteString(header("HOME", s))
	ws := s.Windows["24H"]
	fmt.Fprintf(&b, "Realized Net PnL (24H): %s\n", FormatWei(ws.RealizedNetPnlWei))
	fmt.Fprintf(&b, "provider gate: ready=%v (%s)\n", s.Provider.ExactExecutionReady, s.Provider.GateReason)
	fmt.Fprintf(&b, "signals 24H: %d | unresolved submissions: %d\n", ws.Signals, s.Unresolved)
	for _, l := range s.Lanes {
		b.WriteString(laneLine(l) + "\n")
	}
	b.WriteString("\n[ SYSTEM ][ PNL ]\n[ FUNNEL ][ LANES ]\n[ PROVIDERS ][ GROUND TRUTH ]\n[ INCIDENTS ][ REFRESH ]")
	return b.String()
}

func system(s opsstate.Snapshot) string {
	var b strings.Builder
	b.WriteString(header("SYSTEM", s))
	fmt.Fprintf(&b, "release SHA: %s\nphase since: %s\nphase reason: %s\nrecent transitions:\n",
		s.ReleaseSHA, fmtNullTime(s.PhaseUpdatedAt), s.PhaseReason)
	for _, t := range s.LastTransitions {
		fmt.Fprintf(&b, "  %s -> %s (%s)\n", t.Reason, t.Phase, fmtNullTime(t.ChangedAt))
	}
	return b.String()
}

func pnl(window string, s opsstate.Snapshot) string {
	var b strings.Builder
	b.WriteString(header("PNL ("+window+")", s))
	ws := s.Windows[window]
	fmt.Fprintf(&b, "Realized Net PnL: %s\nsuccesses: %d | losses: %d\n",
		FormatWei(ws.RealizedNetPnlWei), ws.Successes, ws.Losses)
	b.WriteString("(reconciled execution outcomes only; expected/conservative values are never shown here)")
	return b.String()
}

func funnel(window string, s opsstate.Snapshot) string {
	var b strings.Builder
	b.WriteString(header("FUNNEL ("+window+")", s))
	ws := s.Windows[window]
	fmt.Fprintf(&b, "signals: %d\nexact: %d\nfork: %d\nconservative positive: %d\neligible/candidates: %d\nrequests: %d\nattempts: %d\nsuccesses: %d\n",
		ws.Signals, ws.Exact, ws.Fork, ws.ConservativePositive, ws.Candidates,
		ws.Requests, ws.Attempts, ws.Successes)
	return b.String()
}

func lanes(s opsstate.Snapshot) string {
	var b strings.Builder
	b.WriteString(header("LANES", s))
	for _, l := range s.Lanes {
		fmt.Fprintf(&b, "%s\n  %s\n", laneLine(l), caps(l))
	}
	b.WriteString("\ngeneric dex policy: CLOSED unless separately authorized")
	lock := "free"
	if s.Lock.ActiveLane.Valid && s.Lock.ActiveLane.String != "" {
		lock = "held by " + s.Lock.ActiveLane.String
	}
	fmt.Fprintf(&b, "\nsubmission lock: %s (epoch %d)", lock, s.Lock.Epoch)
	return b.String()
}

func providers(s opsstate.Snapshot) string {
	var b strings.Builder
	b.WriteString(header("PROVIDERS", s))
	fmt.Fprintf(&b, "authority gate: exact_execution_ready=%v\nreason: %s\nupdated: %s\nrecovery: %s\n",
		s.Provider.ExactExecutionReady, s.Provider.GateReason,
		fmtNullTime(s.Provider.GateUpdatedAt), s.Provider.RecoveryStatus)
	if s.Provider.FailureReason.Valid && s.Provider.FailureReason.String != "" {
		fmt.Fprintf(&b, "failure: %s\n", s.Provider.FailureReason.String)
	}
	b.WriteString("roles: NOWNodes=HOT_EXECUTION_PRIMARY; Alchemy=ARCHIVE_GROUND_TRUTH/EXACT_SECONDARY when configured; PublicNode=OBSERVABILITY_ONLY; arb1 official=TERTIARY_OBSERVER")
	return b.String()
}

func groundTruth(s opsstate.Snapshot) string {
	var b strings.Builder
	b.WriteString(header("GROUND TRUTH", s))
	fmt.Fprintf(&b, "liquidations recorded: %d (7D: %d)\nlast reconciled: %s\n",
		s.GroundTruth.Total, s.GroundTruth.InWindow, fmtNullTime(s.GroundTruth.LastSeenAt))
	return b.String()
}

func incidents(s opsstate.Snapshot) string {
	var b strings.Builder
	b.WriteString(header("INCIDENTS", s))
	found := false
	if s.Unresolved > 0 {
		fmt.Fprintf(&b, "! unresolved submissions: %d\n", s.Unresolved)
		found = true
	}
	if s.Provider.FailureReason.Valid && s.Provider.FailureReason.String != "" {
		fmt.Fprintf(&b, "! provider failure: %s\n", s.Provider.FailureReason.String)
		found = true
	}
	if !s.Provider.ExactExecutionReady {
		fmt.Fprintf(&b, "! provider gate closed: %s\n", s.Provider.GateReason)
		found = true
	}
	if s.LastSignalAt.Valid && time.Since(s.LastSignalAt.Time) > 30*time.Minute {
		fmt.Fprintf(&b, "! stale signals: last %s\n", fmtNullTime(s.LastSignalAt))
		found = true
	}
	if !found {
		b.WriteString("no open incidents derived from database truth")
	}
	return b.String()
}
