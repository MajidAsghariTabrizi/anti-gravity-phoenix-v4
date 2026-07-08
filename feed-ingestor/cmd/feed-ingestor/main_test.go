package main

import "testing"

func TestResolveSourceConfigBlocksProductionFixture(t *testing.T) {
	_, err := resolveSourceConfig(mapEnv(map[string]string{
		"PHOENIX_ENV":          "production",
		"PHOENIX_FEED_FIXTURE": "/app/fixtures/feed/profitable.ndjson",
	}))
	if err == nil {
		t.Fatal("expected production fixture source to fail")
	}
}

func TestResolveSourceConfigBlocksProductionRelayUntilImplemented(t *testing.T) {
	_, err := resolveSourceConfig(mapEnv(map[string]string{
		"PHOENIX_ENV":            "production",
		"PHOENIX_FEED_SOURCE":    "relay",
		"PHOENIX_FEED_RELAY_URL": "ws://nitro-feed-relay:9642/feed",
	}))
	if err == nil {
		t.Fatal("expected production relay source to fail until Nitro adapter is implemented")
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

func mapEnv(values map[string]string) func(string) string {
	return func(key string) string {
		return values[key]
	}
}
