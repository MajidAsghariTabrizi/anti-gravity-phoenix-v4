package metrics

import (
	"fmt"
	"net/http"
	"sort"
	"strings"
	"sync"
	"time"
)

type Registry struct {
	mu           sync.Mutex
	counters     map[string]uint64
	gauges       map[string]float64
	observations map[string]observation
}

type observation struct {
	count uint64
	sum   float64
}

var defaultCounters = []string{
	"feed_connections_total",
	"feed_messages_total",
	"feed_decode_failures_total",
	"feed_reconnects_total",
	"feed_normalized_transactions_total",
	"feed_sequence_gaps_total",
	"feed_sequence_gap_messages_total",
	"feed_sequence_regressions_total",
	"feed_sequence_duplicates_total",
	"feed_duplicates_total",
	"feed_out_of_order_total",
	"feed_publish_success_total",
	"feed_publish_failures_total",
	"feed_jetstream_publish_success_total",
	"feed_jetstream_publish_failures_total",
	"feed_jetstream_stream_unavailable_total",
	"feed_unsupported_messages_total",
}

var defaultGauges = []string{
	"feed_last_sequence",
	"feed_last_message_timestamp",
	"feed_readiness",
}

func NewRegistry() *Registry {
	r := &Registry{
		counters:     make(map[string]uint64),
		gauges:       make(map[string]float64),
		observations: make(map[string]observation),
	}
	for _, name := range defaultCounters {
		r.counters[name] = 0
	}
	for _, name := range defaultGauges {
		r.gauges[name] = 0
	}
	return r
}

func (r *Registry) Inc(name string) {
	r.Add(name, 1)
}

func (r *Registry) Add(name string, delta uint64) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.counters[name] += delta
}

func (r *Registry) SetGauge(name string, value float64) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.gauges[name] = value
}

func (r *Registry) ObserveIngestLatency(start time.Time) {
	r.ObserveDuration("feed_ingest_latency_seconds", time.Since(start))
}

func (r *Registry) ObserveJetStreamPublishLatency(start time.Time) {
	r.ObserveDuration("feed_jetstream_publish_latency", time.Since(start))
}

func (r *Registry) ObserveDuration(name string, duration time.Duration) {
	r.mu.Lock()
	defer r.mu.Unlock()
	value := r.observations[name]
	value.count++
	value.sum += duration.Seconds()
	r.observations[name] = value
}

func (r *Registry) Handler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/plain; version=0.0.4")
		fmt.Fprint(w, r.Render())
	})
}

func (r *Registry) Render() string {
	r.mu.Lock()
	defer r.mu.Unlock()
	var b strings.Builder
	keys := make([]string, 0, len(r.counters))
	for k := range r.counters {
		keys = append(keys, k)
	}
	sort.Strings(keys)
	for _, k := range keys {
		fmt.Fprintf(&b, "%s %d\n", k, r.counters[k])
	}
	gaugeKeys := make([]string, 0, len(r.gauges))
	for k := range r.gauges {
		gaugeKeys = append(gaugeKeys, k)
	}
	sort.Strings(gaugeKeys)
	for _, k := range gaugeKeys {
		fmt.Fprintf(&b, "%s %.0f\n", k, r.gauges[k])
	}
	observationKeys := make([]string, 0, len(r.observations))
	for name := range r.observations {
		observationKeys = append(observationKeys, name)
	}
	sort.Strings(observationKeys)
	for _, name := range observationKeys {
		value := r.observations[name]
		fmt.Fprintf(&b, "# TYPE %s summary\n", name)
		fmt.Fprintf(&b, "%s_count %d\n", name, value.count)
		fmt.Fprintf(&b, "%s_sum %.9f\n", name, value.sum)
	}
	return b.String()
}

type Readiness struct {
	mu                 sync.RWMutex
	sourceInitialized  bool
	adapterInitialized bool
	sourceConnected    bool
	successfulPublish  bool
	sequenceKnown      bool
	unresolvedGap      bool
	natsReachable      bool
	integrityFailure   string
	fatal              string
}

func (r *Readiness) MarkSourceInitialized() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.sourceInitialized = true
}

func (r *Readiness) MarkAdapterInitialized() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.adapterInitialized = true
}

func (r *Readiness) MarkSourceConnected() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.sourceConnected = true
}

func (r *Readiness) MarkSourceDisconnected() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.sourceConnected = false
	r.sequenceKnown = false
}

func (r *Readiness) MarkSuccessfulPublish() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.successfulPublish = true
	r.sequenceKnown = true
}

func (r *Readiness) MarkSequenceKnown() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.sequenceKnown = true
}

func (r *Readiness) MarkSequenceGap() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.unresolvedGap = true
}

func (r *Readiness) ClearSequenceGap() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.unresolvedGap = false
}

func (r *Readiness) MarkNATSReachable() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.natsReachable = true
}

func (r *Readiness) MarkNATSUnavailable() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.natsReachable = false
}

func (r *Readiness) MarkIntegrityFailure(reason string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	if r.integrityFailure == "" {
		r.integrityFailure = reason
	}
}

func (r *Readiness) MarkFatal(reason string) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.fatal = reason
}

func (r *Readiness) Ready() (bool, string) {
	r.mu.RLock()
	defer r.mu.RUnlock()
	if r.fatal != "" {
		return false, r.fatal
	}
	if r.integrityFailure != "" {
		return false, r.integrityFailure
	}
	if !r.sourceInitialized {
		return false, "source not initialized"
	}
	if !r.adapterInitialized {
		return false, "feed adapter not initialized"
	}
	if !r.sourceConnected {
		return false, "feed source not connected"
	}
	if !r.natsReachable {
		return false, "NATS not reachable"
	}
	if !r.successfulPublish {
		return false, "no successful feed transaction published"
	}
	if !r.sequenceKnown {
		return false, "feed sequence unknown"
	}
	if r.unresolvedGap {
		return false, "unresolved feed sequence gap"
	}
	return true, "ready"
}

func (r *Readiness) HealthHandler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		w.Header().Set("Content-Type", "text/plain")
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte("ok\n"))
	})
}

func (r *Readiness) ReadyHandler() http.Handler {
	return http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		ready, reason := r.Ready()
		w.Header().Set("Content-Type", "text/plain")
		if !ready {
			w.WriteHeader(http.StatusServiceUnavailable)
			_, _ = w.Write([]byte(reason + "\n"))
			return
		}
		w.WriteHeader(http.StatusOK)
		_, _ = w.Write([]byte(reason + "\n"))
	})
}
