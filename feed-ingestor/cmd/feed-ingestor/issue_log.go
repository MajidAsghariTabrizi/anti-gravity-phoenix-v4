package main

import (
	"log"
	"sync"
	"time"

	"anti-gravity-phoenix-v4/feed-ingestor/internal/metrics"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/nitro"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/sequence"
)

const (
	defaultIssueLogInterval = 30 * time.Second
	maxIssueLogStates       = 64
	overflowIssueLogKey     = "\x00overflow"
)

type issueLogState struct {
	lastLogged time.Time
	suppressed uint64
}

type sampledIssueLogger struct {
	mu       sync.Mutex
	logger   *log.Logger
	interval time.Duration
	now      func() time.Time
	states   map[string]issueLogState
}

func newSampledIssueLogger(logger *log.Logger, interval time.Duration, now func() time.Time) *sampledIssueLogger {
	if logger == nil {
		logger = log.Default()
	}
	if interval <= 0 {
		interval = defaultIssueLogInterval
	}
	if now == nil {
		now = time.Now
	}
	return &sampledIssueLogger{
		logger:   logger,
		interval: interval,
		now:      now,
		states: map[string]issueLogState{
			overflowIssueLogKey: {},
		},
	}
}

func (l *sampledIssueLogger) Log(class string, sequence uint64, reason string) {
	key := class + "\x00" + reason
	suppressed, emit := l.sample(key)
	if !emit {
		return
	}

	l.logger.Printf(
		"event=nitro_payload_issue class=%s sequence=%d reason=%q suppressed=%d",
		class,
		sequence,
		reason,
		suppressed,
	)
}

func (l *sampledIssueLogger) LogSequence(observation sequence.Result) {
	key := "sequence\x00" + string(observation.Event)
	suppressed, emit := l.sample(key)
	if !emit {
		return
	}
	l.logger.Printf(
		"event=feed_sequence_event class=%s sequence=%d gap_from=%d gap_to=%d missing=%d reconnected=%t suppressed=%d",
		observation.Event,
		observation.Sequence,
		observation.GapFrom,
		observation.GapTo,
		observation.Missing,
		observation.Reconnected,
		suppressed,
	)
}

func (l *sampledIssueLogger) sample(key string) (uint64, bool) {
	now := l.now()
	l.mu.Lock()
	defer l.mu.Unlock()
	if _, found := l.states[key]; !found && len(l.states) >= maxIssueLogStates {
		key = overflowIssueLogKey
	}
	state, found := l.states[key]
	if found && !state.lastLogged.IsZero() && now.Sub(state.lastLogged) < l.interval {
		state.suppressed++
		l.states[key] = state
		return 0, false
	}
	suppressed := state.suppressed
	l.states[key] = issueLogState{lastLogged: now}
	return suppressed, true
}

func recordFrameIssues(registry *metrics.Registry, issueLogger *sampledIssueLogger, frame nitro.Frame) {
	registry.Add("feed_unsupported_messages_total", uint64(len(frame.Unsupported)))
	registry.Add("feed_decode_failures_total", uint64(len(frame.Malformed)))
	registry.Add("feed_ignored_messages_total", uint64(len(frame.Ignored)))
	for _, kind := range frame.UnsupportedKinds {
		registry.IncUnsupportedMessageKind(kind)
	}
	for _, kind := range frame.IgnoredKinds {
		registry.IncIgnoredMessageKind(kind)
	}
	for _, reason := range frame.Unsupported {
		issueLogger.Log("unsupported", frame.Sequence, reason)
	}
	for _, reason := range frame.Malformed {
		issueLogger.Log("malformed", frame.Sequence, reason)
	}
	for _, reason := range frame.Ignored {
		issueLogger.Log("ignored", frame.Sequence, reason)
	}
}
