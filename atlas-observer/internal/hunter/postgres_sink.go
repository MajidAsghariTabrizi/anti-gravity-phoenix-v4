package hunter

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strings"
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
	"github.com/jackc/pgx/v5"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"
)

type PostgresSignalSink struct {
	pool          *pgxpool.Pool
	floor         string
	atlasAuctions chan *observer.LedgerRecord
}

func requireAcceptedCandidateSignalWrite(rowsAffected int64, record signal) error {
	if rowsAffected != 1 && (record.ExecutionCandidate != nil || record.AtlasCandidate != nil) {
		return errors.New("Aave candidate signal identity conflicts with durable evidence")
	}
	return nil
}

func OpenPostgresSignalSink(ctx context.Context, dsn, floor string) (*PostgresSignalSink, error) {
	if dsn == "" || floor == "" {
		return nil, errors.New("durable signal sink configuration is incomplete")
	}
	config, err := pgxpool.ParseConfig(dsn)
	if err != nil {
		return nil, errors.New("durable signal sink configuration is invalid")
	}
	config.MaxConns = 2
	pool, err := pgxpool.NewWithConfig(ctx, config)
	if err != nil {
		return nil, errors.New("durable signal sink is unavailable")
	}
	if err := pool.Ping(ctx); err != nil {
		pool.Close()
		return nil, errors.New("durable signal sink is unavailable")
	}
	return &PostgresSignalSink{pool: pool, floor: floor, atlasAuctions: make(chan *observer.LedgerRecord, 256)}, nil
}

func (s *PostgresSignalSink) Close() { s.pool.Close() }

func (s *PostgresSignalSink) AtlasAuctions() <-chan *observer.LedgerRecord { return s.atlasAuctions }

type liveSizeAuthorityRow struct {
	lane                string
	armed               bool
	killSwitch          bool
	maximumInputAmount  string
	economicPhase       string
	economicInputAmount string
}

var errRevenueLaneAuthorityDiverged = errors.New("revenue lane authority diverged")

const (
	revenueAuthorityClosedReason   = "revenue_authority_closed"
	liveSizeAuthorityChangedReason = "live_size_authority_changed"
	providerRecoverySampleWindow   = 2 * time.Minute
)

func (s *PostgresSignalSink) RecordProviderFailure(ctx context.Context, reason string, observedAt time.Time) error {
	if reason == "" || observedAt.IsZero() {
		return errors.New("provider failure evidence is invalid")
	}
	result, err := s.pool.Exec(ctx, `
		UPDATE live_canary.revenue_provider_authority
		SET exact_execution_ready = false,
		    gate_reason = $1,
		    gate_updated_at = $2,
		    request_evidence_not_before = $2,
		    sample_count = 0,
		    sample_1_at = NULL, sample_1_primary_provider = NULL, sample_1_confirmation_provider = NULL,
		    sample_2_at = NULL, sample_2_primary_provider = NULL, sample_2_confirmation_provider = NULL,
		    sample_3_at = NULL, sample_3_primary_provider = NULL, sample_3_confirmation_provider = NULL,
		    recovery_status = 'collecting',
		    updated_at = now()
		WHERE singleton
	`, reason, observedAt.UTC())
	if err != nil || result.RowsAffected() != 1 {
		return errors.New("provider execution gate is unavailable")
	}
	return nil
}

func (s *PostgresSignalSink) ResetProviderRecoveryEvidence(ctx context.Context, reason string, observedAt time.Time) error {
	return s.RecordProviderFailure(ctx, reason, observedAt)
}

func canonicalProviderID(value string) bool {
	if len(value) < 1 || len(value) > 64 {
		return false
	}
	for index, char := range value {
		if (char >= 'a' && char <= 'z') || (char >= '0' && char <= '9') || (index > 0 && (char == '.' || char == '_' || char == '-')) {
			continue
		}
		return false
	}
	return true
}

