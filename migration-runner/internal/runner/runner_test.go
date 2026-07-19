package runner

import (
	"context"
	"database/sql"
	"errors"
	"fmt"
	"net/url"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	_ "github.com/lib/pq"
)

func TestShadowProfitabilityMigrationIsAdditiveAndFailClosed(t *testing.T) {
	migrationPath := filepath.Join("..", "..", "..", "migrations", "003_shadow_profitability_evidence.sql")
	content, err := os.ReadFile(migrationPath)
	if err != nil {
		t.Fatalf("read shadow profitability migration: %v", err)
	}
	sqlText := strings.ToUpper(string(content))
	for _, required := range []string{
		"CREATE TABLE IF NOT EXISTS SHADOW_DECISIONS",
		"CREATE TABLE IF NOT EXISTS RPC_QUALITY_RECORDS",
		"CREATE TABLE IF NOT EXISTS SHADOW_REPLAY_RUNS",
		"CHECK (EXECUTION_ELIGIBLE = FALSE)",
	} {
		if !strings.Contains(sqlText, required) {
			t.Fatalf("migration missing %q", required)
		}
	}
	for _, destructive := range []string{"DROP TABLE", "DROP COLUMN", "TRUNCATE TABLE"} {
		if strings.Contains(sqlText, destructive) {
			t.Fatalf("migration contains destructive statement %q", destructive)
		}
	}
}

func TestShadowEngineRuntimeMigrationIsAdditiveBoundedAndIdempotent(t *testing.T) {
	migrationPath := filepath.Join("..", "..", "..", "migrations", "004_shadow_engine_runtime.sql")
	content, err := os.ReadFile(migrationPath)
	if err != nil {
		t.Fatalf("read shadow Engine runtime migration: %v", err)
	}
	sqlText := strings.ToUpper(string(content))
	for _, required := range []string{
		"CREATE TABLE IF NOT EXISTS ENGINE_OUTBOX",
		"SOURCE_EVENT_IDENTITY TEXT NOT NULL UNIQUE",
		"OCTET_LENGTH(PAYLOAD::TEXT) <= 1048576",
		"ENGINE_OUTBOX_PENDING_IDX",
		"ENGINE_OUTBOX_RETRY_IDX",
		"CREATE TABLE IF NOT EXISTS SHADOW_ENGINE_CLASSIFICATIONS",
		"CREATE TABLE IF NOT EXISTS SHADOW_ENGINE_PROCESSING_ATTEMPTS",
	} {
		if !strings.Contains(sqlText, required) {
			t.Fatalf("migration missing %q", required)
		}
	}
	for _, destructive := range []string{"DROP TABLE", "DROP COLUMN", "TRUNCATE TABLE", "DELETE FROM"} {
		if strings.Contains(sqlText, destructive) {
			t.Fatalf("migration contains destructive statement %q", destructive)
		}
	}
}

func TestShadowDecisionIdentityMigrationRemovesOnlyLegacyCollisionKey(t *testing.T) {
	migrationPath := filepath.Join("..", "..", "..", "migrations", "005_shadow_decision_identity.sql")
	content, err := os.ReadFile(migrationPath)
	if err != nil {
		t.Fatalf("read shadow decision identity migration: %v", err)
	}
	sqlText := strings.ToUpper(string(content))
	for _, required := range []string{
		"UNIQUE (STRATEGY_VERSION, ROUTE_FINGERPRINT, SOURCE_SEQUENCE, OBSERVED_BLOCK)",
		"ALTER TABLE PUBLIC.SHADOW_DECISIONS DROP CONSTRAINT",
		"CREATE UNIQUE INDEX IF NOT EXISTS SHADOW_DECISIONS_SOURCE_EVENT_ROUTE_IDX",
		"SOURCE_EVENT_IDENTITY, STRATEGY_VERSION, ROUTE_FINGERPRINT",
	} {
		if !strings.Contains(sqlText, required) {
			t.Fatalf("migration missing %q", required)
		}
	}
	for _, destructive := range []string{"DROP TABLE", "DROP COLUMN", "TRUNCATE TABLE", "DELETE FROM"} {
		if strings.Contains(sqlText, destructive) {
			t.Fatalf("migration contains destructive statement %q", destructive)
		}
	}
}

