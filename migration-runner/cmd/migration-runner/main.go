package main

import (
	"context"
	"database/sql"
	"flag"
	"fmt"
	"os"
	"time"

	_ "github.com/lib/pq"

	"anti-gravity-phoenix-v4/migration-runner/internal/runner"
)

func main() {
	if err := run(); err != nil {
		fmt.Fprintf(os.Stderr, "migration-runner: %v\n", err)
		os.Exit(1)
	}
}

func run() error {
	migrationDir := flag.String("migrations", "/app/migrations", "directory containing ordered SQL migrations")
	timeout := flag.Duration("timeout", 60*time.Second, "migration timeout")
	flag.Parse()

	dsn := os.Getenv("POSTGRES_DSN")
	if dsn == "" {
		return fmt.Errorf("POSTGRES_DSN is required")
	}
	migrations, err := runner.LoadMigrations(*migrationDir)
	if err != nil {
		return err
	}
	db, err := sql.Open("postgres", dsn)
	if err != nil {
		return err
	}
	defer db.Close()

	ctx, cancel := context.WithTimeout(context.Background(), *timeout)
	defer cancel()
	return runner.Run(ctx, db, migrations)
}
