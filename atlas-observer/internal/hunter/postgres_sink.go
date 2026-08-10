package hunter

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"strings"

	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
	"github.com/jackc/pgx/v5/pgconn"
	"github.com/jackc/pgx/v5/pgxpool"
)

type PostgresSignalSink struct {
	pool          *pgxpool.Pool
	floor         string
	atlasAuctions chan *observer.LedgerRecord
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
		return "", errors.New("current Aave revenue lane authority is incomplete")
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
			return "", errors.New("economic size authority diverged across revenue lanes")
		}
	}
	if closed {
		// The gateway wire contract requires a positive live maximum. One wei is
		// below every reviewed SizeLevel, so every evaluated size remains
		// counterfactual and cannot materialize a live Candidate while disarmed.
		return "1", nil
	}
	if !active || !strings.HasPrefix(states[0].economicPhase, "LIVE_") {
		return "", errors.New("Aave revenue lane authority is partially armed")
	}
	economic, economicOK := newBigUint(states[0].economicInputAmount)
	if !economicOK || economic.Sign() <= 0 || economic.Cmp(reviewed) > 0 {
		return "", errors.New("economic Aave size authority is invalid")
	}
	for _, state := range states {
		laneMaximum, laneOK := newBigUint(state.maximumInputAmount)
		if !laneOK || laneMaximum.Cmp(economic) != 0 {
			return "", errors.New("revenue lane and economic size authorities diverged")
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
	_, err := s.pool.Exec(ctx, `
		INSERT INTO live_canary.atlas_auction_ingress(
			auction_id, user_operation_hash, parallel_auction_identity,
			auction_deadline_block, oracle_gas_price_wei, solver_gas_limit,
			dapp, oracle_aggregator, oracle_asset, relevant_aave,
			parallel_eligible, evidence_hash, observed_at
		) VALUES (
			$1, $2, $3, $4::numeric, $5::numeric, $6,
			$7, $8, $9, $10, $11, $12, $13
		)
		ON CONFLICT (auction_id) DO UPDATE SET
			updated_at = now()
		WHERE live_canary.atlas_auction_ingress.evidence_hash = EXCLUDED.evidence_hash
	`, record.AuctionID, strings.ToLower(record.UserOpHash), record.ParallelAuctionIdentity,
		record.AuctionDeadlineBlock, record.OracleGasPriceWei, record.SolverGasLimit,
		strings.ToLower(record.Dapp), aggregator, asset, record.RelevantAaveAuction,
		record.ParallelEligible, record.NotificationSHA256, record.ObservedAt)
	if err != nil {
		return err
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

func (s *PostgresSignalSink) RecordAaveSignal(ctx context.Context, record signal) error {
	if record.TerminalOutcome == "" || record.Block == 0 || record.BlockHash == "" {
		return errors.New("Aave signal identity is incomplete")
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
		return err
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
	}
	tx, err := s.pool.Begin(ctx)
	if err != nil {
		return err
	}
	defer tx.Rollback(ctx)
	_, err = tx.Exec(ctx, `
		INSERT INTO live_canary.revenue_hunting_signals(
			signal_id, signal_identity, source_lane, source_cursor, borrower,
			block_number, block_hash, state_root, zero_cost_profit_upper_bound,
			expected_net_pnl, conservative_net_pnl,
			retained_profit_floor, evidence_mode, terminal_outcome,
			rejection_reason, evidence_hash, observed_at
		) VALUES (
			$1, $2, 'aave_liquidation', $3::numeric, $4,
			$5::numeric, $6, $7, $8::numeric, $9::numeric, $10::numeric,
			$11::numeric, $12, $13, $14, $15, $16
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
			evidence_hash = EXCLUDED.evidence_hash,
			updated_at = now()
		WHERE live_canary.revenue_hunting_signals.terminal_outcome IN
			('prefiltered','exact_pending','fork_pending','incomplete')
	`, signalID, identityBody, record.Cursor, record.Borrower, record.Block,
		record.BlockHash, stateRoot, upper, expected, conservative, s.floor,
		evidenceMode, persistedTerminalOutcome(record), signalRejectionReason(record),
		hex.EncodeToString(evidence[:]), record.ObservedAt)
	if err != nil {
		return err
	}
	if record.ExecutionCandidate != nil {
		if err := insertExecutionCandidate(ctx, tx, record.ExecutionCandidate); err != nil {
			return err
		}
	}
	if record.AtlasCandidate != nil {
		if err := insertAtlasCandidate(ctx, tx, record, record.AtlasCandidate, s.floor); err != nil {
			return err
		}
	}
	return tx.Commit(ctx)
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
			approval_digest, status
		) VALUES (
			$1::uuid, $2::uuid, 'phoenix.live-execution-request.v2', 42161, $3,
			'AAVE_LIQUIDATION_V1', 'AAVE_LIQUIDATION_V1', $4::jsonb, $5::numeric,
			$6::jsonb, $7, $8, $9, $10, $11, $12,
			$13::numeric, $14, $15, $16::numeric, $17::numeric, $18::numeric,
			$19::numeric, $20, $21::jsonb, $22, $23::numeric, $24::numeric,
			$25, $26, $27, $28, $29, 'approved'
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
	if record.TerminalOutcome == "economic_rejection" {
		return "zero_cost_upper_bound_below_floor"
	}
	if record.TerminalOutcome == "incomplete" {
		return "economic_bound_incomplete"
	}
	if record.TerminalOutcome == "atlas_evidence_rejection" {
		return "atlas_callback_evidence_unavailable"
	}
	return nil
}