func TestDependencyExhaustionMigrationOnlyExtendsClassificationChecks(t *testing.T) {
	migrationPath := filepath.Join("..", "..", "..", "migrations", "006_dependency_exhaustion_quarantine.sql")
	content, err := os.ReadFile(migrationPath)
	if err != nil {
		t.Fatalf("read dependency exhaustion migration: %v", err)
	}
	sqlText := strings.ToUpper(string(content))
	for _, required := range []string{
		"ALTER TABLE SHADOW_ENGINE_CLASSIFICATIONS",
		"ALTER TABLE SHADOW_ENGINE_PROCESSING_ATTEMPTS",
		"DEPENDENCY_EXHAUSTED",
		"TRANSIENT_DEPENDENCY_FAILURE",
		"TERMINAL_INTEGRITY_FAILURE",
	} {
		if !strings.Contains(sqlText, required) {
			t.Fatalf("migration missing %q", required)
		}
	}
	for _, destructive := range []string{"DROP TABLE", "DROP COLUMN", "TRUNCATE TABLE", "DELETE FROM"} {
		if strings.Contains(sqlText, destructive) {
			t.Fatalf("migration contains destructive statement %q", destructive)
		}
	}
}

func TestCanonicalProfitabilityMigrationIsAdditiveBoundedAndFailClosed(t *testing.T) {
	migrationPath := filepath.Join("..", "..", "..", "migrations", "007_canonical_profitability_truth.sql")
	content, err := os.ReadFile(migrationPath)
	if err != nil {
		t.Fatalf("read canonical profitability migration: %v", err)
	}
	sqlText := strings.ToUpper(string(content))
	for _, required := range []string{
		"CREATE TABLE IF NOT EXISTS SHADOW_PROFITABILITY_FACTS",
		"EVIDENCE_COMPLETENESS_STATUS <> 'COMPLETE'",
		"GROSS_PROFIT = GROSS_SPREAD - PROTOCOL_FEES - DEX_FEES - PRICE_IMPACT",
		"ARBITRUM_EXECUTION_FEE = EXECUTION_GAS * GAS_PRICE",
		"EXPECTED_NET_PNL = GROSS_SPREAD - TOTAL_COST",
		"VERIFICATION_SKIP_REASON = 'PRIMARY_BELOW_MINIMUM'",
		"SHADOW_ONLY = TRUE",
		"EXECUTION_ELIGIBLE = FALSE",
		"EXECUTION_REQUEST_CREATED = FALSE",
		"CREATE INDEX IF NOT EXISTS SHADOW_PROFITABILITY_EVALUATED_IDX",
		"CREATE OR REPLACE VIEW SHADOW_PROFITABILITY_REPORT_ROWS",
		"NULL::NUMERIC AS EXPECTED_NET_PNL",
	} {
		if !strings.Contains(sqlText, required) {
			t.Fatalf("migration missing %q", required)
		}
	}
	for _, destructive := range []string{
		"DROP TABLE",
		"DROP COLUMN",
		"TRUNCATE TABLE",
		"DELETE FROM",
		"UPDATE SHADOW_DECISIONS",
	} {
		if strings.Contains(sqlText, destructive) {
			t.Fatalf("migration contains destructive statement %q", destructive)
		}
	}
}

