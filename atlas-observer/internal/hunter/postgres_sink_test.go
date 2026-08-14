package hunter

import (
	"testing"
	"time"
)

func TestAdvancePrimaryProviderSamplesIgnoresOutOfOrderCompletion(t *testing.T) {
	base := time.Date(2026, time.August, 14, 11, 41, 0, 0, time.UTC)
	samples := []providerRecoverySample{
		{at: base.Add(time.Second), primary: primaryProviderID},
		{at: base.Add(2 * time.Second), primary: primaryProviderID},
		{at: base.Add(3 * time.Second), primary: primaryProviderID},
	}

	unchanged, advanced := advancePrimaryProviderSamples(
		samples,
		"ready",
		base.Add(2500*time.Millisecond),
		primaryProviderID,
	)
	if advanced {
		t.Fatal("out-of-order completion advanced provider recovery evidence")
	}
	if len(unchanged) != 3 || !unchanged[2].at.Equal(base.Add(3*time.Second)) {
		t.Fatalf("out-of-order completion changed durable samples: %+v", unchanged)
	}

	advancedSamples, advanced := advancePrimaryProviderSamples(
		unchanged,
		"ready",
		base.Add(4*time.Second),
		primaryProviderID,
	)
	if !advanced {
		t.Fatal("newer completion did not advance provider recovery evidence")
	}
	if len(advancedSamples) != 3 ||
		!advancedSamples[0].at.Equal(base.Add(2*time.Second)) ||
		!advancedSamples[1].at.Equal(base.Add(3*time.Second)) ||
		!advancedSamples[2].at.Equal(base.Add(4*time.Second)) {
		t.Fatalf("newer completion did not preserve a strict rolling window: %+v", advancedSamples)
	}
}

func TestAdvancePrimaryProviderSamplesDoesNotRegressCollectingWindow(t *testing.T) {
	base := time.Date(2026, time.August, 14, 11, 41, 0, 0, time.UTC)
	samples := []providerRecoverySample{{at: base.Add(2 * time.Second), primary: primaryProviderID}}

	unchanged, advanced := advancePrimaryProviderSamples(
		samples,
		"collecting",
		base.Add(time.Second),
		primaryProviderID,
	)
	if advanced || len(unchanged) != 1 || !unchanged[0].at.Equal(base.Add(2*time.Second)) {
		t.Fatalf("collecting window regressed to an older completion: advanced=%v samples=%+v", advanced, unchanged)
	}
}
