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
	if !strings.Contains(rendered, "feed_ingest_latency_seconds") {
		t.Fatalf("missing latency: %s", rendered)
	}
}
