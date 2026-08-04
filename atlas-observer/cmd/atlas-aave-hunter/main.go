package main

import (
	"context"
	"encoding/json"
	"flag"
	"fmt"
	"log"
	"net"
	"net/http"
	"os"
	"os/signal"
	"path/filepath"
	"syscall"
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/hunter"
	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
)

func main() {
	var ledgerDir, discovery, discoveryHash, gateway, healthAddr string
	var startingCursor uint64
	var batch int
	var pace time.Duration
	var retainedProfitFloor string
	var maximumGasLimit, flashPremiumBPS, economicReserveBPS uint64
	var maximumFeePerGasWei string
	var executorAddress, executorCodeHash, callerAddress, releaseSHA, maximumPriorityFeeWei string
	flag.StringVar(&ledgerDir, "ledger-dir", "", "absolute durable Atlas ledger directory")
	flag.StringVar(&discovery, "aave-discovery", "", "absolute immutable borrower discovery seed")
	flag.StringVar(&discoveryHash, "aave-discovery-sha256", "", "expected immutable discovery seed SHA-256")
	flag.StringVar(&gateway, "rpc-gateway-url", "http://rpc-gateway:9300", "internal Phoenix RPC Gateway")
	flag.StringVar(&healthAddr, "health-addr", "0.0.0.0:9700", "health listen address")
	flag.Uint64Var(&startingCursor, "aave-starting-cursor", 1100, "minimum preserved screening cursor")
	flag.IntVar(&batch, "aave-batch-size", hunter.DefaultBatch, "bounded Aave screen batch")
	flag.DurationVar(&pace, "aave-pace", 60*time.Second, "minimum time between screen batches")
	flag.StringVar(&retainedProfitFloor, "retained-profit-floor-wei", "", "strict Production retained-profit floor in WETH wei")
	flag.Uint64Var(&maximumGasLimit, "maximum-gas-limit", 0, "maximum liquidation gas charged by the conservative gate")
	flag.StringVar(&maximumFeePerGasWei, "maximum-fee-per-gas-wei", "", "maximum gas price charged by the conservative gate")
	flag.Uint64Var(&flashPremiumBPS, "flash-premium-bps", 9, "conservative flash premium in basis points")
	flag.Uint64Var(&economicReserveBPS, "economic-reserve-bps", 500, "combined failure, latency, drift, and model reserve")
	flag.StringVar(&executorAddress, "executor-address", "", "exact deployed PhoenixExecutor address")
	flag.StringVar(&executorCodeHash, "executor-code-hash", "", "exact deployed PhoenixExecutor SHA-256 code hash")
	flag.StringVar(&callerAddress, "caller-address", "", "signer-derived executor caller address")
	flag.StringVar(&releaseSHA, "release-sha", "", "exact protected release SHA")
	flag.StringVar(&maximumPriorityFeeWei, "maximum-priority-fee-per-gas-wei", "", "maximum priority fee for materialized liquidation requests")
	flag.Parse()
	if ledgerDir == "" || discovery == "" || discoveryHash == "" || !filepath.IsAbs(ledgerDir) {
		fmt.Fprintln(os.Stderr, "durable Atlas and immutable Aave inputs are required")
		os.Exit(2)
	}

	logger := log.New(os.Stdout, "atlas-aave-hunter ", log.LstdFlags|log.LUTC)
	sink, err := hunter.OpenPostgresSignalSink(context.Background(), os.Getenv("POSTGRES_DSN"), retainedProfitFloor)
	if err != nil {
		logger.Fatalf("open durable hunting sink: %v", err)
	}
	defer sink.Close()
	ledger, err := observer.OpenLedger(filepath.Join(ledgerDir, "atlas"), time.Now().UTC(), 0, 0)
	if err != nil {
		logger.Fatalf("open Atlas ledger: %v", err)
	}
	screener, err := hunter.New(hunter.Config{
		DiscoveryPath: discovery, DiscoverySHA256: discoveryHash,
		StateDir: filepath.Join(ledgerDir, "aave"), GatewayURL: gateway,
		StartingCursor: startingCursor, BatchSize: batch, Pace: pace,
		RetainedProfitFloorWei: retainedProfitFloor,
		MaximumGasLimit:        maximumGasLimit,
		MaximumFeePerGasWei:    maximumFeePerGasWei,
		FlashPremiumBPS:        flashPremiumBPS,
		EconomicReserveBPS:     economicReserveBPS,
		ExecutorAddress:        executorAddress,
		ExecutorCodeHash:       executorCodeHash,
		CallerAddress:          callerAddress,
		ReleaseSHA:             releaseSHA,
		MaximumPriorityFeeWei:  maximumPriorityFeeWei,
		SignalSink:             sink,
	})
	if err != nil {
		logger.Fatalf("open Aave screener: %v", err)
	}

	ctx, cancel := signal.NotifyContext(context.Background(), os.Interrupt, syscall.SIGTERM)
	defer cancel()
	atlasErrors := make(chan error, 1)
	aaveErrors := make(chan error, 1)
	atlasCandidateErrors := make(chan error, 1)
	go func() { atlasErrors <- observer.NewClientWithSink(ledger, logger, sink).Run(ctx) }()
	go func() { aaveErrors <- screener.Run(ctx) }()
	go func() {
		for {
			select {
			case <-ctx.Done():
				atlasCandidateErrors <- nil
				return
			case auction := <-sink.AtlasAuctions():
				if err := screener.HandleAtlasAuction(ctx, auction); err != nil {
					atlasCandidateErrors <- err
					return
				}
			}
		}
	}()

	listener, err := net.Listen("tcp", healthAddr)
	if err != nil {
		logger.Fatalf("listen: %v", err)
	}
	server := &http.Server{Handler: healthHandler(ledger, screener), ReadHeaderTimeout: 2 * time.Second, ReadTimeout: 5 * time.Second, WriteTimeout: 5 * time.Second, IdleTimeout: 30 * time.Second}
	serverErrors := make(chan error, 1)
	go func() { serverErrors <- server.Serve(listener) }()
	logger.Printf("continuous lanes started cursor=%d batch=%d signer_present=false execution_request_materialization=true", screener.Snapshot().Cursor, batch)

	var runErr error
	select {
	case runErr = <-atlasErrors:
	case runErr = <-aaveErrors:
	case runErr = <-atlasCandidateErrors:
	case runErr = <-serverErrors:
		if runErr == http.ErrServerClosed {
			runErr = nil
		}
	}
	cancel()
	shutdown, shutdownCancel := context.WithTimeout(context.Background(), 5*time.Second)
	_ = server.Shutdown(shutdown)
	shutdownCancel()
	if runErr != nil {
		logger.Fatalf("hunter failed: %v", runErr)
	}
}

