package nitro

import "testing"

func TestDecodeBroadcastRejectsMalformedJSON(t *testing.T) {
	if _, _, err := DecodeBroadcast([]byte(`{"version":1`)); err == nil {
		t.Fatal("expected malformed JSON to fail")
	}
}