func recordProviderAgreement(ctx context.Context, tx pgx.Tx, observedAt time.Time, primary, confirmation string) error {
	if observedAt.IsZero() || !canonicalProviderID(primary) || !canonicalProviderID(confirmation) || primary == confirmation {
		return errors.New("provider agreement evidence is invalid")
	}
	var failureTransition *time.Time
	var recoveryStatus string
	var requestEvidenceNotBefore time.Time
	var count int16
	var sampleAt [3]*time.Time
	var samplePrimary [3]*string
	var sampleConfirmation [3]*string
	err := tx.QueryRow(ctx, `
		SELECT failure_transition_at, recovery_status,
		       request_evidence_not_before, sample_count,
		       sample_1_at, sample_1_primary_provider, sample_1_confirmation_provider,
		       sample_2_at, sample_2_primary_provider, sample_2_confirmation_provider,
		       sample_3_at, sample_3_primary_provider, sample_3_confirmation_provider
		FROM live_canary.revenue_provider_authority
		WHERE singleton
		FOR UPDATE
	`).Scan(
		&failureTransition, &recoveryStatus, &requestEvidenceNotBefore, &count,
		&sampleAt[0], &samplePrimary[0], &sampleConfirmation[0],
		&sampleAt[1], &samplePrimary[1], &sampleConfirmation[1],
		&sampleAt[2], &samplePrimary[2], &sampleConfirmation[2],
	)
	if err != nil || count < 0 || count > 3 {
		return errors.New("provider recovery state is unavailable")
	}
	now := observedAt.UTC()
	if !now.After(requestEvidenceNotBefore.UTC()) {
		return errors.New("provider agreement predates the request evidence floor")
	}
	if failureTransition != nil && !now.After(failureTransition.UTC()) {
		return errors.New("provider agreement predates the failure transition")
	}
	type sample struct {
		at           time.Time
		primary      string
		confirmation string
	}
	samples := make([]sample, 0, 3)
	for index := 0; index < int(count); index++ {
		if sampleAt[index] == nil || samplePrimary[index] == nil || sampleConfirmation[index] == nil {
			return errors.New("provider recovery samples are invalid")
		}
		samples = append(samples, sample{sampleAt[index].UTC(), *samplePrimary[index], *sampleConfirmation[index]})
	}
	if len(samples) > 0 && recoveryStatus != "ready" && recoveryStatus != "recovered" {
		last := samples[len(samples)-1].at
		if !now.After(last) || now.Sub(last) > providerRecoverySampleWindow {
			samples = nil
		}
	}
	samples = append(samples, sample{now, primary, confirmation})
	if len(samples) > 3 {
		samples = append([]sample(nil), samples[len(samples)-3:]...)
	}
	var at [3]any
	var first [3]any
	var second [3]any
	for index, value := range samples {
		at[index], first[index], second[index] = value.at, value.primary, value.confirmation
	}
	status := "collecting"
	if len(samples) == 3 {
		if recoveryStatus == "recovered" {
			status = "recovered"
		} else {
			status = "ready"
		}
	}
	exactExecutionReady := len(samples) == 3 && (status == "ready" || status == "recovered")
	result, err := tx.Exec(ctx, `
		UPDATE live_canary.revenue_provider_authority
		SET exact_execution_ready = $13,
		    gate_reason = 'exact_dual_agreement', gate_updated_at = $1,
		    recovery_status = $2, sample_count = $3,
		    sample_1_at = $4, sample_1_primary_provider = $5, sample_1_confirmation_provider = $6,
		    sample_2_at = $7, sample_2_primary_provider = $8, sample_2_confirmation_provider = $9,
		    sample_3_at = $10, sample_3_primary_provider = $11, sample_3_confirmation_provider = $12,
		    updated_at = now()
		WHERE singleton
	`, now, status, len(samples), at[0], first[0], second[0], at[1], first[1], second[1], at[2], first[2], second[2], exactExecutionReady)
	if err != nil || result.RowsAffected() != 1 {
		return errors.New("provider agreement persistence failed")
	}
	return nil
}

func revenueLaneAuthorityError(detail string) error {
	return fmt.Errorf("%w: %s", errRevenueLaneAuthorityDiverged, detail)
}

