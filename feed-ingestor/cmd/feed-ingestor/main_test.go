package main

import (
	"errors"
	"strings"
	"testing"
	"time"

	"anti-gravity-phoenix-v4/feed-ingestor/internal/feed"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/metrics"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/nitro"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/normalizer"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/publisher"
)

func TestResolveSourceConfigBlocksProductionFixture(t *testing.T) {
	_, err := resolveSourceConfig(mapEnv(map[string]string{
		"PHOENIX_ENV":          "production",
		"PHOENIX_FEED_FIXTURE": "/app/fixtures/feed/profitable.ndjson",
	}))
	if err == nil {
		t.Fatal("expected production fixture source to fail")
	}
}

func TestResolveSourceConfigAllowsProductionRelay(t *testing.T) {
	cfg, err := resolveSourceConfig(mapEnv(map[string]string{
		"PHOENIX_ENV":            "production",
		"PHOENIX_FEED_SOURCE":    "relay",
		"PHOENIX_FEED_RELAY_URL": "ws://nitro-feed-relay:9642/feed",
	}))
	if err != nil {
		t.Fatalf("unexpected production relay error: %v", err)
	}
	if cfg.kind != "relay" || cfg.relayURL != "ws://nitro-feed-relay:9642/feed" {
		t.Fatalf("unexpected config: %+v", cfg)
	}
}

func TestResolveSourceConfigAllowsShadowFixture(t *testing.T) {
	cfg, err := resolveSourceConfig(mapEnv(map[string]string{
		"PHOENIX_FEED_FIXTURE": "fixtures/feed/profitable.ndjson",
	}))
	if err != nil {
		t.Fatalf("unexpected fixture error: %v", err)
	}
	if cfg.kind != "fixture" || cfg.fixturePath != "fixtures/feed/profitable.ndjson" {
		t.Fatalf("unexpected config: %+v", cfg)
	}
}

func TestResolveSourceConfigAllowsShadowRelay(t *testing.T) {
	cfg, err := resolveSourceConfig(mapEnv(map[string]string{
		"PHOENIX_FEED_SOURCE":    "relay",
		"PHOENIX_FEED_RELAY_URL": "ws://nitro-feed-relay:9642/feed",
	}))
	if err != nil {
		t.Fatalf("unexpected relay error: %v", err)
	}
	if cfg.kind != "relay" || cfg.relayURL != "ws://nitro-feed-relay:9642/feed" {
		t.Fatalf("unexpected config: %+v", cfg)
	}
}

func TestPublishTransactionsCountsFailure(t *testing.T) {
	registry := metrics.NewRegistry()
	published, err := publishTransactions(failingPublisher{}, registry, []normalizer.NormalizedTx{{TxHash: "0xabc"}}, time.Now())
	if err == nil {
		t.Fatal("expected publish failure")
	}
	if published != 0 {
		t.Fatalf("failed publish counted as successful: %d", published)
	}
	rendered := registry.Render()
	if !strings.Contains(rendered, "feed_publish_failures_total 1") {
		t.Fatalf("missing publish failure counter: %s", rendered)
	}
	if !strings.Contains(rendered, "feed_publish_success_total 0") {
		t.Fatalf("unexpected publish success counter: %s", rendered)
	}
}

func TestNormalizeRelayTransactionsRejectsUnsupportedChain(t *testing.T) {
	normalized, failures := normalizeRelayTransactions(nitroFrameWithChain(1))
	if len(normalized) != 0 || len(failures) != 1 || !strings.Contains(failures[0], "unsupported chain id") {
		t.Fatalf("expected unsupported chain to fail normalization: normalized=%+v failures=%+v", normalized, failures)
	}
}

func TestNormalizeRelayTransactionsKeepsValidSibling(t *testing.T) {
	valid := nitroFrameWithChain(nitro.ArbitrumOneChainID).Transactions[0]
	invalid := nitroFrameWithChain(1).Transactions[0]
	frame := nitro.Frame{Sequence: 1, TimestampUnixMS: 1700000000000, Transactions: []normalizer.RelayTx{valid, invalid}}
	normalized, failures := normalizeRelayTransactions(frame)
	if len(normalized) != 1 || len(failures) != 1 {
		t.Fatalf("expected one valid sibling and one failure: normalized=%+v failures=%+v", normalized, failures)
	}
}

