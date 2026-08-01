package observer

import (
	"encoding/json"
	"fmt"
	"net/http"
	"sort"
	"strings"
	"time"
)

type HealthHandler struct {
	ledger *Ledger
	now    func() time.Time
}

func NewHealthHandler(ledger *Ledger) http.Handler {
	handler := &HealthHandler{ledger: ledger, now: time.Now}
	mux := http.NewServeMux()
	mux.HandleFunc("/healthz", handler.health)
	mux.HandleFunc("/readyz", handler.ready)
	mux.HandleFunc("/metrics", handler.metrics)
	return mux
}

func (h *HealthHandler) health(writer http.ResponseWriter, _ *http.Request) {
	state := h.ledger.Snapshot(h.now().UTC())
	status := http.StatusOK
	if state.InvalidCount > 0 || state.CompletionReason == "safety_identity_limit" {
		status = http.StatusServiceUnavailable
	}
	writeHealthJSON(writer, status, state, "health")
}

func (h *HealthHandler) ready(writer http.ResponseWriter, _ *http.Request) {
	state := h.ledger.Snapshot(h.now().UTC())
	status := http.StatusOK
	if !state.Connected || state.LastSubscriptionAt == nil || state.InvalidCount > 0 || state.Completed {
		status = http.StatusServiceUnavailable
	}
	writeHealthJSON(writer, status, state, "readiness")
}

func writeHealthJSON(writer http.ResponseWriter, status int, state LedgerState, check string) {
	writer.Header().Set("Content-Type", "application/json")
	writer.WriteHeader(status)
	_ = json.NewEncoder(writer).Encode(struct {
		Check              string     `json:"check"`
		OK                 bool       `json:"ok"`
		Connected          bool       `json:"connected"`
		Continuous         bool       `json:"continuous"`
		Completed          bool       `json:"completed"`
		CompletionReason   string     `json:"completion_reason,omitempty"`
		UniqueAuctions     uint64     `json:"unique_auctions"`
		RelevantAave       uint64     `json:"relevant_aave"`
		Invalid            uint64     `json:"invalid"`
		Reconnects         uint64     `json:"reconnects"`
		LastMessageAt      *time.Time `json:"last_message_at"`
		LastSubscriptionAt *time.Time `json:"last_subscription_at"`
	}{
		Check:              check,
		OK:                 status == http.StatusOK,
		Connected:          state.Connected,
		Continuous:         state.Continuous,
		Completed:          state.Completed,
		CompletionReason:   state.CompletionReason,
		UniqueAuctions:     state.UniqueAuctionCount,
		RelevantAave:       state.RelevantAaveCount,
		Invalid:            state.InvalidCount,
		Reconnects:         state.ReconnectCount,
		LastMessageAt:      state.LastMessageAt,
		LastSubscriptionAt: state.LastSubscriptionAt,
	})
}

func (h *HealthHandler) metrics(writer http.ResponseWriter, _ *http.Request) {
	state := h.ledger.Snapshot(h.now().UTC())
	writer.Header().Set("Content-Type", "text/plain; version=0.0.4")
	connected := 0
	if state.Connected {
		connected = 1
	}
	completed := 0
	if state.Completed {
		completed = 1
	}
	lines := []string{
		"# HELP phoenix_atlas_connected Whether the reviewed Atlas subscription is connected.",
		"# TYPE phoenix_atlas_connected gauge",
		fmt.Sprintf("phoenix_atlas_connected %d", connected),
		"# HELP phoenix_atlas_completed Whether the observation contract has completed.",
		"# TYPE phoenix_atlas_completed gauge",
		fmt.Sprintf("phoenix_atlas_completed %d", completed),
		"# HELP phoenix_atlas_auctions_total Durable unique Arbitrum Atlas auctions.",
		"# TYPE phoenix_atlas_auctions_total counter",
		fmt.Sprintf("phoenix_atlas_auctions_total %d", state.UniqueAuctionCount),
		"# HELP phoenix_atlas_aave_auctions_total Durable Aave-SVR Atlas auctions.",
		"# TYPE phoenix_atlas_aave_auctions_total counter",
		fmt.Sprintf("phoenix_atlas_aave_auctions_total %d", state.RelevantAaveCount),
		"# HELP phoenix_atlas_invalid_total Rejected invariant-breaking notifications.",
		"# TYPE phoenix_atlas_invalid_total counter",
		fmt.Sprintf("phoenix_atlas_invalid_total %d", state.InvalidCount),
		"# HELP phoenix_atlas_reconnects_total Atlas gateway reconnect attempts.",
		"# TYPE phoenix_atlas_reconnects_total counter",
		fmt.Sprintf("phoenix_atlas_reconnects_total %d", state.ReconnectCount),
	}
	feeds := make([]string, 0, len(state.PerFeed))
	for feed := range state.PerFeed {
		feeds = append(feeds, feed)
	}
	sort.Strings(feeds)
	for _, feed := range feeds {
		lines = append(lines, fmt.Sprintf("phoenix_atlas_feed_auctions_total{asset=%q} %d", feed, state.PerFeed[feed]))
	}
	if state.LastMessageAt != nil {
		lines = append(lines, fmt.Sprintf("phoenix_atlas_last_message_timestamp_seconds %d", state.LastMessageAt.Unix()))
	}
	if state.LastSubscriptionAt != nil {
		lines = append(lines, fmt.Sprintf("phoenix_atlas_last_subscription_timestamp_seconds %d", state.LastSubscriptionAt.Unix()))
	}
	_, _ = writer.Write([]byte(strings.Join(lines, "\n") + "\n"))
}