func (s *PostgresSignalSink) CurrentAaveLiveMaximumInputAmount(ctx context.Context) (string, error) {
	rows, err := s.pool.Query(ctx, `
		SELECT lane, armed, kill_switch,
		       maximum_input_amount::text AS maximum_input_amount,
		       economic.phase AS economic_phase,
		       economic.current_input_wei::text AS economic_input_amount
		FROM live_canary.revenue_lane_controls
		CROSS JOIN live_canary.economic_control AS economic
		WHERE economic.singleton
		  AND lane IN ('aave_liquidation', 'atlas_solver')
		ORDER BY lane
	`)
	if err != nil {
		return "", errors.New("current Aave live size authority is unavailable")
	}
	defer rows.Close()
	states := make([]liveSizeAuthorityRow, 0, 2)
	for rows.Next() {
		var state liveSizeAuthorityRow
		if err := rows.Scan(
			&state.lane,
			&state.armed,
			&state.killSwitch,
			&state.maximumInputAmount,
			&state.economicPhase,
			&state.economicInputAmount,
		); err != nil {
			return "", errors.New("current Aave live size authority is invalid")
		}
		states = append(states, state)
	}
	if err := rows.Err(); err != nil {
		return "", errors.New("current Aave live size authority is unavailable")
	}
	return validatedAaveLiveMaximumInputAmount(states)
}

func validatedAaveLiveMaximumInputAmount(states []liveSizeAuthorityRow) (string, error) {
	if len(states) != 2 || states[0].lane != "aave_liquidation" || states[1].lane != "atlas_solver" {
		return "", revenueLaneAuthorityError("current Aave revenue lane authority is incomplete")
	}
	reviewed, reviewedOK := newBigUint(maximumReviewedInputWei)
	if !reviewedOK {
		return "", errors.New("reviewed Aave size authority is invalid")
	}
	closed := true
	active := true
	for _, state := range states {
		closed = closed && !state.armed && state.killSwitch
		active = active && state.armed && !state.killSwitch
		if state.economicPhase != states[0].economicPhase || state.economicInputAmount != states[0].economicInputAmount {
			return "", revenueLaneAuthorityError("economic size authority diverged across revenue lanes")
		}
	}
	if closed {
		// The gateway wire contract requires a positive live maximum. One wei is
		// below every reviewed SizeLevel, so every evaluated size remains
		// counterfactual and cannot materialize a live Candidate while disarmed.
		return "1", nil
	}
	liveEconomicPhase := strings.HasPrefix(states[0].economicPhase, "LIVE_")
	explicitMaximumReviewedAuthority := states[0].economicPhase == "DISARMED_EVIDENCE" &&
		states[0].economicInputAmount == maximumReviewedInputWei
	if !active || (!liveEconomicPhase && !explicitMaximumReviewedAuthority) {
		return "", revenueLaneAuthorityError("Aave revenue lane authority is partially armed")
	}
	economic, economicOK := newBigUint(states[0].economicInputAmount)
	if !economicOK || economic.Sign() <= 0 || economic.Cmp(reviewed) > 0 {
		return "", revenueLaneAuthorityError("economic Aave size authority is invalid")
	}
	for _, state := range states {
		laneMaximum, laneOK := newBigUint(state.maximumInputAmount)
		if !laneOK || laneMaximum.Cmp(economic) != 0 {
			return "", revenueLaneAuthorityError("revenue lane and economic size authorities diverged")
		}
	}
	return economic.String(), nil
}

