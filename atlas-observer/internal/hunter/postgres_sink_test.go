package hunter

import (
	"context"
	"errors"
	"fmt"
	"testing"
	"time"

	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
)

func TestAdvancePrimaryProviderSamplesIgnoresOutOfOrderCompletion(t *testing.T) {
	base := time.Date(2026, time.August, 14, 11, 41, 0, 0, time.UTC)
	samples := []providerRecoverySample{
		{at: base.Add(time.Second), primary: primaryProviderID},
		{at: base.Add(2 * time.Second), primary: primaryProviderID},
		{at: base.Add(3 * time.Second), primary: primaryProviderID},
	}

	unchanged, advanced := advancePrimaryProviderSamples(
		samples,
		"ready",
		base.Add(2500*time.Millisecond),
		primaryProviderID,
	)
	if advanced {
		t.Fatal("out-of-order completion advanced provider recovery evidence")
	}
	if len(unchanged) != 3 || !unchanged[2].at.Equal(base.Add(3*time.Second)) {
		t.Fatalf("out-of-order completion changed durable samples: %+v", unchanged)
	}

	advancedSamples, advanced := advancePrimaryProviderSamples(
		unchanged,
		"ready",
		base.Add(4*time.Second),
		primaryProviderID,
	)
	if !advanced {
		t.Fatal("newer completion did not advance provider recovery evidence")
	}
	if len(advancedSamples) != 3 ||
		!advancedSamples[0].at.Equal(base.Add(2*time.Second)) ||
		!advancedSamples[1].at.Equal(base.Add(3*time.Second)) ||
		!advancedSamples[2].at.Equal(base.Add(4*time.Second)) {
		t.Fatalf("newer completion did not preserve a strict rolling window: %+v", advancedSamples)
	}
}

func TestAdvancePrimaryProviderSamplesDoesNotRegressCollectingWindow(t *testing.T) {
	base := time.Date(2026, time.August, 14, 11, 41, 0, 0, time.UTC)
	samples := []providerRecoverySample{{at: base.Add(2 * time.Second), primary: primaryProviderID}}

	unchanged, advanced := advancePrimaryProviderSamples(
		samples,
		"collecting",
		base.Add(time.Second),
		primaryProviderID,
	)
	if advanced || len(unchanged) != 1 || !unchanged[0].at.Equal(base.Add(2*time.Second)) {
		t.Fatalf("collecting window regressed to an older completion: advanced=%v samples=%+v", advanced, unchanged)
	}
}

// fakeProviderAuthorityRow mimics the revenue_provider_authority singleton row
// for deterministic evidence-floor and recovery-sample accounting tests.
type fakeProviderAuthorityRow struct {
	values []any
	err    error
}

func (r fakeProviderAuthorityRow) Scan(dest ...any) error {
	if r.err != nil {
		return r.err
	}
	for index := range dest {
		if index >= len(r.values) {
			return nil
		}
		value := r.values[index]
		switch target := dest[index].(type) {
		case *string:
			if value == nil {
				*target = ""
			} else {
				*target = value.(string)
			}
		case **string:
			if value == nil {
				*target = nil
			} else {
				text := value.(string)
				*target = &text
			}
		case *time.Time:
			if value == nil {
				*target = time.Time{}
			} else {
				*target = value.(time.Time)
			}
		case **time.Time:
			if value == nil {
				*target = nil
			} else {
				instant := value.(time.Time)
				*target = &instant
			}
		case *int16:
			*target = value.(int16)
		default:
			return fmt.Errorf("unexpected scan target %T", dest[index])
		}
	}
	return nil
}

type fakeProviderAuthorityExec struct {
	sql  string
	args []any
}

type fakeProviderAuthorityDB struct {
	row          fakeProviderAuthorityRow
	execs        []fakeProviderAuthorityExec
	execErr      error
	rowsAffected int64
}

func (f *fakeProviderAuthorityDB) QueryRow(_ context.Context, _ string, _ ...any) pgx.Row {
	return f.row
}

func (f *fakeProviderAuthorityDB) Exec(_ context.Context, sql string, args ...any) (pgconn.CommandTag, error) {
	if f.execErr != nil {
		return pgconn.CommandTag{}, f.execErr
	}
	f.execs = append(f.execs, fakeProviderAuthorityExec{sql: sql, args: args})
	return pgconn.NewCommandTag(fmt.Sprintf("UPDATE %d", f.rowsAffected)), nil
}

// fakeAuthorityRow builds a scan-ordered row:
// failure_transition_at, recovery_status, request_evidence_not_before,
// sample_count, sample_1_at..sample_3_at, sample_1_primary..sample_3_primary.
func fakeAuthorityRow(requestEvidenceNotBefore time.Time, failureTransition *time.Time, recoveryStatus string, samples ...providerRecoverySample) fakeProviderAuthorityRow {
	values := make([]any, 10)
	if failureTransition == nil {
		values[0] = nil
	} else {
		values[0] = *failureTransition
	}
	values[1] = recoveryStatus
	values[2] = requestEvidenceNotBefore
	values[3] = int16(len(samples))
	for index, sample := range samples {
		if index >= 3 {
			break
		}
		values[4+2*index] = sample.at
		values[5+2*index] = sample.primary
	}
	return fakeProviderAuthorityRow{values: values}
}