func TestShadowRouteDiscoveryIndexesAreAdditive(t *testing.T) {
	migrationPath := filepath.Join("..", "..", "..", "migrations", "008_shadow_route_discovery_indexes.sql")
	content, err := os.ReadFile(migrationPath)
	if err != nil {
		t.Fatalf("read shadow route discovery index migration: %v", err)
	}
	sqlText := strings.ToUpper(string(content))
	for _, required := range []string{
		"CREATE INDEX IF NOT EXISTS RPC_QUALITY_RECORDS_SHADOW_DECISION_IDX",
		"CREATE INDEX IF NOT EXISTS POOL_STATE_CHECKPOINTS_LATEST_POOL_IDX",
	} {
		if !strings.Contains(sqlText, required) {
			t.Fatalf("migration missing %q", required)
		}
	}
	for _, destructive := range []string{"DROP", "TRUNCATE", "DELETE", "UPDATE", "ALTER"} {
		if strings.Contains(sqlText, destructive) {
			t.Fatalf("migration contains destructive statement %q", destructive)
		}
	}
}

func TestProfitTriggeredVerificationMigrationIsForwardOnlyAndFailClosed(t *testing.T) {
	migrationPath := filepath.Join("..", "..", "..", "migrations", "009_profit_triggered_secondary_verification.sql")
	content, err := os.ReadFile(migrationPath)
	if err != nil {
		t.Fatalf("read profit-triggered verification migration: %v", err)
	}
	sqlText := strings.ToUpper(string(content))
	for _, required := range []string{
		"ADD COLUMN IF NOT EXISTS ROUTE_CONFIG_HASH",
		"INDEPENDENT_VERIFICATION_STATUS IN",
		"'NOT_REQUESTED'",
		"'REQUESTED'",
		"'AGREED'",
		"'DISAGREED'",
		"'PROVIDER_UNAVAILABLE'",
		"'INTEGRITY_FAILURE'",
		"SECONDARY_PROVIDER_ID <> PRIMARY_PROVIDER_ID",
		"SECONDARY_BLOCK_NUMBER = PINNED_BLOCK_NUMBER",
		"SECONDARY_BLOCK_HASH = PINNED_BLOCK_HASH",
		"SECONDARY_ROUTE_CONFIG_HASH = ROUTE_CONFIG_HASH",
		"EXECUTION_REQUEST_CREATED",
		"CREATE INDEX IF NOT EXISTS SHADOW_PROFITABILITY_INDEPENDENT_VERIFICATION_IDX",
	} {
		if !strings.Contains(sqlText, required) {
			t.Fatalf("migration missing %q", required)
		}
	}
	for _, destructive := range []string{"DROP TABLE", "DROP COLUMN", "TRUNCATE TABLE", "DELETE FROM", "UPDATE SHADOW_PROFITABILITY_FACTS"} {
		if strings.Contains(sqlText, destructive) {
			t.Fatalf("migration contains destructive statement %q", destructive)
		}
	}
}

func TestForkSimulationEvidenceMigrationIsAdditiveAndForkOnly(t *testing.T) {
	migrationPath := filepath.Join("..", "..", "..", "migrations", "010_fork_simulation_evidence.sql")
	content, err := os.ReadFile(migrationPath)
	if err != nil {
		t.Fatalf("read fork simulation evidence migration: %v", err)
	}
	sqlText := strings.ToUpper(string(content))
	for _, required := range []string{
		"ADD COLUMN IF NOT EXISTS FORK_EVIDENCE_SCHEMA_VERSION",
		"CREATE TABLE IF NOT EXISTS FORK_SIMULATION_RESULTS",
		"PHOENIX.UNSIGNED-FORK-PLAN.V1",
		"PHOENIX.FORK-RESULT.V1",
		"FORK_ONLY = TRUE",
		"SHADOW_ONLY = TRUE",
		"LIVE_EXECUTION = FALSE",
		"EXECUTION_ELIGIBLE = FALSE",
		"EXECUTION_REQUEST_CREATED = FALSE",
		"PUBLIC_BROADCAST = FALSE",
		"SIGNER_USED = FALSE",
	} {
		if !strings.Contains(sqlText, required) {
			t.Fatalf("migration missing %q", required)
		}
	}
	for _, destructive := range []string{
		"DROP TABLE",
		"DROP COLUMN",
		"TRUNCATE TABLE",
		"DELETE FROM",
		"UPDATE SHADOW_PROFITABILITY_FACTS",
	} {
		if strings.Contains(sqlText, destructive) {
			t.Fatalf("migration contains destructive statement %q", destructive)
		}
	}
}

