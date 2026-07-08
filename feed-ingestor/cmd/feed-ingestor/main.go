package main

import (
	"context"
	"fmt"
	"io"
	"log"
	"net/http"
	"os"
	"time"

	"anti-gravity-phoenix-v4/feed-ingestor/internal/decoder"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/feed"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/metrics"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/publisher"
)

const txSubject = "phoenix.feed.tx"

func main() {
	if err := run(context.Background()); err != nil {
		log.Fatal(err)
	}
}

func run(ctx context.Context) error {
	registry := metrics.NewRegistry()
	metricsAddr := env("METRICS_ADDR", "0.0.0.0:9100")
	go func() {
		mux := http.NewServeMux()
		mux.Handle("/metrics", registry.Handler())
		_ = http.ListenAndServe(metricsAddr, mux)
	}()

	var input io.ReadCloser
	fixturePath := os.Getenv("PHOENIX_FEED_FIXTURE")
	if fixturePath != "" {
		f, err := os.Open(fixturePath)
		if err != nil {
			return err
		}
		input = f
	} else {
		input = io.NopCloser(os.Stdin)
	}
	defer input.Close()

	natsURL := env("NATS_URL", "nats://127.0.0.1:4222")
	pub, err := publisher.DialNATSCore(natsURL, 2*time.Second)
	if err != nil {
		return fmt.Errorf("connect nats: %w", err)
	}
	defer pub.Close()

	source := feed.NewLineSource(input)
	ordered := decoder.NewOrderedDecoder(time.Now)

	for {
		start := time.Now()
		raw, err := source.Next(ctx)
		if err == io.EOF {
			return nil
		}
		if err != nil {
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

func env(key, fallback string) string {
	value := os.Getenv(key)
	if value == "" {
		return fallback
	}
	return value
}
