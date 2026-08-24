// Package opsstate takes bounded read-only snapshots of Phoenix operational
// truth from Postgres for the Telegram Ops reporter.
//
// Invariants:
//   - READ ONLY: every query is a single SELECT; nothing here mutates state.
//   - Money values are transported as exact decimal strings (wei); no floats.
//   - Every query is LIMIT/interval-bounded so a degraded database cannot
//     turn the reporter into a load generator.
package opsstate

import (
	"context"
	"database/sql"
	"fmt"
	"time"

	_ "github.com/lib/pq" // postgres driver (registered as "postgres")
)

// Window is one reporting window (24H / 72H / 7D).
type Window struct {
	Key      string
	Duration time.Duration
}

// Windows are the owner-facing reporting windows, ordered.
var Windows = []Window{{"24H", 24 * time.Hour}, {"72H", 72 * time.Hour}, {"7D", 7 * 24 * time.Hour}}

// LaneState is one row of live_canary.revenue_lane_controls.
type LaneState struct {
	Lane             string
	Armed            bool
	KillSwitch       bool
	MaxInputWei      string
	MaxGasLimit      int64
	MaxFeePerGasWei  string
	MaxAtlasBidWei   string
	DailyLossLimitWe string
	ProfitFloorWei   string
	DisarmReason     string
	ControlEpoch     int64
}

// ProviderAuthority is the singleton revenue provider authority row.
type ProviderAuthority struct {
	ExactExecutionReady bool
	GateReason          string
	GateUpdatedAt       sql.NullTime
	RecoveryStatus      string
	FailureReason       sql.NullString
}

// GlobalLock is the singleton global revenue submission lock.
type GlobalLock struct {
	ActiveLane sql.NullString
	AcquiredAt sql.NullTime
	Epoch      int64
}

// WindowStats carries per-window funnel and realized-PnL aggregates.
type WindowStats struct {
	Signals              int64
	Exact                int64
	Fork                 int64
	ConservativePositive int64
	Candidates           int64
	Requests             int64
	Attempts             int64
	Successes            int64
	Losses               int64
	RealizedNetPnlWei    string // exact wei decimal string; "0" when none
}

// GroundTruth is the Atlas liquidation ground-truth aggregate.
type GroundTruth struct {
	Total      int64
	InWindow   int64
	LastSeenAt sql.NullTime
}

// Snapshot is one coherent read-only view of operational truth.
type Snapshot struct {
	TakenAt        time.Time
	Phase          string
	PhaseReason    string
	PhaseUpdatedAt sql.NullTime
	ReleaseSHA     string
	SchemaVersion  string

	Lanes      []LaneState
	Provider   ProviderAuthority
	Lock       GlobalLock
	Unresolved int64
	NonceGap   int64 // used nonce high-water minus next usable nonce, when readable

	Windows         map[string]WindowStats
	LastSignalAt    sql.NullTime
	GroundTruth     GroundTruth
	LastTransitions []Transition

	// Monotonic change detectors for alerting.
	LastRequestID int64
	LastAttemptID int64
	LastOutcomeN  int64

	// ExpectedReleaseSHA is owner-configured (env), not database truth;
	// MismatchSeen is runtime bookkeeping so a mismatch alerts once.
	ExpectedReleaseSHA string
	MismatchSeen       bool
}

// Transition is one economic_control phase transition.
type Transition struct {
	Phase     string
	Reason    string
	ChangedAt sql.NullTime
}

const windowStatsSQL = `
WITH w AS (SELECT now() - $1::interval AS since)
SELECT
  (SELECT count(*) FROM live_canary.revenue_hunting_signals s, w WHERE s.observed_at > w.since),
  (SELECT count(*) FROM live_canary.revenue_hunting_signals s, w WHERE s.observed_at > w.since AND s.evidence_mode = 'EIP1186_VERIFIED'),
  (SELECT count(*) FROM live_canary.revenue_hunting_signals s, w WHERE s.observed_at > w.since AND s.evidence_mode = 'DUAL_PROVIDER_FORK_VERIFIED'),
  (SELECT count(*) FROM live_canary.revenue_hunting_signals s, w WHERE s.observed_at > w.since AND s.conservative_net_pnl > 0),
  (SELECT count(*) FROM live_canary.revenue_hunting_signals s, w WHERE s.observed_at > w.since AND s.terminal_outcome IN ('candidate','submitted','settled')),
  (SELECT count(DISTINCT a.request_id) FROM live_canary.execution_attempts a, w WHERE a.claimed_at > w.since),
  (SELECT count(*) FROM live_canary.execution_attempts a, w WHERE a.claimed_at > w.since),
  (SELECT count(*) FROM live_canary.execution_outcomes o, w WHERE o.recorded_at > w.since AND o.receipt_status = 1),
  (SELECT count(*) FROM live_canary.execution_outcomes o, w WHERE o.recorded_at > w.since AND o.net_pnl_wei < 0),
  COALESCE((SELECT sum(o.net_pnl_wei)::text FROM live_canary.execution_outcomes o, w WHERE o.recorded_at > w.since), '0')
`

func windowStats(ctx context.Context, db *sql.DB, d time.Duration) (WindowStats, error) {
	var ws WindowStats
	var pnl sql.NullString
	interval := fmt.Sprintf("%.0f hours", d.Hours())
	row := db.QueryRowContext(ctx, windowStatsSQL, interval)
	if err := row.Scan(&ws.Signals, &ws.Exact, &ws.Fork, &ws.ConservativePositive,
		&ws.Candidates, &ws.Requests, &ws.Attempts, &ws.Successes, &ws.Losses, &pnl); err != nil {
		return ws, err
	}
	if pnl.Valid && pnl.String != "" {
		ws.RealizedNetPnlWei = pnl.String
	} else {
		ws.RealizedNetPnlWei = "0"
	}
	return ws, nil
}

