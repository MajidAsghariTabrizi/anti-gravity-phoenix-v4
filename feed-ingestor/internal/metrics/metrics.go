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

func NewRegistry() *Registry {
	return &Registry{counters: make(map[string]uint64)}
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
