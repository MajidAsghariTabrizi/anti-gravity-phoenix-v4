package publisher

import "testing"

func TestMemoryPublisherStoresMessages(t *testing.T) {
	pub := &MemoryPublisher{}
	if err := pub.Publish("phoenix.feed.tx", map[string]string{"ok": "true"}); err != nil {
		t.Fatal(err)
	}
	if len(pub.Messages) != 1 {
		t.Fatalf("expected one message, got %d", len(pub.Messages))
	}
	if pub.Messages[0].Subject != "phoenix.feed.tx" {
		t.Fatalf("unexpected subject %s", pub.Messages[0].Subject)
	}
}
