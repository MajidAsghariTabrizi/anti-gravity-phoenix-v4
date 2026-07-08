package runner

import (
	"context"
	"crypto/sha256"
	"database/sql"
	"encoding/hex"
	"errors"
	"fmt"
	"os"
	"path/filepath"
	"sort"
	"strings"
)

const advisoryLockKey int64 = 0x50484f454e4958

var ErrMigrationLockUnavailable = errors.New("migration lock unavailable")

type Migration struct {
	Version  string
	Path     string
	Checksum string
	SQL      string
}

func LoadMigrations(dir string) ([]Migration, error) {
	entries, err := os.ReadDir(dir)
	if err != nil {
		return nil, err
	}
	var names []string
	for _, entry := range entries {
		if entry.IsDir() || !strings.HasSuffix(entry.Name(), ".sql") {
			continue
		}
		names = append(names, entry.Name())
	}
	sort.Strings(names)
	migrations := make([]Migration, 0, len(names))
	seen := make(map[string]struct{}, len(names))
	for _, name := range names {
		version := strings.TrimSuffix(name, ".sql")
		if _, ok := seen[version]; ok {
			return nil, fmt.Errorf("duplicate migration version %s", version)
		}
		seen[version] = struct{}{}
		path := filepath.Join(dir, name)
		body, err := os.ReadFile(path)
		if err != nil {
			return nil, err
		}
		sum := sha256.Sum256(body)
		migrations = append(migrations, Migration{
			Version:  version,
			Path:     path,
			Checksum: hex.EncodeToString(sum[:]),
			SQL:      string(body),
		})
	}
	return migrations, nil
}

func Pending(applied map[string]string, migrations []Migration) ([]Migration, error) {
	pending := make([]Migration, 0, len(migrations))
	for _, migration := range migrations {
		checksum, ok := applied[migration.Version]
		if !ok {
			pending = append(pending, migration)
			continue
		}
		if checksum != migration.Checksum {
			return nil, fmt.Errorf("migration %s checksum changed after apply", migration.Version)
		}
	}
	return pending, nil
}

func Run(ctx context.Context, db *sql.DB, migrations []Migration) error {
	if err := ensureSchema(ctx, db); err != nil {
		return err
	}
	locked, err := acquireLock(ctx, db)
	if err != nil {
		return err
	}
	if !locked {
		return ErrMigrationLockUnavailable
	}
	defer func() {
		_, _ = db.ExecContext(context.Background(), "SELECT pg_advisory_unlock($1)", advisoryLockKey)
	}()

	applied, err := loadApplied(ctx, db)
	if err != nil {
		return err
	}
	pending, err := Pending(applied, migrations)
	if err != nil {
		return err
	}
	for _, migration := range pending {
		if err := applyMigration(ctx, db, migration); err != nil {
			return err
		}
	}
	return nil
}

func ensureSchema(ctx context.Context, db *sql.DB) error {
	_, err := db.ExecContext(ctx, `
CREATE TABLE IF NOT EXISTS schema_migrations (
    version TEXT PRIMARY KEY,
    checksum TEXT NOT NULL,
    applied_at TIMESTAMPTZ NOT NULL DEFAULT now()
)`)
	return err
}

func acquireLock(ctx context.Context, db *sql.DB) (bool, error) {
	var locked bool
	err := db.QueryRowContext(ctx, "SELECT pg_try_advisory_lock($1)", advisoryLockKey).Scan(&locked)
	return locked, err
}

func loadApplied(ctx context.Context, db *sql.DB) (map[string]string, error) {
	rows, err := db.QueryContext(ctx, "SELECT version, checksum FROM schema_migrations")
	if err != nil {
		return nil, err
	}
	defer rows.Close()
	applied := make(map[string]string)
	for rows.Next() {
		var version string
		var checksum string
		if err := rows.Scan(&version, &checksum); err != nil {
			return nil, err
		}
		applied[version] = checksum
	}
	return applied, rows.Err()
}

func applyMigration(ctx context.Context, db *sql.DB, migration Migration) error {
	tx, err := db.BeginTx(ctx, nil)
	if err != nil {
		return err
	}
	defer tx.Rollback()
	if _, err := tx.ExecContext(ctx, migration.SQL); err != nil {
		return fmt.Errorf("apply %s: %w", migration.Version, err)
	}
	if _, err := tx.ExecContext(ctx, "INSERT INTO schema_migrations (version, checksum) VALUES ($1, $2)", migration.Version, migration.Checksum); err != nil {
		return fmt.Errorf("record %s: %w", migration.Version, err)
	}
	return tx.Commit()
}