func TestMoneyPathSelectivePersistenceMigrationIsAdditiveAndBounded(t *testing.T) {
	migrationPath := filepath.Join("..", "..", "..", "migrations", "011_money_path_selective_persistence.sql")
	content, err := os.ReadFile(migrationPath)
	if err != nil {
		t.Fatalf("read money-path persistence migration: %v", err)
	}
	sqlText := strings.ToUpper(string(content))
	for _, required := range []string{
		"CREATE TABLE IF NOT EXISTS MONEY_PATH_INGRESS_DAILY",
		"CREATE TABLE IF NOT EXISTS MONEY_PATH_INGRESS_SAMPLES",
		"UNSUPPORTED_INTERESTING",
		"SAMPLE_ORDINAL BETWEEN 1 AND 1000",
		"MONEY_PATH.INGRESS.V1",
		"SAFE_DECODER_SUMMARY",
	} {
		if !strings.Contains(sqlText, required) {
			t.Fatalf("migration missing %q", required)
		}
	}
	for _, destructive := range []string{"DROP TABLE", "TRUNCATE", "DELETE FROM", "VACUUM FULL"} {
		if strings.Contains(sqlText, destructive) {
			t.Fatalf("migration contains destructive statement %q", destructive)
		}
	}
}

func TestLoadMigrationsOrdersByVersion(t *testing.T) {
	dir := t.TempDir()
	writeMigration(t, dir, "002_second.sql", "SELECT 2;")
	writeMigration(t, dir, "001_first.sql", "SELECT 1;")
	migrations, err := LoadMigrations(dir)
	if err != nil {
		t.Fatal(err)
	}
	if len(migrations) != 2 {
		t.Fatalf("expected two migrations, got %d", len(migrations))
	}
	if migrations[0].Version != "001_first" || migrations[1].Version != "002_second" {
		t.Fatalf("unexpected order: %+v", migrations)
	}
}

func TestPendingReturnsFirstMigration(t *testing.T) {
	migrations := []Migration{{Version: "001", Checksum: "a"}}
	pending, err := Pending(map[string]string{}, migrations)
	if err != nil {
		t.Fatal(err)
	}
	if len(pending) != 1 || pending[0].Version != "001" {
		t.Fatalf("unexpected pending migrations: %+v", pending)
	}
}

func TestPendingSkipsAlreadyAppliedMigration(t *testing.T) {
	migrations := []Migration{{Version: "001", Checksum: "a"}}
	pending, err := Pending(map[string]string{"001": "a"}, migrations)
	if err != nil {
		t.Fatal(err)
	}
	if len(pending) != 0 {
		t.Fatalf("expected no pending migrations, got %+v", pending)
	}
}

func TestPendingFailsChangedChecksum(t *testing.T) {
	migrations := []Migration{{Version: "001", Checksum: "new"}}
	if _, err := Pending(map[string]string{"001": "old"}, migrations); err == nil {
		t.Fatal("expected changed checksum error")
	}
}

