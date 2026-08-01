package main

import (
	"flag"
	"fmt"
	"os"
	"time"

	"anti-gravity-phoenix-v4/atlas-observer/internal/observer"
)

func main() {
	var ledgerDir string
	flag.StringVar(&ledgerDir, "ledger-dir", "", "absolute private directory containing the auction ledger")
	flag.Parse()
	if ledgerDir == "" {
		fmt.Fprintln(os.Stderr, "--ledger-dir is required")
		os.Exit(2)
	}
	transcript, raw, err := observer.DecodeRPCTranscript(os.Stdin)
	if err != nil {
		fmt.Fprintf(os.Stderr, "decode public RPC transcript: %v\n", err)
		os.Exit(1)
	}
	records, err := observer.ReconcileTranscript(ledgerDir, transcript, raw, time.Now().UTC())
	if err != nil {
		fmt.Fprintf(os.Stderr, "reconcile public chain evidence: %v\n", err)
		os.Exit(1)
	}
	settled := 0
	liquidations := 0
	for _, record := range records {
		if record.PublicSettlementFound {
			settled++
		}
		liquidations += len(record.PublicLiquidations)
	}
	fmt.Printf("reconciled=%d settlements=%d public_liquidations=%d\n", len(records), settled, liquidations)
}