func healthHandler(ledger *observer.Ledger, screener *hunter.Screener) http.Handler {
	atlasHandler := observer.NewHealthHandler(ledger)
	mux := http.NewServeMux()
	mux.Handle("/metrics", atlasHandler)
	mux.HandleFunc("/healthz", func(writer http.ResponseWriter, request *http.Request) {
		state := screener.Snapshot()
		status := http.StatusOK
		if state.Cursor < 1100 || state.LastErrorClass != "" {
			status = http.StatusServiceUnavailable
		}
		writer.Header().Set("Content-Type", "application/json")
		writer.WriteHeader(status)
		_ = json.NewEncoder(writer).Encode(map[string]any{"ok": status == http.StatusOK, "aave_cursor": state.Cursor, "aave_tail_next_block": state.TailNextBlock, "aave_debt_bearing": state.DebtBearingCount, "aave_exact_queue": state.ExactQueueCount})
	})
	mux.HandleFunc("/readyz", func(writer http.ResponseWriter, request *http.Request) {
		atlas := ledger.Snapshot(time.Now().UTC())
		aave := screener.Snapshot()
		fresh := aave.LastBatchAt != nil && time.Since(*aave.LastBatchAt) < 10*time.Minute
		tailFresh := aave.LastTailAt != nil && time.Since(*aave.LastTailAt) < 10*time.Minute
		status := http.StatusOK
		if !atlas.Connected || atlas.LastSubscriptionAt == nil || atlas.InvalidCount > 0 || atlas.Completed || !fresh || !tailFresh || aave.LastErrorClass != "" {
			status = http.StatusServiceUnavailable
		}
		writer.Header().Set("Content-Type", "application/json")
		writer.WriteHeader(status)
		_ = json.NewEncoder(writer).Encode(map[string]any{"ok": status == http.StatusOK, "atlas_connected": atlas.Connected, "aave_cursor": aave.Cursor, "aave_tail_next_block": aave.TailNextBlock, "aave_debt_bearing": aave.DebtBearingCount, "aave_exact_queue": aave.ExactQueueCount, "signer_present": false})
	})
	return mux
}
