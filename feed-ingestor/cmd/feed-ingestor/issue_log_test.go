package main

import (
	"bytes"
	"fmt"
	"log"
	"strings"
	"testing"
	"time"

	"anti-gravity-phoenix-v4/feed-ingestor/internal/metrics"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/nitro"
)

func TestIssueLogRateLimitingDoesNotAffectCounters(t *testing.T) {
	registry := metrics.NewRegistry()
	var output bytes.Buffer
	now := time.Unix(1700000000, 0)
	issueLogger := newSampledIssueLogger(log.New(&output, "", 0), 30*time.Second, func() time.Time {
		return now
	})
	frame := nitro.Frame{
		Sequence:    460530858,
		Unsupported: []string{"batch item 1: unknown L2 message kind 0x7f"},
	}

	for range 10 {
		recordFrameIssues(registry, issueLogger, frame)
	}
	if !strings.Contains(registry.Render(), "feed_unsupported_messages_total 10") {
		t.Fatalf("sampling changed the counter: %s", registry.Render())
	}
	if got := strings.Count(output.String(), "event=nitro_payload_issue"); got != 1 {
		t.Fatalf("expected one sampled log, got %d:\n%s", got, output.String())
	}

	now = now.Add(31 * time.Second)
	recordFrameIssues(registry, issueLogger, frame)
	if !strings.Contains(registry.Render(), "feed_unsupported_messages_total 11") {
		t.Fatalf("post-window counter mismatch: %s", registry.Render())
	}
	if got := strings.Count(output.String(), "event=nitro_payload_issue"); got != 2 {
		t.Fatalf("expected a second sampled log, got %d:\n%s", got, output.String())
	}
	if !strings.Contains(output.String(), "suppressed=9") || !strings.Contains(output.String(), "sequence=460530858") {
		t.Fatalf("sampled diagnostics lack suppression/sequence evidence:\n%s", output.String())
	}
}

func TestRecordFrameIssuesSeparatesMalformedAndUnsupportedCounters(t *testing.T) {
	registry := metrics.NewRegistry()
	issueLogger := newSampledIssueLogger(log.New(&bytes.Buffer{}, "", 0), time.Minute, nil)
	recordFrameIssues(registry, issueLogger, nitro.Frame{
		Sequence:    9,
		Unsupported: []string{"unsupported kind"},
		Malformed:   []string{"truncated batch", "invalid signature"},
	})

	rendered := registry.Render()
	if !strings.Contains(rendered, "feed_unsupported_messages_total 1") || !strings.Contains(rendered, "feed_decode_failures_total 2") {
		t.Fatalf("issue counters were conflated: %s", rendered)
	}
}

func TestIssueLogStateCardinalityIsBounded(t *testing.T) {
	var output bytes.Buffer
	issueLogger := newSampledIssueLogger(log.New(&output, "", 0), time.Minute, nil)
	for index := range maxIssueLogStates * 2 {
		issueLogger.Log("unsupported", uint64(index), fmt.Sprintf("unknown nested kind %d", index))
	}

	if got := len(issueLogger.states); got > maxIssueLogStates {
		t.Fatalf("sampled issue state grew past its bound: %d", got)
	}
	if got := strings.Count(output.String(), "event=nitro_payload_issue"); got > maxIssueLogStates {
		t.Fatalf("high-cardinality issues emitted too many logs in one window: %d", got)
	}
}
