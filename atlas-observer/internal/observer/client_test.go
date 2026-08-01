package observer

import (
	"encoding/json"
	"testing"
)

func TestSubscriptionPayloadIsReadOnlyAndCanonical(t *testing.T) {
	payload := SubscriptionPayload()
	if payload.JSONRPC != "2.0" || payload.ID != 1 || payload.Method != "solver_subscribe" {
		t.Fatalf("unexpected subscription request: %#v", payload)
	}
	if len(payload.Params) != 1 || payload.Params[0] != "userOperations" {
		t.Fatalf("unexpected subscription topic: %#v", payload.Params)
	}
	encoded, err := json.Marshal(payload)
	if err != nil {
		t.Fatal(err)
	}
	var decoded map[string]any
	if err := json.Unmarshal(encoded, &decoded); err != nil {
		t.Fatal(err)
	}
	if len(decoded) != 4 {
		t.Fatalf("subscription request contains unexpected fields: %s", encoded)
	}
}
