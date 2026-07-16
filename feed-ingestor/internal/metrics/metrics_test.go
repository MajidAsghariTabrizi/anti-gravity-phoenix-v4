package metrics

import (
	"strings"
	"testing"
	"time"

	"anti-gravity-phoenix-v4/feed-ingestor/internal/nitro"
)

func TestRegistryRendersCountersAndLatency(t *testing.T) {
	reg := NewRegistry()
	reg.Inc("feed_messages_total")
	reg.ObserveIngestLatency(time.Now().Add(-time.Millisecond))
	reg.ObserveJetStreamPublishLatency(time.Now().Add(-time.Millisecond))
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
	if !strings.Contains(rendered, "feed_jetstream_publish_latency_count 1") {
		t.Fatalf("missing JetStream acknowledgement latency: %s", rendered)
	}
	if !strings.Contains(rendered, "feed_jetstream_publish_success_total 0") {
		t.Fatalf("missing JetStream counters: %s", rendered)
	}
	for _, required := range []string{
		"feed_sequence_gap_messages_total 0",
		"feed_sequence_regressions_total 0",
		"feed_sequence_duplicates_total 0",
	} {
		if !strings.Contains(rendered, required) {
			t.Fatalf("missing sequence counter %q: %s", required, rendered)
		}
	}
}

func TestRegistryRendersOnlyBoundedStructuredMessageKindLabels(t *testing.T) {
	reg := NewRegistry()
	reg.IncUnsupportedMessageKind(nitro.MessageKind{Layer: nitro.MessageLayerL2, Kind: 0x7f})
	reg.IncIgnoredMessageKind(nitro.MessageKind{Layer: nitro.MessageLayerL1, Kind: nitro.L1MessageTypeEndOfBlock})
	reg.IncUnsupportedMessageKind(nitro.MessageKind{Layer: 99, Kind: 1})

	rendered := reg.Render()
	for _, expected := range []string{
		`feed_message_kind_total{classification="unsupported",layer="l2",kind="127"} 1`,
		`feed_message_kind_total{classification="ignored",layer="l1",kind="6"} 1`,
	} {
		if !strings.Contains(rendered, expected) {
			t.Fatalf("missing bounded message-kind metric %q: %s", expected, rendered)
		}
	}
	for _, forbidden := range []string{"reason=", "payload=", "tx_hash=", `layer="unknown"`} {
		if strings.Contains(rendered, forbidden) {
			t.Fatalf("message-kind metric contains unbounded label %q: %s", forbidden, rendered)
		}
	}
}

func TestReadinessFallsWhenDurableNATSConnectionIsUnavailable(t *testing.T) {
	var ready Readiness
	ready.MarkSourceInitialized()
	ready.MarkAdapterInitialized()
	ready.MarkSourceConnected()
	ready.MarkNATSReachable()
	ready.MarkSuccessfulPublish()
	if ok, _ := ready.Ready(); !ok {
		t.Fatal("expected acknowledged publication evidence to be ready")
	}
	ready.MarkNATSUnavailable()
	if ok, reason := ready.Ready(); ok || reason != "NATS not reachable" {
		t.Fatalf("durable NATS outage did not clear readiness ok=%v reason=%q", ok, reason)
	}
	ready.MarkNATSReachable()
	if ok, reason := ready.Ready(); !ok || reason != "ready" {
		t.Fatalf("readiness did not recover with NATS ok=%v reason=%q", ok, reason)
	}
}

