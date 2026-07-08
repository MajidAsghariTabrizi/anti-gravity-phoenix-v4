package metrics

import (
	"strings"
	"testing"
	"time"
)

func TestRegistryRendersCountersAndLatency(t *testing.T) {
	reg := NewRegistry()
	reg.Inc("feed_messages_total")
	reg.ObserveIngestLatency(time.Now().Add(-time.Millisecond))
	rendered := reg.Render()
	if !strings.Contains(rendered, "feed_messages_total 1") {
		t.Fatalf("missing counter: %s", rendered)
	}
	if !strings.Contains(rendered, "feed_reconnects_total 0") {
		t.Fatalf("missing zero-value required counter: %s", rendered)
	}
	if !strings.Contains(rendered, "feed_ingest_latency_seconds") {
		t.Fatalf("missing latency: %s", rendered)
	}
}

func TestReadinessRequiresSourceAndNATS(t *testing.T) {
	var ready Readiness
	if ok, reason := ready.Ready(); ok || reason != "source not initialized" {
		t.Fatalf("unexpected initial readiness ok=%v reason=%q", ok, reason)
	}
	ready.MarkSourceInitialized()
	if ok, reason := ready.Ready(); ok || reason != "NATS not reachable" {
		t.Fatalf("unexpected source-only readiness ok=%v reason=%q", ok, reason)
	}
	ready.MarkNATSReachable()
	if ok, reason := ready.Ready(); !ok || reason != "ready" {
		t.Fatalf("unexpected final readiness ok=%v reason=%q", ok, reason)
	}
	ready.MarkFatal("decoder stopped")
	if ok, reason := ready.Ready(); ok || reason != "decoder stopped" {
		t.Fatalf("unexpected fatal readiness ok=%v reason=%q", ok, reason)
	}
}
