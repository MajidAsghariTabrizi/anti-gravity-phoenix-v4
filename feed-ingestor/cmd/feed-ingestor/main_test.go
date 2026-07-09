package main

import (
	"errors"
	"strings"
	"testing"
	"time"

	"anti-gravity-phoenix-v4/feed-ingestor/internal/metrics"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/nitro"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/normalizer"
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

func TestResolveSourceConfigBlocksProductionRelayUntilLiveVerified(t *testing.T) {
	_, err := resolveSourceConfig(mapEnv(map[string]string{
		"PHOENIX_ENV":            "production",
		"PHOENIX_FEED_SOURCE":    "relay",
		"PHOENIX_FEED_RELAY_URL": "ws://nitro-feed-relay:9642/feed",
	}))
	if err == nil {
		t.Fatal("expected production relay source to fail until Nitro adapter is live-verified")
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
	err := publishTransactions(failingPublisher{}, registry, []normalizer.NormalizedTx{{TxHash: "0xabc"}}, time.Now())
	if err == nil {
		t.Fatal("expected publish failure")
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
	registry := metrics.NewRegistry()
	_, ok := normalizeRelayTransactions(nitroFrameWithChain(1), registry)
	if ok {
		t.Fatal("expected unsupported chain to fail normalization")
	}
	if !strings.Contains(registry.Render(), "feed_decode_failures_total 1") {
		t.Fatalf("missing decode failure counter: %s", registry.Render())
	}
}

type failingPublisher struct{}

func (failingPublisher) Publish(string, any) error { return errors.New("publish failed") }
func (failingPublisher) Close() error              { return nil }

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