func (s *PostgresSignalSink) RecordAtlasAuction(ctx context.Context, record *observer.LedgerRecord) error {
	if record == nil || record.AuctionID == "" || record.UserOpHash == "" || record.AuctionDeadlineBlock == "" || record.NotificationSHA256 == "" {
		return errors.New("Atlas auction identity is incomplete")
	}
	var asset any
	var aggregator any
	if record.OracleUpdate != nil {
		aggregator = strings.ToLower(record.OracleUpdate.Aggregator)
		if record.OracleUpdate.Asset != nil {
			asset = *record.OracleUpdate.Asset
		}
	}
	terminalOutcome := "observed"
	var rejectionReason any
	if record.RelevantAaveAuction {
		terminalOutcome = "economic_rejection"
		rejectionReason = "atlas_callback_evidence_unavailable"
	}
	result, err := s.pool.Exec(ctx, `
		INSERT INTO live_canary.atlas_auction_ingress(
			auction_id, user_operation_hash, parallel_auction_identity,
			auction_deadline_block, oracle_gas_price_wei, solver_gas_limit,
			dapp, oracle_aggregator, oracle_asset, relevant_aave,
			parallel_eligible, evidence_hash, terminal_outcome,
			rejection_reason, observed_at
		) VALUES (
			$1, $2, $3, $4::numeric, $5::numeric, $6,
			$7, $8, $9, $10, $11, $12, $13, $14, $15
		)
		ON CONFLICT (auction_id) DO UPDATE SET
			terminal_outcome = EXCLUDED.terminal_outcome,
			rejection_reason = EXCLUDED.rejection_reason,
			updated_at = now()
		WHERE live_canary.atlas_auction_ingress.evidence_hash = EXCLUDED.evidence_hash
		  AND live_canary.atlas_auction_ingress.terminal_outcome IN ('observed','exact_pending')
	`, record.AuctionID, strings.ToLower(record.UserOpHash), record.ParallelAuctionIdentity,
		record.AuctionDeadlineBlock, record.OracleGasPriceWei, record.SolverGasLimit,
		strings.ToLower(record.Dapp), aggregator, asset, record.RelevantAaveAuction,
		record.ParallelEligible, record.NotificationSHA256, terminalOutcome,
		rejectionReason, record.ObservedAt)
	if err != nil {
		return err
	}
	accepted := result.RowsAffected() == 1
	if !accepted {
		var exactReplay bool
		if err := s.pool.QueryRow(ctx, `
			SELECT EXISTS (
				SELECT 1
				FROM live_canary.atlas_auction_ingress
				WHERE auction_id = $1 AND evidence_hash = $2
			)
		`, record.AuctionID, record.NotificationSHA256).Scan(&exactReplay); err != nil {
			return err
		}
		if !exactReplay {
			return errors.New("Atlas auction evidence identity conflicts with durable ingress")
		}
		return nil
	}
	if record.RelevantAaveAuction {
		copy := *record
		select {
		case s.atlasAuctions <- &copy:
		case <-ctx.Done():
			return ctx.Err()
		}
	}
	return nil
}

func (s *PostgresSignalSink) RecordAtlasCallbackUnavailable(ctx context.Context, auctionID, evidenceHash string) error {
	if len(auctionID) < 1 || len(auctionID) > 128 || len(evidenceHash) != 64 {
		return errors.New("Atlas auction identity is invalid")
	}
	result, err := s.pool.Exec(ctx, `
		UPDATE live_canary.atlas_auction_ingress
		SET terminal_outcome = 'economic_rejection',
		    rejection_reason = 'atlas_callback_evidence_unavailable',
		    updated_at = now()
		WHERE auction_id = $1
		  AND evidence_hash = $2
		  AND relevant_aave
		  AND terminal_outcome IN ('observed','exact_pending','economic_rejection')
	`, auctionID, evidenceHash)
	if err != nil {
		return err
	}
	if result.RowsAffected() != 1 {
		return errors.New("Atlas auction disposition identity is incomplete")
	}
	return nil
}

