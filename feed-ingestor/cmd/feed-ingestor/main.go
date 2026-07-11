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
	"anti-gravity-phoenix-v4/feed-ingestor/internal/nitro"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/normalizer"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/publisher"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/sequence"
)

const txSubject = "phoenix.feed.tx"

type sourceConfig struct {
	kind        string
	fixturePath string
	relayURL    string
}

func main() {
	if err := run(context.Background()); err != nil {
		log.Fatal(err)
	}
}

func run(ctx context.Context) error {
	registry := metrics.NewRegistry()
	readiness := &metrics.Readiness{}
	registry.SetGauge("feed_readiness", 0)
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

	natsURL := env("NATS_URL", "nats://127.0.0.1:4222")
	pub, err := publisher.DialNATSCore(natsURL, 2*time.Second)
	if err != nil {
		wrapped := fmt.Errorf("connect nats: %w", err)
		readiness.MarkFatal(wrapped.Error())
		return wrapped
	}
	defer pub.Close()
	readiness.MarkNATSReachable()

	if sourceCfg.kind == "relay" {
		return runRelaySource(ctx, sourceCfg, pub, registry, readiness)
	}
	return runLineSource(ctx, input, pub, registry, readiness)
}

func runLineSource(ctx context.Context, input io.Reader, pub publisher.Publisher, registry *metrics.Registry, readiness *metrics.Readiness) error {
	readiness.MarkSourceInitialized()
	readiness.MarkAdapterInitialized()
	readiness.MarkSourceConnected()
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
			registry.Inc("feed_decode_failures_total")
			log.Printf("feed_decode_failure source=line error=%q", err.Error())
			continue
		}
		if result.Duplicate {
			registry.Inc("feed_duplicates_total")
			continue
		}
		if result.Gap {
			registry.Inc("feed_sequence_gaps_total")
			readiness.MarkSequenceGap()
			log.Printf("feed_sequence_gap source=line gap_from=%d gap_to=%d", result.GapFrom, result.GapTo)
		} else {
			readiness.ClearSequenceGap()
		}
		registry.SetGauge("feed_last_sequence", float64(result.Sequence))
		registry.SetGauge("feed_last_message_timestamp", float64(result.TimestampUnixMS))
		registry.Add("feed_normalized_transactions_total", uint64(len(result.Transactions)))
		if err := publishAndUpdateReadiness(pub, registry, readiness, result.Transactions, start); err != nil {
			return err
		}
	}
}

func runRelaySource(ctx context.Context, sourceCfg sourceConfig, pub publisher.Publisher, registry *metrics.Registry, readiness *metrics.Readiness) error {
	state := sequence.New()
	source, err := feed.NewRelaySource(
		sourceCfg.relayURL,
		nitro.ArbitrumOneChainID,
		state.NextExpected,
		feed.RelaySourceOptions{
			Timeout: 30 * time.Second,
			Logger:  log.Default(),
			OnEvent: relayLifecycleHandler(registry, readiness),
		},
	)
	if err != nil {
		readiness.MarkFatal(err.Error())
		return err
	}
	defer source.Close()
	readiness.MarkSourceInitialized()
	readiness.MarkAdapterInitialized()
	issueLogger := newSampledIssueLogger(log.Default(), defaultIssueLogInterval, nil)

	for {
		start := time.Now()
		message, err := source.Next(ctx)
		if err != nil {
			readiness.MarkFatal(err.Error())
			return err
		}
		registry.Inc("feed_messages_total")
		frames, _, err := nitro.DecodeBroadcastContext(ctx, message.Data)
		if err != nil {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			registry.Inc("feed_decode_failures_total")
			issueLogger.Log("malformed", 0, "decode Nitro broadcast: "+err.Error())
			continue
		}
		for index, frame := range frames {
			recordFrameIssues(registry, issueLogger, frame)
			observation := state.Observe(frame.Sequence, message.AfterReconnect && index == 0)
			switch observation.Event {
			case sequence.Duplicate:
				registry.Inc("feed_duplicates_total")
				continue
			case sequence.Gap:
				registry.Inc("feed_sequence_gaps_total")
				readiness.MarkSequenceGap()
				log.Printf("feed_sequence_gap source=relay gap_from=%d gap_to=%d", observation.GapFrom, observation.GapTo)
				continue
			case sequence.FeedReset:
				registry.Inc("feed_out_of_order_total")
				readiness.MarkSequenceGap()
				log.Printf("feed_reset source=relay sequence=%d", observation.Sequence)
				continue
			case sequence.OutOfOrder:
				registry.Inc("feed_out_of_order_total")
				log.Printf("feed_out_of_order source=relay sequence=%d", observation.Sequence)
				continue
			}
			if state.HasUnresolvedGap() {
				readiness.MarkSequenceGap()
			} else {
				readiness.ClearSequenceGap()
			}
			readiness.MarkSequenceKnown()
			registry.SetGauge("feed_last_sequence", float64(frame.Sequence))
			registry.SetGauge("feed_last_message_timestamp", float64(frame.TimestampUnixMS))

			normalized, normalizationFailures := normalizeRelayTransactions(frame)
			registry.Add("feed_decode_failures_total", uint64(len(normalizationFailures)))
			for _, reason := range normalizationFailures {
				issueLogger.Log("malformed", frame.Sequence, reason)
			}
			registry.Add("feed_normalized_transactions_total", uint64(len(normalized)))
			if len(normalized) == 0 {
				registry.SetGauge("feed_readiness", readinessGauge(readiness))
				continue
			}
			if err := publishAndUpdateReadiness(pub, registry, readiness, normalized, start); err != nil {
				return err
			}
		}
		registry.SetGauge("feed_readiness", readinessGauge(readiness))
	}
}