func TestRelayLifecycleCountsReconnectsWithoutClaimingReadiness(t *testing.T) {
	registry := metrics.NewRegistry()
	readiness := &metrics.Readiness{}
	readiness.MarkSourceInitialized()
	readiness.MarkAdapterInitialized()
	readiness.MarkNATSReachable()
	handle := relayLifecycleHandler(registry, readiness)

	handle(feed.RelayEvent{Kind: feed.RelayEventConnected, Attempt: 1})
	handle(feed.RelayEvent{Kind: feed.RelayEventReconnectAttempt, Attempt: 2, Backoff: time.Millisecond})
	handle(feed.RelayEvent{Kind: feed.RelayEventConnected, Attempt: 2, Reconnected: true})

	rendered := registry.Render()
	if !strings.Contains(rendered, "feed_connections_total 2") {
		t.Fatalf("connection metric does not reflect successful handshakes: %s", rendered)
	}
	if !strings.Contains(rendered, "feed_reconnects_total 1") {
		t.Fatalf("reconnect metric does not reflect retry attempts: %s", rendered)
	}
	if !strings.Contains(rendered, "feed_messages_total 0") || !strings.Contains(rendered, "feed_decode_failures_total 0") {
		t.Fatalf("lifecycle events must not count as delivered or rejected messages: %s", rendered)
	}
	if ok, reason := readiness.Ready(); ok || reason != "no successful feed transaction published" {
		t.Fatalf("readiness must remain false before valid live evidence, ok=%v reason=%q", ok, reason)
	}
}

func TestReadinessBecomesTrueOnlyAfterSuccessfulPublish(t *testing.T) {
	registry := metrics.NewRegistry()
	readiness := publishReadyState()
	pub := &publisher.MemoryPublisher{}
	transactions := []normalizer.NormalizedTx{{TxHash: "0xabc"}}

	if ok, _ := readiness.Ready(); ok {
		t.Fatal("readiness was true before publication")
	}
	if err := publishAndUpdateReadiness(pub, registry, readiness, transactions, time.Now()); err != nil {
		t.Fatal(err)
	}
	if ok, reason := readiness.Ready(); !ok || reason != "ready" {
		t.Fatalf("successful publish did not enable readiness ok=%v reason=%q", ok, reason)
	}
	rendered := registry.Render()
	if !strings.Contains(rendered, "feed_publish_success_total 1") || !strings.Contains(rendered, "feed_readiness 1") {
		t.Fatalf("successful publish metrics mismatch: %s", rendered)
	}
}

func TestPublishFailureRemainsFailClosed(t *testing.T) {
	registry := metrics.NewRegistry()
	readiness := publishReadyState()
	transactions := []normalizer.NormalizedTx{{TxHash: "0xabc"}}

	if err := publishAndUpdateReadiness(failingPublisher{}, registry, readiness, transactions, time.Now()); err == nil {
		t.Fatal("expected publish failure")
	}
	if ok, _ := readiness.Ready(); ok {
		t.Fatal("publish failure enabled readiness")
	}
	rendered := registry.Render()
	if !strings.Contains(rendered, "feed_publish_failures_total 1") || !strings.Contains(rendered, "feed_publish_success_total 0") || !strings.Contains(rendered, "feed_readiness 0") {
		t.Fatalf("failed publish metrics mismatch: %s", rendered)
	}
}

type failingPublisher struct{}

func (failingPublisher) Publish(string, any) error { return errors.New("publish failed") }
func (failingPublisher) Close() error              { return nil }

func publishReadyState() *metrics.Readiness {
	readiness := &metrics.Readiness{}
	readiness.MarkSourceInitialized()
	readiness.MarkAdapterInitialized()
	readiness.MarkSourceConnected()
	readiness.MarkNATSReachable()
	readiness.MarkSequenceKnown()
	return readiness
}

func nitroFrameWithChain(chainID uint64) nitro.Frame {
	return nitro.Frame{
		Sequence:        1,
		TimestampUnixMS: 1700000000000,
		Transactions: []normalizer.RelayTx{
			{
				Hash:                 "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
				Type:                 "0x65",
				ChainID:              chainID,
				From:                 "0x1111111111111111111111111111111111111111",
				To:                   "0x2222222222222222222222222222222222222222",
				Nonce:                7,
				Value:                "0",
				Calldata:             "0x1234",
				GasLimit:             "21000",
				MaxFeePerGas:         "100",
				MaxPriorityFeePerGas: "0",
				RawTx:                "0x0102",
			},
		},
	}
}

func mapEnv(values map[string]string) func(string) string {
	return func(key string) string {
		return values[key]
	}
}