func TestFreshV5DatabaseInitializesFromZeroAndIsIdempotent(t *testing.T) {
	dsn := os.Getenv("MIGRATION_TEST_DSN")
	if dsn == "" {
		t.Skip("MIGRATION_TEST_DSN not set")
	}
	if os.Getenv("PHOENIX_FRESH_DATABASE_TEST_CONFIRM") != "CREATE_AND_DROP_ISOLATED_TEST_DATABASE" {
		t.Fatal("PHOENIX_FRESH_DATABASE_TEST_CONFIRM is required")
	}
	parsed, err := url.Parse(dsn)
	if err != nil {
		t.Fatalf("parse migration test DSN: %v", err)
	}
	if parsed.Scheme != "postgres" && parsed.Scheme != "postgresql" {
		t.Fatal("migration test DSN must use PostgreSQL")
	}
	if parsed.Hostname() != "127.0.0.1" && parsed.Hostname() != "localhost" {
		t.Fatal("fresh database integration test is loopback-only")
	}

	databaseName := fmt.Sprintf("phoenix_v5_fresh_test_%d", time.Now().UnixNano())
	adminURL := *parsed
	adminURL.Path = "/postgres"
	admin, err := sql.Open("postgres", adminURL.String())
	if err != nil {
		t.Fatal(err)
	}
	defer admin.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 60*time.Second)
	defer cancel()
	if err := admin.PingContext(ctx); err != nil {
		t.Fatalf("connect test database administrator: %v", err)
	}
	if _, err := admin.ExecContext(ctx, `CREATE DATABASE "`+databaseName+`"`); err != nil {
		t.Fatalf("create isolated test database: %v", err)
	}
	defer func() {
		cleanupCtx, cleanupCancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cleanupCancel()
		_, _ = admin.ExecContext(
			cleanupCtx,
			"SELECT pg_terminate_backend(pid) FROM pg_stat_activity WHERE datname = $1",
			databaseName,
		)
		if _, dropErr := admin.ExecContext(
			cleanupCtx, `DROP DATABASE IF EXISTS "`+databaseName+`"`,
		); dropErr != nil {
			t.Errorf("drop isolated test database: %v", dropErr)
		}
	}()

	candidateURL := *parsed
	candidateURL.Path = "/" + databaseName
	db, err := sql.Open("postgres", candidateURL.String())
	if err != nil {
		t.Fatal(err)
	}
	db.SetMaxOpenConns(1)
	defer db.Close()
	if err := db.PingContext(ctx); err != nil {
		t.Fatalf("connect isolated candidate database: %v", err)
	}

	var initialTables int
	if err := db.QueryRowContext(
		ctx,
		"SELECT count(*) FROM information_schema.tables WHERE table_schema = 'public' AND table_type = 'BASE TABLE'",
	).Scan(&initialTables); err != nil {
		t.Fatal(err)
	}
	if initialTables != 0 {
		t.Fatalf("isolated candidate database is not empty: %d public tables", initialTables)
	}

	migrations, err := LoadMigrations(filepath.Join("..", "..", "..", "migrations"))
	if err != nil {
		t.Fatal(err)
	}
	expectedMigrations := []string{
		"001_init",
		"002_event_signatures",
		"003_shadow_profitability_evidence",
		"004_shadow_engine_runtime",
		"005_shadow_decision_identity",
		"006_dependency_exhaustion_quarantine",
		"007_canonical_profitability_truth",
		"008_shadow_route_discovery_indexes",
		"009_profit_triggered_secondary_verification",
		"010_fork_simulation_evidence",
		"011_money_path_selective_persistence",
	}
	loadedVersions := make([]string, 0, len(migrations))
	for _, migration := range migrations {
		loadedVersions = append(loadedVersions, migration.Version)
	}
	if !reflect.DeepEqual(loadedVersions, expectedMigrations) {
		t.Fatalf("unexpected candidate migration set: %v", loadedVersions)
	}
	if err := Run(ctx, db, migrations); err != nil {
		t.Fatalf("apply fresh candidate migrations: %v", err)
	}
	if err := Run(ctx, db, migrations); err != nil {
		t.Fatalf("second candidate migration apply must be idempotent: %v", err)
	}

	rows, err := db.QueryContext(
		ctx,
		"SELECT version, checksum FROM schema_migrations ORDER BY version",
	)
	if err != nil {
		t.Fatal(err)
	}
	applied := make(map[string]string)
	for rows.Next() {
		var version string
		var checksum string
		if err := rows.Scan(&version, &checksum); err != nil {
			rows.Close()
			t.Fatal(err)
		}
		applied[version] = checksum
	}
	if err := rows.Close(); err != nil {
		t.Fatal(err)
	}
	if len(applied) != len(migrations) {
		t.Fatalf("expected %d applied migrations, got %d", len(migrations), len(applied))
	}
	for _, migration := range migrations {
		if applied[migration.Version] != migration.Checksum {
			t.Fatalf("migration checksum mismatch for %s", migration.Version)
		}
	}

	requiredColumns := map[string][]string{
		"origin_transactions":               {"tx_hash"},
		"feed_events":                       {"sequence_number"},
		"engine_outbox":                     {"outbox_id", "claim_owner", "claim_expires_at", "published_at"},
		"shadow_engine_classifications":     {"source_event_identity"},
		"shadow_decisions":                  {"execution_eligible"},
		"money_path_ingress_daily":          {"event_count"},
		"money_path_ingress_samples":        {"safe_decoder_summary"},
		"shadow_profitability_facts":        {"shadow_decision_id", "execution_request_created"},
		"shadow_engine_processing_attempts": {"source_event_identity"},
	}
	for table, columns := range requiredColumns {
		for _, column := range columns {
			var exists bool
			if err := db.QueryRowContext(
				ctx,
				`SELECT EXISTS (
					SELECT 1 FROM information_schema.columns
					WHERE table_schema = 'public' AND table_name = $1 AND column_name = $2
				)`,
				table,
				column,
			).Scan(&exists); err != nil {
				t.Fatal(err)
			}
			if !exists {
				t.Fatalf("fresh candidate schema is missing %s.%s", table, column)
			}
		}
	}

	for _, table := range []string{
		"origin_transactions",
		"feed_events",
		"engine_outbox",
		"opportunities",
		"opportunity_legs",
		"shadow_engine_processing_attempts",
		"shadow_engine_classifications",
		"shadow_decisions",
		"shadow_profitability_facts",
		"fork_simulation_results",
		"money_path_ingress_daily",
		"money_path_ingress_samples",
		"execution_attempts",
		"executions",
		"realized_pnl",
	} {
		var count int
		if err := db.QueryRowContext(ctx, "SELECT count(*) FROM "+table).Scan(&count); err != nil {
			t.Fatalf("count fresh candidate table %s: %v", table, err)
		}
		if count != 0 {
			t.Fatalf("fresh candidate table %s unexpectedly contains %d rows", table, count)
		}
	}
}

