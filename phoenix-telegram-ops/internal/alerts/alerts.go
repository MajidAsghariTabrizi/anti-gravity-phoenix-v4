// Package alerts derives owner push alerts by comparing consecutive
// read-only snapshots. Alerting is pure here: main owns the loop.
package alerts

import (
	"fmt"
	"time"

	"anti-gravity-phoenix-v4/phoenix-telegram-ops/internal/opsstate"
)

// Alert is one owner-facing push message.
type Alert struct {
	Key  string // stable identity for dedup/logging
	Text string
}

// StaleSignalAfter is how long a silent hunting feed raises STALE_DATA.
var StaleSignalAfter = 30 * time.Minute

// Diff compares previous to current and returns the alerts to push.
// A nil prev yields no alerts (first snapshot never spams).
func Diff(prev *opsstate.Snapshot, cur opsstate.Snapshot) []Alert {
	if prev == nil {
		return nil
	}
	var out []Alert
	prevLane := map[string]opsstate.LaneState{}
	for _, l := range prev.Lanes {
		prevLane[l.Lane] = l
	}
	for _, l := range cur.Lanes {
		p, ok := prevLane[l.Lane]
		if !ok {
			continue
		}
		if p.Armed != l.Armed {
			state := "DISARMED"
			if l.Armed {
				state = "ARMED"
			}
			out = append(out, Alert{Key: "arm/" + l.Lane,
				Text: fmt.Sprintf("ARM/DISARM: %s -> %s (reason=%s)", l.Lane, state, l.DisarmReason)})
		}
		if p.KillSwitch != l.KillSwitch {
			text := fmt.Sprintf("KILL SWITCH: %s kill_switch=%v", l.Lane, l.KillSwitch)
			if l.KillSwitch {
				text = fmt.Sprintf("KILL SWITCH ENGAGED: %s (reason=%s)", l.Lane, l.DisarmReason)
			} else {
				text = fmt.Sprintf("KILL SWITCH RELEASED: %s", l.Lane)
			}
			out = append(out, Alert{Key: "kill/" + l.Lane, Text: text})
		}
	}

	if cur.Unresolved > 0 && prev.Unresolved == 0 {
		out = append(out, Alert{Key: "unresolved",
			Text: fmt.Sprintf("UNRESOLVED SUBMISSION(S): %d attempt(s) stuck non-terminal", cur.Unresolved)})
	}

	if prev.Provider.ExactExecutionReady && !cur.Provider.ExactExecutionReady {
		out = append(out, Alert{Key: "provider/degraded",
			Text: fmt.Sprintf("PROVIDER DEGRADATION: authority gate closed (%s)", cur.Provider.GateReason)})
	}
	if !prev.Provider.ExactExecutionReady && cur.Provider.ExactExecutionReady {
		out = append(out, Alert{Key: "provider/recovered",
			Text: "PROVIDER RECOVERED: authority gate ready again"})
	}

	if prev.Lock.ActiveLane.String != cur.Lock.ActiveLane.String {
		if cur.Lock.ActiveLane.Valid && cur.Lock.ActiveLane.String != "" {
			out = append(out, Alert{Key: "lock/acquired",
				Text: "EXECUTION LOCK ACQUIRED by " + cur.Lock.ActiveLane.String})
		} else {
			out = append(out, Alert{Key: "lock/released",
				Text: "EXECUTION LOCK RELEASED"})
		}
	}

	// New reconciled outcomes since the previous snapshot.
	if prev.LastOutcomeN != cur.LastOutcomeN {
		text := fmt.Sprintf("RECEIPT/RECONCILIATION recorded (24H realized net: %s)",
			cur.Windows["24H"].RealizedNetPnlWei)
		if isNegativeWei(cur.Windows["24H"].RealizedNetPnlWei) {
			text += " — LOSS territory"
		}
		out = append(out, Alert{Key: "outcome", Text: text})
	}

	// New submission attempts.
	if prev.LastAttemptID != cur.LastAttemptID {
		out = append(out, Alert{Key: "submission",
			Text: "SUBMISSION: new execution attempt recorded"})
	}

	// New execution requests.
	if prev.LastRequestID != cur.LastRequestID {
		out = append(out, Alert{Key: "request",
			Text: "EXECUTION REQUEST created"})
	}

	// Surface a distinct REALIZED PNL alert when the 24H sum moved.
	if prev.Windows["24H"].RealizedNetPnlWei != cur.Windows["24H"].RealizedNetPnlWei {
		out = append(out, Alert{Key: "pnl24h",
			Text: "REALIZED PNL (24H): " + cur.Windows["24H"].RealizedNetPnlWei + " wei"})
	}

	// Stale data: signals stopped flowing for too long.
	if cur.LastSignalAt.Valid {
		if time.Since(cur.LastSignalAt.Time) > StaleSignalAfter &&
			!prevStale(prev) {
			out = append(out, Alert{Key: "stale/signals",
				Text: fmt.Sprintf("STALE DATA: no hunting signals for >%s (last %s)",
					StaleSignalAfter, cur.LastSignalAt.Time.UTC().Format(time.RFC3339))})
		}
	}

	// Release mismatch against the expected SHA, when configured.
	if cur.ExpectedReleaseSHA != "" && cur.ReleaseSHA != "" &&
		cur.ExpectedReleaseSHA != cur.ReleaseSHA && prev.MismatchSeen != true {
		out = append(out, Alert{Key: "release/mismatch",
			Text: "RELEASE MISMATCH: running " + short(cur.ReleaseSHA) +
				" expected " + short(cur.ExpectedReleaseSHA)})
	}
	return out
}

func prevStale(p *opsstate.Snapshot) bool {
	return p.LastSignalAt.Valid && time.Since(p.LastSignalAt.Time) > StaleSignalAfter
}

func short(sha string) string {
	if len(sha) > 10 {
		return sha[:10]
	}
	return sha
}

func isNegativeWei(wei string) bool {
	if len(wei) > 0 && wei[0] == '-' {
		return true
	}
	for _, c := range wei {
		if c != '0' {
			return false
		}
	}
	return false
}
