package runner

import (
	"context"
	"database/sql"
	"errors"
	"os"
	"path/filepath"
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