func TestReadinessRequiresSourceAdapterConnectionSuccessfulPublishAndNATS(t *testing.T) {
	var ready Readiness
	if ok, reason := ready.Ready(); ok || reason != "source not initialized" {
		t.Fatalf("unexpected initial readiness ok=%v reason=%q", ok, reason)
	}
	ready.MarkSourceInitialized()
	if ok, reason := ready.Ready(); ok || reason != "feed adapter not initialized" {
		t.Fatalf("unexpected source-only readiness ok=%v reason=%q", ok, reason)
	}
	ready.MarkAdapterInitialized()
	if ok, reason := ready.Ready(); ok || reason != "feed source not connected" {
		t.Fatalf("unexpected adapter-only readiness ok=%v reason=%q", ok, reason)
	}
	ready.MarkSourceConnected()
	if ok, reason := ready.Ready(); ok || reason != "NATS not reachable" {
		t.Fatalf("unexpected source-connected readiness ok=%v reason=%q", ok, reason)
	}
	ready.MarkNATSReachable()
	if ok, reason := ready.Ready(); ok || reason != "no successful feed transaction published" {
		t.Fatalf("unexpected nats-only readiness ok=%v reason=%q", ok, reason)
	}
	ready.MarkSuccessfulPublish()
	if ok, reason := ready.Ready(); !ok || reason != "ready" {
		t.Fatalf("unexpected final readiness ok=%v reason=%q", ok, reason)
	}
	ready.MarkSequenceGap()
	if ok, reason := ready.Ready(); ok || reason != "unresolved feed sequence gap" {
		t.Fatalf("unexpected gap readiness ok=%v reason=%q", ok, reason)
	}
	ready.ClearSequenceGap()
	if ok, reason := ready.Ready(); !ok || reason != "ready" {
		t.Fatalf("unexpected resolved readiness ok=%v reason=%q", ok, reason)
	}
	ready.MarkFatal("decoder stopped")
	if ok, reason := ready.Ready(); ok || reason != "decoder stopped" {
		t.Fatalf("unexpected fatal readiness ok=%v reason=%q", ok, reason)
	}
}

func TestReadinessSequenceEvidenceCannotReplaceSuccessfulPublish(t *testing.T) {
	var ready Readiness
	ready.MarkSourceInitialized()
	ready.MarkAdapterInitialized()
	ready.MarkSourceConnected()
	ready.MarkNATSReachable()
	ready.MarkSequenceKnown()
	if ok, reason := ready.Ready(); ok || reason != "no successful feed transaction published" {
		t.Fatalf("sequence evidence must not claim readiness ok=%v reason=%q", ok, reason)
	}
}

func TestDisconnectRequiresFreshSequenceEvidenceBeforeReadinessRecovers(t *testing.T) {
	ready := Readiness{}
	ready.MarkSourceInitialized()
	ready.MarkAdapterInitialized()
	ready.MarkSourceConnected()
	ready.MarkNATSReachable()
	ready.MarkSuccessfulPublish()
	ready.MarkSourceDisconnected()
	if ok, reason := ready.Ready(); ok || reason != "feed source not connected" {
		t.Fatalf("disconnect did not clear readiness ok=%v reason=%q", ok, reason)
	}
	ready.MarkSourceConnected()
	if ok, reason := ready.Ready(); ok || reason != "feed sequence unknown" {
		t.Fatalf("reconnect claimed readiness before sequence evidence ok=%v reason=%q", ok, reason)
	}
	ready.MarkSequenceKnown()
	if ok, reason := ready.Ready(); !ok || reason != "ready" {
		t.Fatalf("fresh sequence evidence did not recover readiness ok=%v reason=%q", ok, reason)
	}
}

func TestIntegrityFailureIsTerminalForProcessLifetime(t *testing.T) {
	ready := Readiness{}
	ready.MarkSourceInitialized()
	ready.MarkAdapterInitialized()
	ready.MarkSourceConnected()
	ready.MarkNATSReachable()
	ready.MarkSuccessfulPublish()
	ready.MarkIntegrityFailure("Nitro feed sequence regression")
	ready.MarkSourceDisconnected()
	ready.MarkSourceConnected()
	ready.MarkSequenceKnown()
	if ok, reason := ready.Ready(); ok || reason != "Nitro feed sequence regression" {
		t.Fatalf("terminal integrity failure recovered unexpectedly ok=%v reason=%q", ok, reason)
	}
}