func (s *PostgresSignalSink) RecordAaveSignal(ctx context.Context, record signal) (signal, error) {
	if record.TerminalOutcome == "" || record.Block == 0 || record.BlockHash == "" {
		return record, errors.New("Aave signal identity is incomplete")
	}
	tx, err := s.pool.Begin(ctx)
	if err != nil {
		return record, err
	}
	defer tx.Rollback(ctx)
	if record.ExactPrimaryProvider != "" || record.ExactSecondaryProvider != "" {
		observedAt := record.ObservedAt
		if record.ExactCompletedAt != nil {
			observedAt = record.ExactCompletedAt.UTC()
		}
		if err := recordProviderAgreement(ctx, tx, observedAt, record.ExactPrimaryProvider, record.ExactSecondaryProvider); err != nil {
			return record, err
		}
	}
	if record.ExecutionCandidate != nil || record.AtlasCandidate != nil {
		record, err = normalizeCandidateAuthority(ctx, tx, record)
		if err != nil {
			return record, err
		}
	}
	var approvalDigest, simulationResultHash string
	if record.ExecutionCandidate != nil {
		approvalDigest = record.ExecutionCandidate.ApprovalDigest
		simulationResultHash = record.ExecutionCandidate.SimulationResultHash
	}
	body, err := json.Marshal(struct {
		Signal               signal `json:"signal"`
		ApprovalDigest       string `json:"approval_digest,omitempty"`
		SimulationResultHash string `json:"simulation_result_hash,omitempty"`
	}{record, approvalDigest, simulationResultHash})
	if err != nil {
		return record, err
	}
	evidence := sha256.Sum256(body)
	identityBody := fmt.Sprintf("aave_liquidation|%s|%d|%d", record.Borrower, record.Block, record.Cursor)
	identityHash := sha256.Sum256([]byte(identityBody))
	signalID := deterministicUUID(identityHash)
	var upper any
	if record.ZeroCostProfitUpperBoundWei != "" {
		upper = record.ZeroCostProfitUpperBoundWei
	}
	var expected, conservative, stateRoot any
	if record.ExpectedNetPnLWei != "" {
		expected = record.ExpectedNetPnLWei
	}
	if record.ConservativeNetPnLWei != "" {
		conservative = record.ConservativeNetPnLWei
	}
	if record.StateRoot != "" {
		stateRoot = record.StateRoot
	}
	var evidenceMode any
	if record.ExecutionCandidate != nil {
		evidenceMode = record.ExecutionCandidate.RoutePayload.EvidenceMode
	} else if record.ExactDiagnostics != nil && record.ExactDiagnostics.ForkEvidenceMode != "" {
		evidenceMode = record.ExactDiagnostics.ForkEvidenceMode
	}
	var exactDiagnostics any
	if record.ExactDiagnostics != nil {
		encoded, marshalErr := json.Marshal(record.ExactDiagnostics)
		if marshalErr != nil || len(encoded) > 64*1024 {
			return record, errors.New("Aave exact diagnostics are invalid")
		}
		exactDiagnostics = string(encoded)
	}
	result, err := tx.Exec(ctx, `
		INSERT INTO live_canary.revenue_hunting_signals(
			signal_id, signal_identity, source_lane, source_cursor, borrower,
			block_number, block_hash, state_root, zero_cost_profit_upper_bound,
			expected_net_pnl, conservative_net_pnl,
			retained_profit_floor, evidence_mode, terminal_outcome,
			rejection_reason, exact_diagnostics, evidence_hash, observed_at
		) VALUES (
			$1, $2, 'aave_liquidation', $3::numeric, $4,
			$5::numeric, $6, $7, $8::numeric, $9::numeric, $10::numeric,
			$11::numeric, $12, $13, $14, $15::jsonb, $16, $17
		)
		ON CONFLICT (signal_identity) DO UPDATE SET
			block_number = EXCLUDED.block_number,
			block_hash = EXCLUDED.block_hash,
			state_root = EXCLUDED.state_root,
			zero_cost_profit_upper_bound = EXCLUDED.zero_cost_profit_upper_bound,
			expected_net_pnl = EXCLUDED.expected_net_pnl,
			conservative_net_pnl = EXCLUDED.conservative_net_pnl,
			evidence_mode = EXCLUDED.evidence_mode,
			terminal_outcome = EXCLUDED.terminal_outcome,
			rejection_reason = EXCLUDED.rejection_reason,
			exact_diagnostics = EXCLUDED.exact_diagnostics,
			evidence_hash = EXCLUDED.evidence_hash,
			updated_at = now()
		WHERE (
			live_canary.revenue_hunting_signals.terminal_outcome IN
				('prefiltered','exact_pending','fork_pending','incomplete')
			AND NOT (
				live_canary.revenue_hunting_signals.exact_diagnostics IS NOT NULL
				AND EXCLUDED.exact_diagnostics IS NULL
			)
		) OR live_canary.revenue_hunting_signals.evidence_hash = EXCLUDED.evidence_hash
	`, signalID, identityBody, record.Cursor, record.Borrower, record.Block,
		record.BlockHash, stateRoot, upper, expected, conservative, s.floor,
		evidenceMode, persistedTerminalOutcome(record), signalRejectionReason(record),
		exactDiagnostics, hex.EncodeToString(evidence[:]), record.ObservedAt)
	if err != nil {
		return record, err
	}
	if err := requireAcceptedCandidateSignalWrite(result.RowsAffected(), record); err != nil {
		return record, err
	}
	if record.ExecutionCandidate != nil {
		if err := insertExecutionCandidate(ctx, tx, record.ExecutionCandidate); err != nil {
			return record, err
		}
	}
	if record.AtlasCandidate != nil {
		if err := insertAtlasCandidate(ctx, tx, record, record.AtlasCandidate, s.floor); err != nil {
			return record, err
		}
	}
	return record, tx.Commit(ctx)
}