func TestRecordPrimaryProviderSuccessStaleEvidenceClassification(t *testing.T) {
	floor := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	transition := floor.Add(30 * time.Second)
	cases := []struct {
		name              string
		observedAt        time.Time
		failureTransition *time.Time
		requestNotBefore  time.Time
		wantStale         bool
	}{
		{name: "observation before floor", observedAt: floor.Add(-time.Second), requestNotBefore: floor, wantStale: true},
		{name: "observation exactly equal to floor", observedAt: floor, requestNotBefore: floor, wantStale: true},
		{name: "observation after floor", observedAt: floor.Add(time.Second), requestNotBefore: floor, wantStale: false},
		{name: "observation before failure transition", observedAt: transition.Add(-time.Second), failureTransition: &transition, requestNotBefore: floor, wantStale: true},
		{name: "observation exactly equal to failure transition", observedAt: transition, failureTransition: &transition, requestNotBefore: floor, wantStale: true},
		{name: "observation after failure transition", observedAt: transition.Add(time.Second), failureTransition: &transition, requestNotBefore: floor, wantStale: false},
	}
	for _, testCase := range cases {
		t.Run(testCase.name, func(t *testing.T) {
			db := &fakeProviderAuthorityDB{
				row:          fakeAuthorityRow(testCase.requestNotBefore, testCase.failureTransition, "collecting"),
				rowsAffected: 1,
			}
			err := recordPrimaryProviderSuccess(context.Background(), db, testCase.observedAt, primaryProviderID)
			if got := errors.Is(err, errProviderEvidenceStale); got != testCase.wantStale {
				t.Fatalf("stale classification: want=%v got=%v err=%v", testCase.wantStale, got, err)
			}
			if testCase.wantStale && len(db.execs) != 0 {
				t.Fatalf("stale evidence wrote durable state: %+v", db.execs)
			}
			if !testCase.wantStale && err != nil {
				t.Fatalf("fresh evidence failed: %v", err)
			}
		})
	}
}

func TestRecordPrimaryProviderSuccessRecoveryTransitionWritesReady(t *testing.T) {
	floor := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	first := floor.Add(10 * time.Second)
	second := floor.Add(20 * time.Second)
	third := floor.Add(30 * time.Second)
	db := &fakeProviderAuthorityDB{
		row: fakeAuthorityRow(floor, nil, "collecting",
			providerRecoverySample{at: first, primary: primaryProviderID},
			providerRecoverySample{at: second, primary: primaryProviderID}),
		rowsAffected: 1,
	}
	if err := recordPrimaryProviderSuccess(context.Background(), db, third, primaryProviderID); err != nil {
		t.Fatalf("recovery transition failed: %v", err)
	}
	if len(db.execs) != 1 {
		t.Fatalf("expected one durable write, got %d", len(db.execs))
	}
	args := db.execs[0].args
	if len(args) != 13 {
		t.Fatalf("unexpected durable write arity: %d", len(args))
	}
	if args[1] != "ready" || args[2] != 3 || args[12] != true {
		t.Fatalf("ready transition drifted: status=%v count=%v ready=%v", args[1], args[2], args[12])
	}
}

func TestAdvancePrimaryProviderSamplesWindowResetWhileRecovering(t *testing.T) {
	base := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	samples := []providerRecoverySample{{at: base, primary: primaryProviderID}}
	advanced, ok := advancePrimaryProviderSamples(samples, "recovering", base.Add(time.Minute), primaryProviderID)
	if !ok || len(advanced) != 2 || !advanced[1].at.Equal(base.Add(time.Minute)) {
		t.Fatalf("in-window recovery sample did not advance: ok=%v samples=%+v", ok, advanced)
	}
	reset, ok := advancePrimaryProviderSamples(samples, "recovering", base.Add(3*time.Minute), primaryProviderID)
	if !ok || len(reset) != 1 || !reset[0].at.Equal(base.Add(3*time.Minute)) {
		t.Fatalf("out-of-window recovery samples were not reset: ok=%v samples=%+v", ok, reset)
	}
	kept, ok := advancePrimaryProviderSamples(samples, "ready", base.Add(3*time.Minute), primaryProviderID)
	if !ok || len(kept) != 2 {
		t.Fatalf("ready-state samples must not reset: ok=%v samples=%+v", ok, kept)
	}
}