func relayLifecycleHandler(registry *metrics.Registry, readiness *metrics.Readiness) func(feed.RelayEvent) {
	return func(event feed.RelayEvent) {
		switch event.Kind {
		case feed.RelayEventConnected:
			readiness.MarkSourceConnected()
			registry.Inc("feed_connections_total")
		case feed.RelayEventReconnectAttempt:
			registry.Inc("feed_reconnects_total")
		}
	}
}

func normalizeRelayTransactions(frame nitro.Frame) ([]normalizer.NormalizedTx, []string) {
	normalized := make([]normalizer.NormalizedTx, 0, len(frame.Transactions))
	failures := make([]string, 0)
	for index, tx := range frame.Transactions {
		n, err := normalizer.Normalize(frame.Sequence, frame.TimestampUnixMS, tx, time.Now())
		if err != nil {
			failures = append(failures, fmt.Sprintf("normalize transaction %d: %s", index, err.Error()))
			continue
		}
		normalized = append(normalized, n)
	}
	return normalized, failures
}

func publishTransactions(pub publisher.Publisher, registry *metrics.Registry, transactions []normalizer.NormalizedTx, start time.Time) (uint64, error) {
	var published uint64
	for _, tx := range transactions {
		if err := pub.Publish(txSubject, tx); err != nil {
			registry.Inc("feed_publish_failures_total")
			log.Printf("feed_publish_failure subject=%s error=%q", txSubject, err.Error())
			return published, err
		}
		registry.Inc("feed_publish_success_total")
		registry.ObserveIngestLatency(start)
		published++
	}
	return published, nil
}

func publishAndUpdateReadiness(
	pub publisher.Publisher,
	registry *metrics.Registry,
	readiness *metrics.Readiness,
	transactions []normalizer.NormalizedTx,
	start time.Time,
) error {
	published, err := publishTransactions(pub, registry, transactions, start)
	if err != nil {
		readiness.MarkFatal(err.Error())
		registry.SetGauge("feed_readiness", readinessGauge(readiness))
		return err
	}
	if published > 0 {
		readiness.MarkSuccessfulPublish()
	}
	registry.SetGauge("feed_readiness", readinessGauge(readiness))
	return nil
}

func readinessGauge(readiness *metrics.Readiness) float64 {
	if ok, _ := readiness.Ready(); ok {
		return 1
	}
	return 0
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
		return sourceConfig{kind: "relay", relayURL: relayURL}, nil
	}
	switch source {
	case "relay":
		if relayURL == "" {
			return sourceConfig{}, fmt.Errorf("PHOENIX_FEED_RELAY_URL is required when PHOENIX_FEED_SOURCE=relay")
		}
		return sourceConfig{kind: "relay", relayURL: relayURL}, nil
	case "fixture":
		if fixturePath == "" {
			return sourceConfig{}, fmt.Errorf("PHOENIX_FEED_FIXTURE is required when PHOENIX_FEED_SOURCE=fixture")
		}
		return sourceConfig{kind: "fixture", fixturePath: fixturePath}, nil
	case "stdin", "":
	default:
		return sourceConfig{}, fmt.Errorf("unsupported PHOENIX_FEED_SOURCE %q", source)
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