func normalizeCandidateAuthority(ctx context.Context, tx pgx.Tx, record signal) (signal, error) {
	var economicPhase, economicInput string
	var providerExecutionReady bool
	if err := tx.QueryRow(ctx, `
		SELECT economic.phase, economic.current_input_wei::text, provider.exact_execution_ready
		FROM live_canary.economic_control
		CROSS JOIN live_canary.revenue_provider_authority AS provider
		WHERE economic.singleton AND provider.singleton
		FOR UPDATE OF economic, provider
	`).Scan(&economicPhase, &economicInput, &providerExecutionReady); err != nil {
		return record, errors.New("candidate economic authority is unavailable")
	}
	rows, err := tx.Query(ctx, `
		SELECT lane, armed, kill_switch, maximum_input_amount::text
		FROM live_canary.revenue_lane_controls
		WHERE lane IN ('aave_liquidation', 'atlas_solver')
		ORDER BY lane
		FOR UPDATE
	`)
	if err != nil {
		return record, errors.New("candidate revenue authority is unavailable")
	}
	defer rows.Close()
	states := make([]liveSizeAuthorityRow, 0, 2)
	for rows.Next() {
		var state liveSizeAuthorityRow
		if err := rows.Scan(&state.lane, &state.armed, &state.killSwitch, &state.maximumInputAmount); err != nil {
			return record, errors.New("candidate revenue authority is invalid")
		}
		state.economicPhase = economicPhase
		state.economicInputAmount = economicInput
		states = append(states, state)
	}
	if err := rows.Err(); err != nil {
		return record, errors.New("candidate revenue authority is unavailable")
	}
	currentMaximum, authorityErr := validatedAaveLiveMaximumInputAmount(states)
	active := providerExecutionReady && len(states) == 2
	for _, state := range states {
		active = active && state.armed && !state.killSwitch
	}
	return normalizeCandidateForCurrentAuthority(record, currentMaximum, authorityErr, active)
}

func normalizeCandidateForCurrentAuthority(
	record signal,
	currentMaximum string,
	authorityErr error,
	active bool,
) (signal, error) {
	if authorityErr != nil {
		if !errors.Is(authorityErr, errRevenueLaneAuthorityDiverged) {
			return record, authorityErr
		}
		return withoutCandidateAuthority(record, revenueLaneAuthorityDivergedClass), nil
	}
	if !active {
		return withoutCandidateAuthority(record, revenueAuthorityClosedReason), nil
	}
	if !candidateMatchesCurrentAuthority(record, currentMaximum) {
		return withoutCandidateAuthority(record, liveSizeAuthorityChangedReason), nil
	}
	return record, nil
}

func withoutCandidateAuthority(record signal, reason string) signal {
	record.Authority = false
	record.ExecutionCandidate = nil
	record.AtlasCandidate = nil
	record.TerminalOutcome = "exact_pending"
	record.ExactDeferredReason = reason
	return record
}

func candidateMatchesCurrentAuthority(record signal, currentMaximum string) bool {
	maximum, maximumOK := newBigUint(currentMaximum)
	if !maximumOK || maximum.Sign() <= 0 {
		return false
	}
	if record.ExecutionCandidate != nil {
		selected, selectedOK := newBigUint(record.ExecutionCandidate.SelectedSize)
		if !selectedOK || selected.Sign() <= 0 || selected.Cmp(maximum) > 0 ||
			record.ExecutionCandidate.MaximumInputAmount != currentMaximum {
			return false
		}
	}
	if record.AtlasCandidate != nil {
		selected, selectedOK := newBigUint(record.AtlasCandidate.SelectedSize)
		if !selectedOK || selected.Sign() <= 0 || selected.Cmp(maximum) > 0 ||
			record.AtlasCandidate.MaximumInputAmount != currentMaximum {
			return false
		}
	}
	return record.ExecutionCandidate != nil || record.AtlasCandidate != nil
}

type candidateExecutor interface {
	Exec(context.Context, string, ...any) (pgconn.CommandTag, error)
}