func TestRecordPrimaryProviderSuccessIdentityMismatchRemainsFatal(t *testing.T) {
	floor := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	first := floor.Add(10 * time.Second)
	second := floor.Add(20 * time.Second)
	third := floor.Add(30 * time.Second)
	db := &fakeProviderAuthorityDB{
		row: fakeAuthorityRow(floor, nil, "collecting",
			providerRecoverySample{at: first, primary: primaryProviderID},
			providerRecoverySample{at: second, primary: "other-provider"}),
		rowsAffected: 1,
	}
	err := recordPrimaryProviderSuccess(context.Background(), db, third, primaryProviderID)
	if err == nil || errors.Is(err, errProviderEvidenceStale) {
		t.Fatalf("identity mismatch must remain a fatal integrity failure: %v", err)
	}
	if len(db.execs) != 0 {
		t.Fatalf("identity mismatch wrote durable state: %+v", db.execs)
	}
}

func TestRecordPrimaryProviderSuccessMalformedStateRemainsFatal(t *testing.T) {
	floor := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	db := &fakeProviderAuthorityDB{
		row:          fakeAuthorityRow(floor, nil, "collecting"),
		rowsAffected: 1,
	}
	db.row.values[3] = int16(7)
	err := recordPrimaryProviderSuccess(context.Background(), db, floor.Add(time.Second), primaryProviderID)
	if err == nil || errors.Is(err, errProviderEvidenceStale) {
		t.Fatalf("malformed recovery state must remain a fatal integrity failure: %v", err)
	}
	if len(db.execs) != 0 {
		t.Fatalf("malformed state wrote durable state: %+v", db.execs)
	}
}

func TestRecordPrimaryProviderSuccessUnavailableStateRemainsFatal(t *testing.T) {
	floor := time.Date(2026, 8, 21, 12, 0, 0, 0, time.UTC)
	db := &fakeProviderAuthorityDB{
		row:          fakeProviderAuthorityRow{err: errors.New("database unavailable")},
		rowsAffected: 1,
	}
	err := recordPrimaryProviderSuccess(context.Background(), db, floor.Add(time.Second), primaryProviderID)
	if err == nil || errors.Is(err, errProviderEvidenceStale) {
		t.Fatalf("unavailable durable state must remain a fatal integrity failure: %v", err)
	}
}

func TestApplyStaleProviderEvidenceRejection(t *testing.T) {
	candidate := &executionCandidate{ApprovalDigest: "approval"}
	cases := []struct {
		name   string
		record signal
		check  func(t *testing.T, record signal)
	}{
		{
			name: "candidate authority stripped",
			record: signal{
				TerminalOutcome:    "exact_pending",
				Authority:          true,
				ExecutionCandidate: candidate,
			},
			check: func(t *testing.T, record signal) {
				if record.Authority || record.ExecutionCandidate != nil || record.AtlasCandidate != nil {
					t.Fatalf("stale evidence kept candidate authority: %+v", record)
				}
				if record.TerminalOutcome != "exact_pending" || record.ExactDeferredReason != providerEvidenceStaleClass {
					t.Fatalf("stale evidence did not receive the precise terminal reason: %+v", record)
				}
			},
		},
		{
			name: "economic rejection preserved",
			record: signal{
				TerminalOutcome:          "economic_rejection",
				AuthorityRejectionReason: "gross_edge_below_retained_profit_gate",
			},
			check: func(t *testing.T, record signal) {
				if record.TerminalOutcome != "economic_rejection" ||
					record.AuthorityRejectionReason != "gross_edge_below_retained_profit_gate" ||
					record.ExactDeferredReason != "" {
					t.Fatalf("economic rejection observation drifted: %+v", record)
				}
			},
		},
		{
			name:   "pending observation gets precise reason",
			record: signal{TerminalOutcome: "exact_pending"},
			check: func(t *testing.T, record signal) {
				if record.TerminalOutcome != "exact_pending" || record.ExactDeferredReason != providerEvidenceStaleClass {
					t.Fatalf("pending observation did not receive the precise terminal reason: %+v", record)
				}
			},
		},
		{
			name:   "fork pending observation gets precise reason",
			record: signal{TerminalOutcome: "fork_pending"},
			check: func(t *testing.T, record signal) {
				if record.TerminalOutcome != "fork_pending" || record.ExactDeferredReason != providerEvidenceStaleClass {
					t.Fatalf("fork pending observation did not receive the precise terminal reason: %+v", record)
				}
			},
		},
		{
			name: "existing deferred reason preserved",
			record: signal{
				TerminalOutcome:     "exact_pending",
				ExactDeferredReason: "scheduler_capacity",
			},
			check: func(t *testing.T, record signal) {
				if record.TerminalOutcome != "exact_pending" || record.ExactDeferredReason != "scheduler_capacity" {
					t.Fatalf("existing deferred reason was overwritten: %+v", record)
				}
			},
		},
	}
	for _, testCase := range cases {
		t.Run(testCase.name, func(t *testing.T) {
			testCase.check(t, applyStaleProviderEvidenceRejection(testCase.record))
		})
	}
}
