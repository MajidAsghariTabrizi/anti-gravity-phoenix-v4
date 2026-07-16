package main

import (
	"context"
	"errors"
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
	pub, err := publisher.DialJetStream(natsURL, 2*time.Second, publisher.ConnectionEvents{
		Disconnected: func() {
			readiness.MarkNATSUnavailable()
			registry.SetGauge("feed_readiness", 0)
			log.Printf("feed_jetstream_disconnected")
		},
		Reconnected: func() {
			readiness.MarkNATSReachable()
			registry.SetGauge("feed_readiness", readinessGauge(readiness))
			log.Printf("feed_jetstream_reconnected")
		},
	})
	if err != nil {
		if errors.Is(err, publisher.ErrStreamUnavailable) {
			registry.Inc("feed_jetstream_stream_unavailable_total")
		}
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
			registry.Inc("feed_sequence_duplicates_total")
			continue
		}
		if result.Gap {
			recordSequenceGap(registry, result.GapTo-result.GapFrom+1)
			readiness.MarkSequenceGap()
			log.Printf("feed_sequence_gap source=line gap_from=%d gap_to=%d", result.GapFrom, result.GapTo)
		} else {
			readiness.ClearSequenceGap()
			registry.SetGauge("feed_data_completeness", 1)
		}
		registry.SetGauge("feed_last_sequence", float64(result.Sequence))
		registry.SetGauge("feed_last_message_timestamp", float64(result.TimestampUnixMS))
		registry.Add("feed_normalized_transactions_total", uint64(len(result.Transactions)))
		if err := publishAndUpdateReadiness(ctx, pub, registry, readiness, result.Transactions, start); err != nil {
			return err
		}
	}
}

func runRelaySource(ctx context.Context, sourceCfg sourceConfig, pub publisher.Publisher, registry *metrics.Registry, readiness *metrics.Readiness) error {
	state := sequence.New()
	reconnect := reconnectBaseline{}
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
		reconnect.ObserveMessage(message.AfterReconnect)
		frames, _, err := nitro.DecodeBroadcastContext(ctx, message.Data)
		if err != nil {
			if ctx.Err() != nil {
				return ctx.Err()
			}
			recordRelayDecodeFailure(registry, readiness, issueLogger, err)
			continue
		}
		for _, frame := range frames {
			if err := processRelayFrame(
				ctx,
				frame,
				reconnect.ConsumeForFrame(),
				state,
				pub,
				registry,
				readiness,
				issueLogger,
				start,
			); err != nil {
				return err
			}
		}
		registry.SetGauge("feed_readiness", readinessGauge(readiness))
	}
}

type reconnectBaseline struct {
	pending bool
}

func (r *reconnectBaseline) ObserveMessage(afterReconnect bool) {
	if afterReconnect {
		r.pending = true
	}
}

func (r *reconnectBaseline) ConsumeForFrame() bool {
	pending := r.pending
	r.pending = false
	return pending
}

func recordRelayDecodeFailure(registry *metrics.Registry, readiness *metrics.Readiness, issueLogger *sampledIssueLogger, err error) {
	registry.Inc("feed_decode_failures_total")
	readiness.MarkIntegrityFailure("Nitro broadcast decoding integrity failure")
	registry.SetGauge("feed_readiness", 0)
	issueLogger.Log("malformed", 0, "decode Nitro broadcast: "+err.Error())
}