func insertExecutionCandidate(ctx context.Context, executor candidateExecutor, candidate *executionCandidate) error {
	if candidate == nil || candidate.ApprovalDigest == "" {
		return errors.New("execution candidate approval is incomplete")
	}
	legs, err := json.Marshal(candidate.Legs)
	if err != nil {
		return err
	}
	tokenPath, err := json.Marshal(candidate.TokenPath)
	if err != nil {
		return err
	}
	routePayload, err := json.Marshal(candidate.RoutePayload)
	if err != nil {
		return err
	}
	result, err := executor.Exec(ctx, `
		INSERT INTO live_canary.execution_requests(
			id, opportunity_id, schema_version, chain_id, route_id,
			route_fingerprint, route_type, route_payload, selected_size,
			token_path, origin_router, executor_address, executor_code_hash,
			calldata_hash, simulation_result_hash, plan_hash,
			pinned_block_number, pinned_block_hash, flash_asset, flash_amount,
			maximum_input_amount, minimum_profit, expected_profit, deadline,
			legs, gas_limit, max_fee_per_gas, max_priority_fee_per_gas,
			approved_by, approved_at, approval_deadline, policy_version,
			approval_digest, status, created_at, updated_at
		) VALUES (
			$1::uuid, $2::uuid, 'phoenix.live-execution-request.v2', 42161, $3,
			'AAVE_LIQUIDATION_V1', 'AAVE_LIQUIDATION_V1', $4::jsonb, $5::numeric,
			$6::jsonb, $7, $8, $9, $10, $11, $12,
			$13::numeric, $14, $15, $16::numeric, $17::numeric, $18::numeric,
			$19::numeric, $20, $21::jsonb, $22, $23::numeric, $24::numeric,
			$25, $26, $27, $28, $29, 'approved', $26, $26
		)
		ON CONFLICT (id) DO UPDATE SET updated_at = now()
		WHERE live_canary.execution_requests.opportunity_id = EXCLUDED.opportunity_id
		  AND live_canary.execution_requests.route_id = EXCLUDED.route_id
		  AND live_canary.execution_requests.approval_digest = EXCLUDED.approval_digest
	`, candidate.RequestID, candidate.OpportunityID, candidate.RouteID, routePayload,
		candidate.SelectedSize, tokenPath, candidate.OriginRouter, candidate.ExecutorAddress,
		candidate.ExecutorCodeHash, candidate.CalldataHash, candidate.SimulationResultHash,
		candidate.PlanHash, candidate.PinnedBlockNumber, candidate.PinnedBlockHash,
		candidate.FlashAsset, candidate.FlashAmount, candidate.MaximumInputAmount,
		candidate.MinimumProfit, candidate.ExpectedProfit, candidate.Deadline, legs,
		candidate.GasLimit, candidate.MaxFeePerGas, candidate.MaxPriorityFeePerGas,
		candidate.ApprovedBy, candidate.ApprovedAt, candidate.ApprovalDeadline,
		candidate.PolicyVersion, candidate.ApprovalDigest)
	if err != nil {
		return err
	}
	if result.RowsAffected() != 1 {
		return errors.New("execution candidate identity conflict")
	}
	return nil
}