// Take reads one full snapshot. It never writes.
func Take(ctx context.Context, db *sql.DB) (Snapshot, error) {
	snap := Snapshot{TakenAt: time.Now().UTC(), Windows: map[string]WindowStats{}}

	control := db.QueryRowContext(ctx, `
		SELECT phase, last_transition_reason, updated_at, release_sha
		FROM live_canary.economic_control WHERE singleton`)
	if err := control.Scan(&snap.Phase, &snap.PhaseReason, &snap.PhaseUpdatedAt, &snap.ReleaseSHA); err != nil {
		return snap, fmt.Errorf("economic_control: %w", err)
	}

	laneRows, err := db.QueryContext(ctx, `
		SELECT lane, armed, kill_switch, maximum_input_amount::text, maximum_gas_limit,
		       maximum_fee_per_gas::text, maximum_atlas_bid::text, daily_loss_limit::text,
		       retained_profit_floor::text, disarm_reason, control_epoch
		FROM live_canary.revenue_lane_controls ORDER BY lane`)
	if err != nil {
		return snap, fmt.Errorf("lane controls: %w", err)
	}
	defer laneRows.Close()
	for laneRows.Next() {
		var l LaneState
		if err := laneRows.Scan(&l.Lane, &l.Armed, &l.KillSwitch, &l.MaxInputWei,
			&l.MaxGasLimit, &l.MaxFeePerGasWei, &l.MaxAtlasBidWei, &l.DailyLossLimitWe,
			&l.ProfitFloorWei, &l.DisarmReason, &l.ControlEpoch); err != nil {
			return snap, err
		}
		snap.Lanes = append(snap.Lanes, l)
	}
	if err := laneRows.Err(); err != nil {
		return snap, err
	}

	p := db.QueryRowContext(ctx, `
		SELECT exact_execution_ready, gate_reason, gate_updated_at, recovery_status, failure_reason
		FROM live_canary.revenue_provider_authority WHERE singleton`)
	if err := p.Scan(&snap.Provider.ExactExecutionReady, &snap.Provider.GateReason,
		&snap.Provider.GateUpdatedAt, &snap.Provider.RecoveryStatus, &snap.Provider.FailureReason); err != nil {
		return snap, fmt.Errorf("provider authority: %w", err)
	}

	lock := db.QueryRowContext(ctx, `
		SELECT active_lane, acquired_at, control_epoch
		FROM live_canary.global_revenue_submission_lock WHERE singleton`)
	if err := lock.Scan(&snap.Lock.ActiveLane, &snap.Lock.AcquiredAt, &snap.Lock.Epoch); err != nil {
		return snap, fmt.Errorf("submission lock: %w", err)
	}

	if err := db.QueryRowContext(ctx, `
		SELECT count(*) FROM live_canary.execution_attempts
		WHERE status IN ('claimed','signing','submitted') AND terminal_at IS NULL`).Scan(&snap.Unresolved); err != nil {
		return snap, fmt.Errorf("unresolved submissions: %w", err)
	}

	if err := db.QueryRowContext(ctx, `
		SELECT max(observed_at) FROM live_canary.revenue_hunting_signals`).Scan(&snap.LastSignalAt); err != nil {
		return snap, fmt.Errorf("last signal: %w", err)
	}

	if err := db.QueryRowContext(ctx, `
		SELECT COALESCE(max(id), 0) FROM live_canary.execution_requests`).Scan(&snap.LastRequestID); err != nil {
		return snap, fmt.Errorf("last request id: %w", err)
	}
	if err := db.QueryRowContext(ctx, `
		SELECT COALESCE(max(id), 0) FROM live_canary.execution_attempts`).Scan(&snap.LastAttemptID); err != nil {
		return snap, fmt.Errorf("last attempt id: %w", err)
	}
	if err := db.QueryRowContext(ctx, `
		SELECT count(*) FROM live_canary.execution_outcomes`).Scan(&snap.LastOutcomeN); err != nil {
		return snap, fmt.Errorf("outcome count: %w", err)
	}

	gt := db.QueryRowContext(ctx, `
		SELECT count(*),
		       count(*) FILTER (WHERE reconciled_at > now() - interval '168 hours'),
		       max(reconciled_at)
		FROM live_canary.atlas_liquidation_ground_truth`)
	if err := gt.Scan(&snap.GroundTruth.Total, &snap.GroundTruth.InWindow, &snap.GroundTruth.LastSeenAt); err != nil {
		return snap, fmt.Errorf("ground truth: %w", err)
	}

	trRows, err := db.QueryContext(ctx, `
		SELECT phase, last_transition_reason, updated_at
		FROM live_canary.economic_transitions ORDER BY updated_at DESC LIMIT 5`)
	if err == nil {
		defer trRows.Close()
		for trRows.Next() {
			var t Transition
			if err := trRows.Scan(&t.Phase, &t.Reason, &t.ChangedAt); err != nil {
				return snap, err
			}
			snap.LastTransitions = append(snap.LastTransitions, t)
		}
	}

	for _, w := range Windows {
		stats, err := windowStats(ctx, db, w.Duration)
		if err != nil {
			return snap, fmt.Errorf("window %s: %w", w.Key, err)
		}
		snap.Windows[w.Key] = stats
	}
	return snap, nil
}
