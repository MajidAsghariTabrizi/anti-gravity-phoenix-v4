package main

import (
	"context"
	"flag"
	"fmt"
	"log"
	"net"
	"net/http"
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
		continuous      bool
		healthAddr      string
	)
	flag.StringVar(&ledgerDir, "ledger-dir", "", "absolute directory for the append-only read-only auction ledger")
	flag.Uint64Var(&maximumAuctions, "max-auctions", observer.DefaultMaximumAuctions, "stop after this many unique auctions")
	flag.DurationVar(&maximumDuration, "max-duration", 72*time.Hour, "stop after this observation duration")
	flag.BoolVar(&continuous, "continuous", false, "run a continuous scan-only observation contract")
	flag.StringVar(&healthAddr, "health-addr", "", "optional health and metrics listen address")
	flag.Parse()
	if ledgerDir == "" {
		fmt.Fprintln(os.Stderr, "--ledger-dir is required")
		os.Exit(2)
	}
	if continuous {
		maximumAuctions = 0
		maximumDuration = 0
	}

	logger := log.New(os.Stdout, "atlas-observer ", log.LstdFlags|log.LUTC)
	ledger, err := observer.OpenLedger(ledgerDir, time.Now().UTC(), maximumAuctions, maximumDuration)
	if err != nil {
		logger.Fatalf("open ledger: %v", err)
	}
	state := ledger.Snapshot(time.Now().UTC())
	stopAt := "continuous"
	if !state.Continuous {
		stopAt = state.StopAt.Format(time.RFC3339)
	}
	logger.Printf("starting read-only gateway=%s chain_id=%s atlas=%s control=%s stop_at=%s max_auctions=%d existing=%d",
		observer.OfficialSearcherGateway,
		observer.ArbitrumChainIDHex,
		observer.ArbitrumAtlas,
		observer.ArbitrumDappControl,
		stopAt,
		state.MaximumAuctions,
		state.UniqueAuctionCount,
	)

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	var server *http.Server
	serverErrors := make(chan error, 1)
	if healthAddr != "" {
		listener, err := net.Listen("tcp", healthAddr)
		if err != nil {
			logger.Fatalf("listen for health checks: %v", err)
		}
		server = &http.Server{
			Handler:           observer.NewHealthHandler(ledger),
			ReadHeaderTimeout: 2 * time.Second,
			ReadTimeout:       5 * time.Second,
			WriteTimeout:      5 * time.Second,
			IdleTimeout:       30 * time.Second,
		}
		go func() {
			serverErrors <- server.Serve(listener)
		}()
	}
	client := observer.NewClient(ledger, logger)
	clientErrors := make(chan error, 1)
	go func() { clientErrors <- client.Run(ctx) }()
	var runErr error
	select {
	case runErr = <-clientErrors:
	case runErr = <-serverErrors:
		if runErr == http.ErrServerClosed {
			runErr = nil
		}
		cancel()
		<-clientErrors
	}
	if server != nil {
		shutdownCtx, shutdownCancel := context.WithTimeout(context.Background(), 5*time.Second)
		_ = server.Shutdown(shutdownCtx)
		shutdownCancel()
	}
	if runErr != nil {
		logger.Fatalf("observer failed: %v", runErr)
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