func processRelayFrame(
	ctx context.Context,
	frame nitro.Frame,
	afterReconnect bool,
	state *sequence.State,
	pub publisher.Publisher,
	registry *metrics.Registry,
	readiness *metrics.Readiness,
	issueLogger *sampledIssueLogger,
	start time.Time,
) error {
	recordFrameIssues(registry, issueLogger, frame)
	if len(frame.Malformed) > 0 {
		readiness.MarkIntegrityFailure("malformed Nitro feed message")
	}

	observation := state.Observe(frame.Sequence, afterReconnect)
	switch observation.Event {
	case sequence.Duplicate:
		registry.Inc("feed_duplicates_total")
		registry.Inc("feed_sequence_duplicates_total")
		issueLogger.LogSequence(observation)
		registry.SetGauge("feed_readiness", readinessGauge(readiness))
		return nil
	case sequence.Regression:
		registry.Inc("feed_out_of_order_total")
		registry.Inc("feed_sequence_regressions_total")
		readiness.MarkIntegrityFailure("Nitro feed sequence regression")
		issueLogger.LogSequence(observation)
		registry.SetGauge("feed_readiness", 0)
		return nil
	case sequence.Gap:
		recordSequenceGap(registry, observation.Missing)
		readiness.MarkSequenceGap()
		issueLogger.LogSequence(observation)
	case sequence.Reconnect:
		issueLogger.LogSequence(observation)
	}

	if state.HasUnresolvedGap() {
		readiness.MarkSequenceGap()
		registry.SetGauge("feed_data_completeness", 0)
	} else {
		readiness.ClearSequenceGap()
		registry.SetGauge("feed_data_completeness", 1)
	}
	readiness.MarkSequenceKnown()
	registry.SetGauge("feed_last_sequence", float64(frame.Sequence))
	registry.SetGauge("feed_last_message_timestamp", float64(frame.TimestampUnixMS))

	normalized, normalizationFailures := normalizeRelayTransactions(frame)
	registry.Add("feed_decode_failures_total", uint64(len(normalizationFailures)))
	if len(normalizationFailures) > 0 {
		readiness.MarkIntegrityFailure("normalized Nitro transaction integrity failure")
	}
	for _, reason := range normalizationFailures {
		issueLogger.Log("malformed", frame.Sequence, reason)
	}
	registry.Add("feed_normalized_transactions_total", uint64(len(normalized)))
	if len(normalized) == 0 {
		registry.SetGauge("feed_readiness", readinessGauge(readiness))
		return nil
	}
	return publishAndUpdateReadiness(ctx, pub, registry, readiness, normalized, start)
}

func relayLifecycleHandler(registry *metrics.Registry, readiness *metrics.Readiness) func(feed.RelayEvent) {
	return func(event feed.RelayEvent) {
		switch event.Kind {
		case feed.RelayEventConnected:
			readiness.MarkSourceConnected()
			registry.Inc("feed_connections_total")
		case feed.RelayEventDisconnected:
			readiness.MarkSourceDisconnected()
			registry.SetGauge("feed_data_completeness", 0)
			registry.SetGauge("feed_readiness", 0)
		case feed.RelayEventReconnectAttempt:
			readiness.MarkSourceDisconnected()
			registry.SetGauge("feed_data_completeness", 0)
			registry.SetGauge("feed_readiness", 0)
			registry.Inc("feed_reconnects_total")
		}
	}
}

func recordSequenceGap(registry *metrics.Registry, missing uint64) {
	registry.Inc("feed_sequence_gaps_total")
	registry.Add("feed_sequence_gap_messages_total", missing)
	registry.Add("feed_missing_sequences_total", missing)
	registry.SetGauge("feed_last_gap_timestamp_seconds", float64(time.Now().Unix()))
	registry.SetGauge("feed_data_completeness", 0)
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

func publishTransactions(ctx context.Context, pub publisher.Publisher, registry *metrics.Registry, transactions []normalizer.NormalizedTx, start time.Time) (uint64, error) {
	var published uint64
	for _, tx := range transactions {
		ackStarted := time.Now()
		if err := pub.Publish(ctx, txSubject, tx); err != nil {
			registry.Inc("feed_publish_failures_total")
			registry.Inc("feed_jetstream_publish_failures_total")
			if errors.Is(err, publisher.ErrStreamUnavailable) {
				registry.Inc("feed_jetstream_stream_unavailable_total")
			}
			log.Printf("feed_publish_failure subject=%s error=%q", txSubject, err.Error())
			return published, err
		}
		registry.Inc("feed_publish_success_total")
		registry.Inc("feed_jetstream_publish_success_total")
		registry.ObserveJetStreamPublishLatency(ackStarted)
		registry.ObserveIngestLatency(start)
		published++
	}
	return published, nil
}

func publishAndUpdateReadiness(
	ctx context.Context,
	pub publisher.Publisher,
	registry *metrics.Registry,
	readiness *metrics.Readiness,
	transactions []normalizer.NormalizedTx,
	start time.Time,
) error {
	published, err := publishTransactions(ctx, pub, registry, transactions, start)
	if err != nil {
		readiness.MarkNATSUnavailable()
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