func insertAtlasCandidate(ctx context.Context, executor candidateExecutor, record signal, candidate *atlasCandidate, floor string) error {
	if candidate == nil || candidate.OperationHash == "" || candidate.SimulationResultHash == "" {
		return errors.New("Atlas execution candidate is incomplete")
	}
	operation, err := json.Marshal(candidate.Operation)
	if err != nil {
		return err
	}
	identity := fmt.Sprintf("atlas_solver|%s|%s|%d", candidate.AuctionID, record.Borrower, record.Block)
	identityHash := sha256.Sum256([]byte(identity))
	signalID := deterministicUUID(identityHash)
	evidenceBody, err := json.Marshal(struct {
		Identity             string `json:"identity"`
		OperationHash        string `json:"operation_hash"`
		SimulationResultHash string `json:"simulation_result_hash"`
	}{identity, candidate.OperationHash, candidate.SimulationResultHash})
	if err != nil {
		return err
	}
	evidenceHash := sha256.Sum256(evidenceBody)
	result, err := executor.Exec(ctx, `
		INSERT INTO live_canary.revenue_hunting_signals(
			signal_id, signal_identity, source_lane, source_cursor, auction_id,
			borrower, block_number, block_hash, state_root, expected_net_pnl,
			conservative_net_pnl, retained_profit_floor, evidence_mode,
			terminal_outcome, evidence_hash, observed_at
		) VALUES (
			$1::uuid, $2, 'atlas_solver', $3::numeric, $4, $5, $6::numeric,
			$7, $8, $9::numeric, $10::numeric, $11::numeric, $12,
			'candidate', $13, $14
		)
		ON CONFLICT (signal_identity) DO UPDATE SET updated_at = now()
		WHERE live_canary.revenue_hunting_signals.evidence_hash = EXCLUDED.evidence_hash
	`, signalID, identity, record.Cursor, candidate.AuctionID, record.Borrower,
		record.Block, record.BlockHash, record.StateRoot, candidate.ExpectedNetPnL,
		candidate.ConservativeNetPnL, floor, candidate.EvidenceMode,
		hex.EncodeToString(evidenceHash[:]), candidate.ObservedAt)
	if err != nil {
		return err
	}
	if result.RowsAffected() != 1 {
		return errors.New("Atlas signal identity conflict")
	}
	result, err = executor.Exec(ctx, `
		INSERT INTO live_canary.atlas_solver_requests(
			auction_id, signal_id, user_operation_hash, solver_operation_hash,
			solver_operation, maximum_bid, selected_bid, status, created_at
		) VALUES ($1, $2::uuid, $3, $4, $5::jsonb, $6::numeric, $7::numeric, 'ready', $8)
		ON CONFLICT (auction_id) DO UPDATE SET updated_at = now()
		WHERE live_canary.atlas_solver_requests.solver_operation_hash = EXCLUDED.solver_operation_hash
	`, candidate.AuctionID, signalID, candidate.Operation.UserOpHash,
		candidate.OperationHash, operation, candidate.MaximumBid, candidate.SelectedBid,
		candidate.ObservedAt)
	if err != nil {
		return err
	}
	if result.RowsAffected() != 1 {
		return errors.New("Atlas solver request identity conflict")
	}
	_, err = executor.Exec(ctx, `
		UPDATE live_canary.atlas_auction_ingress
		SET terminal_outcome = 'candidate', updated_at = now()
		WHERE auction_id = $1 AND terminal_outcome IN ('observed','exact_pending')
	`, candidate.AuctionID)
	return err
}

func deterministicUUID(digest [32]byte) string {
	bytes := digest[:16]
	bytes[6] = (bytes[6] & 0x0f) | 0x50
	bytes[8] = (bytes[8] & 0x3f) | 0x80
	return fmt.Sprintf("%s-%s-%s-%s-%s", hex.EncodeToString(bytes[0:4]), hex.EncodeToString(bytes[4:6]), hex.EncodeToString(bytes[6:8]), hex.EncodeToString(bytes[8:10]), hex.EncodeToString(bytes[10:16]))
}

func persistedTerminalOutcome(record signal) string {
	switch record.TerminalOutcome {
	case "counterfactual_positive":
		return "economic_rejection"
	case "atlas_evidence_rejection":
		return "fork_rejection"
	default:
		return record.TerminalOutcome
	}
}

func signalRejectionReason(record signal) any {
	if record.AuthorityRejectionReason != "" {
		return record.AuthorityRejectionReason
	}
	if record.ExactRouteIneligibleReason != "" {
		return record.ExactRouteIneligibleReason
	}
	if record.ExactDeferredReason != "" {
		return record.ExactDeferredReason
	}
	if record.ExactDiagnostics != nil && record.ExactDiagnostics.FailureClass != "" {
		return record.ExactDiagnostics.FailureClass
	}
	if len(record.SizeDiagnostics) > 0 {
		if failure := buildExactDiagnosticSummary(record, 0, 0).FailureClass; failure != "" {
			return failure
		}
	}
	if record.TerminalOutcome == "economic_rejection" && record.StateRoot == "" && record.ZeroCostProfitUpperBoundWei != "" {
		return "prefilter_upper_bound_below_floor"
	}
	if record.TerminalOutcome == "incomplete" {
		return "economic_bound_incomplete"
	}
	if record.TerminalOutcome == "atlas_evidence_rejection" {
		return "atlas_callback_evidence_unavailable"
	}
	return nil
}
