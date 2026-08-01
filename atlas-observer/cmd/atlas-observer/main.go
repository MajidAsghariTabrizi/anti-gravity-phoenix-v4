package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"os"
	"os/signal"
	"syscall"
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
)

func main() {
	var (
		ledgerDir       string
		maximumAuctions uint64
		maximumDuration time.Duration
	)
	flag.StringVar(&ledgerDir, "ledger-dir", "", "absolute directory for the append-only read-only auction ledger")
	flag.Uint64Var(&maximumAuctions, "max-auctions", observer.DefaultMaximumAuctions, "stop after this many unique auctions")
	flag.DurationVar(&maximumDuration, "max-duration", 72*time.Hour, "stop after this observation duration")
	flag.Parse()
	if ledgerDir == "" {
		fmt.Fprintln(os.Stderr, "--ledger-dir is required")
		os.Exit(2)
	}

	logger := log.New(os.Stdout, "atlas-observer ", log.LstdFlags|log.LUTC)
	ledger, err := observer.OpenLedger(ledgerDir, time.Now().UTC(), maximumAuctions, maximumDuration)
	if err != nil {
		logger.Fatalf("open ledger: %v", err)
	}
	state := ledger.Snapshot(time.Now().UTC())
	logger.Printf("starting read-only gateway=%s chain_id=%s atlas=%s control=%s stop_at=%s max_auctions=%d existing=%d",
		observer.OfficialSearcherGateway,
		observer.ArbitrumChainIDHex,
		observer.ArbitrumAtlas,
		observer.ArbitrumDappControl,
		state.StopAt.Format(time.RFC3339),
		state.MaximumAuctions,
		state.UniqueAuctionCount,
	)

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	client := observer.NewClient(ledger, logger)
	if err := client.Run(ctx); err != nil {
		logger.Fatalf("observer failed: %v", err)
	}
	final := ledger.Snapshot(time.Now().UTC())
	logger.Printf("observer stopped completed=%t reason=%s auctions=%d relevant_aave=%d invalid=%d duplicates=%d",
		final.Completed,
		final.CompletionReason,
		final.UniqueAuctionCount,
		final.RelevantAaveCount,
		final.InvalidCount,
		final.DuplicateCount,
	)
}
