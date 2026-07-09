package main

import (
	"context"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"strings"
	"time"

	"anti-gravity-phoenix-v4/feed-ingestor/internal/decoder"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/feed"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/metrics"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/publisher"
)

const txSubject = "phoenix.feed.tx"

type sourceConfig struct {
	kind        string
	fixturePath string
}

func main() {
	if err := run(context.Background()); err != nil {
		log.Fatal(err)
	}
}

func run(ctx context.Context) error {
	registry := metrics.NewRegistry()
	readiness := &metrics.Readiness{}
	metricsAddr := env("METRICS_ADDR", "0.0.0.0:9100")
	go func() {
		mux := http.NewServeMux()
		mux.Handle("/metrics", registry.Handler())
		mux.Handle("/healthz", readiness.HealthHandler())
		mux.Handle("/readyz", readiness.ReadyHandler())
		_ = http.ListenAndServe(metricsAddr, mux)
	}()

	sourceCfg, err := resolveSourceConfig(os.Getenv)
	if err != nil {
		readiness.MarkFatal(err.Error())
		return err
	}

	var input io.ReadCloser
	if sourceCfg.kind == "fixture" {
		f, err := os.Open(sourceCfg.fixturePath)
		if err != nil {
			return err
		}
		input = f
	} else {
		input = io.NopCloser(os.Stdin)
	}
	defer input.Close()
	readiness.MarkSourceInitialized()

	natsURL := env("NATS_URL", "nats://127.0.0.1:4222")
	pub, err := publisher.DialNATSCore(natsURL, 2*time.Second)
	if err != nil {
		return fmt.Errorf("connect nats: %w", err)
	}
	defer pub.Close()
	readiness.MarkNATSReachable()

	source := feed.NewLineSource(input)
	ordered := decoder.NewOrderedDecoder(time.Now)

	for {
		start := time.Now()
		raw, err := source.Next(ctx)
		if err == io.EOF {
			return nil
		}
		if err != nil {
			readiness.MarkFatal(err.Error())
			return err
		}
		registry.Inc("feed_messages_total")
		result, err := ordered.DecodeJSONFrame(raw)
		if err != nil {
			registry.Inc("feed_decode_errors_total")
			log.Printf("decode error: %v", err)
			continue
		}
		if result.Duplicate {
			registry.Inc("feed_duplicates_total")
			continue
		}
		if result.Gap {
			registry.Inc("feed_sequence_gaps_total")
			log.Printf("sequence gap from %d to %d", result.GapFrom, result.GapTo)
		}
		for _, tx := range result.Transactions {
			if err := pub.Publish(txSubject, tx); err != nil {
				return err
			}
			registry.Inc("feed_transactions_total")
			registry.ObserveIngestLatency(start)
		}
	}
}

func resolveSourceConfig(getenv func(string) string) (sourceConfig, error) {
	production := strings.EqualFold(getenv("PHOENIX_ENV"), "production")
	fixturePath := strings.TrimSpace(getenv("PHOENIX_FEED_FIXTURE"))
	source := strings.ToLower(strings.TrimSpace(getenv("PHOENIX_FEED_SOURCE")))
	relayURL := strings.TrimSpace(getenv("PHOENIX_FEED_RELAY_URL"))

	if production && fixturePath != "" {
		return sourceConfig{}, fmt.Errorf("production feed readiness blocked: PHOENIX_FEED_FIXTURE is set")
	}
	if production {
		if source != "relay" {
			return sourceConfig{}, fmt.Errorf("production feed readiness blocked: PHOENIX_FEED_SOURCE must be relay")
		}
		if relayURL == "" {
			return sourceConfig{}, fmt.Errorf("production feed readiness blocked: PHOENIX_FEED_RELAY_URL is required")
		}
		return sourceConfig{}, fmt.Errorf("production feed readiness blocked: official Nitro relay adapter is not implemented or verified")
	}
	if fixturePath != "" {
		return sourceConfig{kind: "fixture", fixturePath: fixturePath}, nil
	}
	return sourceConfig{kind: "stdin"}, nil
}

func env(key, fallback string) string {
	value := os.Getenv(key)
	if value == "" {
		return fallback
	}
	return value
}