func TestRunIntegrationAndConcurrentLock(t *testing.T) {
	dsn := os.Getenv("MIGRATION_TEST_DSN")
	if dsn == "" {
		t.Skip("MIGRATION_TEST_DSN not set")
	}
	db, err := sql.Open("postgres", dsn)
	if err != nil {
		t.Fatal(err)
	}
	defer db.Close()

	ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
	defer cancel()

	suffix := time.Now().UnixNano()
	migrations := []Migration{
		{
			Version:  "001_probe",
			Checksum: "checksum-a",
			SQL:      "CREATE TABLE IF NOT EXISTS migration_runner_probe_" + time.Now().Format("20060102150405") + " (id INT PRIMARY KEY)",
		},
	}
	if err := Run(ctx, db, migrations); err != nil {
		t.Fatal(err)
	}
	if err := Run(ctx, db, migrations); err != nil {
		t.Fatalf("already applied migration should not fail: %v", err)
	}
	if _, err := db.ExecContext(ctx, "SELECT pg_advisory_lock($1)", advisoryLockKey); err != nil {
		t.Fatal(err)
	}
	defer db.ExecContext(context.Background(), "SELECT pg_advisory_unlock($1)", advisoryLockKey)
	other, err := sql.Open("postgres", dsn)
	if err != nil {
		t.Fatal(err)
	}
	defer other.Close()
	lockMigrations := []Migration{{Version: "999_lock_probe", Checksum: "checksum-lock", SQL: "SELECT " + string(rune('0'+suffix%9))}}
	err = Run(ctx, other, lockMigrations)
	if !errors.Is(err, ErrMigrationLockUnavailable) {
		t.Fatalf("expected lock unavailable, got %v", err)
	}
}

func writeMigration(t *testing.T, dir string, name string, body string) {
	t.Helper()
	if err := os.WriteFile(filepath.Join(dir, name), []byte(body), 0o600); err != nil {
		t.Fatal(err)
	}
}
