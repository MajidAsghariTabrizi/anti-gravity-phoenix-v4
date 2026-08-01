package observer

import (
	"net/http"
	"net/http/httptest"
	"strings"
	"testing"
	"time"
)

func TestHealthAndReadinessAreFailClosedAndSanitized(t *testing.T) {
	start := time.Unix(1_700_000_000, 0).UTC()
	ledger, err := OpenLedger(t.TempDir(), start, 0, 0)
	if err != nil {
		t.Fatal(err)
	}
	handler := NewHealthHandler(ledger)

	health := httptest.NewRecorder()
	handler.ServeHTTP(health, httptest.NewRequest(http.MethodGet, "/healthz", nil))
	if health.Code != http.StatusOK {
		t.Fatalf("new ledger was not healthy: %d %s", health.Code, health.Body.String())
	}
	ready := httptest.NewRecorder()
	handler.ServeHTTP(ready, httptest.NewRequest(http.MethodGet, "/readyz", nil))
	if ready.Code != http.StatusServiceUnavailable {
		t.Fatalf("disconnected ledger was ready: %d", ready.Code)
	}

	if err := ledger.RecordSubscription(start.Add(time.Second), "private-subscription-identity"); err != nil {
		t.Fatal(err)
	}
	ready = httptest.NewRecorder()
	handler.ServeHTTP(ready, httptest.NewRequest(http.MethodGet, "/readyz", nil))
	if ready.Code != http.StatusOK {
		t.Fatalf("connected ledger was not ready: %d %s", ready.Code, ready.Body.String())
	}
	if strings.Contains(ready.Body.String(), "private-subscription-identity") {
		t.Fatal("readiness exposed the subscription identity")
	}

	if err := ledger.AppendInvalid(InvalidRecord{
		ObservedAt:         start.Add(2 * time.Second),
		Reason:             "test invariant",
		NotificationSHA256: strings.Repeat("a", 64),
	}); err != nil {
		t.Fatal(err)
	}
	health = httptest.NewRecorder()
	handler.ServeHTTP(health, httptest.NewRequest(http.MethodGet, "/healthz", nil))
	if health.Code != http.StatusServiceUnavailable {
		t.Fatalf("invalid ledger remained healthy: %d", health.Code)
	}
	if strings.Contains(health.Body.String(), "test invariant") {
		t.Fatal("health exposed an invalid raw reason")
	}
}

func TestMetricsContainOnlyAggregateAuctionEvidence(t *testing.T) {
	start := time.Unix(1_700_000_000, 0).UTC()
	ledger, err := OpenLedger(t.TempDir(), start, 0, 0)
	if err != nil {
		t.Fatal(err)
	}
	record, err := DecodeAndValidateNotification(
		validNotification(t, "0x4c76F02E484e8ce9B6C2358CF9624BabC5531E9e"),
		start.Add(time.Second),
	)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := ledger.Append(record); err != nil {
		t.Fatal(err)
	}

	response := httptest.NewRecorder()
	NewHealthHandler(ledger).ServeHTTP(response, httptest.NewRequest(http.MethodGet, "/metrics", nil))
	body := response.Body.String()
	if response.Code != http.StatusOK || !strings.Contains(body, `phoenix_atlas_feed_auctions_total{asset="LINK"} 1`) {
		t.Fatalf("missing aggregate feed metric: %d %s", response.Code, body)
	}
	if strings.Contains(body, validAuctionID) || strings.Contains(body, record.UserOpHash) {
		t.Fatal("metrics exposed a canonical auction identity")
	}
}
