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
	mu       sync.Mutex
	counters map[string]uint64
	latency  []float64
}

var defaultCounters = []string{
	"feed_messages_total",
	"feed_transactions_total",
	"feed_decode_errors_total",
	"feed_reconnects_total",
	"feed_sequence_gaps_total",
	"feed_duplicates_total",
}

func NewRegistry() *Registry {
	r := &Registry{counters: make(map[string]uint64)}
	for _, name := range defaultCounters {
		r.counters[name] = 0
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

func (r *Registry) ObserveIngestLatency(start time.Time) {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.latency = append(r.latency, time.Since(start).Seconds())
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
	for _, v := range r.latency {
		fmt.Fprintf(&b, "feed_ingest_latency_seconds %.9f\n", v)
	}
	return b.String()
}

type Readiness struct {
	mu                sync.RWMutex
	sourceInitialized bool
	natsReachable     bool
	fatal             string
}

func (r *Readiness) MarkSourceInitialized() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.sourceInitialized = true
}

func (r *Readiness) MarkNATSReachable() {
	r.mu.Lock()
	defer r.mu.Unlock()
	r.natsReachable = true
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
	if !r.sourceInitialized {
		return false, "source not initialized"
	}
	if !r.natsReachable {
		return false, "NATS not reachable"
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
