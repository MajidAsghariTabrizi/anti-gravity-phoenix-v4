package publisher

import (
	"context"
	"errors"
	"os"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/nats-io/nats.go/jetstream"
)

type fakeJetStream struct {
	configs    []jetstream.StreamConfig
	ack        *jetstream.PubAck
	publishErr error
	wait       bool
}

func (f *fakeJetStream) EnsureStream(_ context.Context, config jetstream.StreamConfig) error {
	f.configs = append(f.configs, config)
	return nil
}

func (f *fakeJetStream) Publish(ctx context.Context, _ string, _ []byte, _ ...jetstream.PublishOpt) (*jetstream.PubAck, error) {
	if f.wait {
		<-ctx.Done()
		return nil, ctx.Err()
	}
	return f.ack, f.publishErr
}

type noopCloser struct{}

func (noopCloser) Close() {}

type identifiedValue struct{}

func (identifiedValue) DurableMessageID() string { return "1:0xabc" }

func TestMemoryPublisherStoresMessages(t *testing.T) {
	pub := &MemoryPublisher{}
	if err := pub.Publish(context.Background(), "phoenix.feed.tx", map[string]string{"ok": "true"}); err != nil {
		t.Fatal(err)
	}
	if len(pub.Messages) != 1 {
		t.Fatalf("expected one message, got %d", len(pub.Messages))
	}
	if pub.Messages[0].Subject != "phoenix.feed.tx" {
		t.Fatalf("unexpected subject %s", pub.Messages[0].Subject)
	}
}

func TestStreamCreationIsIdempotentAndBounded(t *testing.T) {
	api := &fakeJetStream{}
	for range 2 {
		if _, err := newJetStreamPublisher(context.Background(), api, noopCloser{}, time.Second); err != nil {
			t.Fatal(err)
		}
	}
	if len(api.configs) != 2 || !reflect.DeepEqual(api.configs[0], api.configs[1]) {
		t.Fatalf("stream provisioning was not idempotent: %+v", api.configs)
	}
	config := api.configs[0]
	if config.Name != StreamName || !reflect.DeepEqual(config.Subjects, []string{StreamSubject}) {
		t.Fatalf("unexpected stream identity: %+v", config)
	}
	if config.Retention != jetstream.WorkQueuePolicy || config.Storage != jetstream.FileStorage || config.Discard != jetstream.DiscardNew {
		t.Fatalf("stream is not durable and fail-closed: %+v", config)
	}
	if config.MaxMsgs != StreamMaxMessages || config.MaxBytes != StreamMaxBytes || config.MaxAge != StreamMaxAge || config.MaxMsgSize != StreamMaxMessageBytes {
		t.Fatalf("stream limits changed: %+v", config)
	}
}

func TestPublishSucceedsOnlyAfterValidJetStreamAcknowledgement(t *testing.T) {
	api := &fakeJetStream{ack: &jetstream.PubAck{Stream: StreamName, Sequence: 7}}
	pub, err := newJetStreamPublisher(context.Background(), api, noopCloser{}, time.Second)
	if err != nil {
		t.Fatal(err)
	}
	if err := pub.Publish(context.Background(), StreamSubject, identifiedValue{}); err != nil {
		t.Fatalf("acknowledged publish failed: %v", err)
	}

	api.ack = &jetstream.PubAck{Stream: "OTHER", Sequence: 8}
	if err := pub.Publish(context.Background(), StreamSubject, identifiedValue{}); !errors.Is(err, ErrInvalidPublishAck) {
		t.Fatalf("invalid acknowledgement was accepted: %v", err)
	}
}

func TestPublishAcknowledgementTimeoutIsBounded(t *testing.T) {
	api := &fakeJetStream{wait: true}
	pub, err := newJetStreamPublisher(context.Background(), api, noopCloser{}, 5*time.Millisecond)
	if err != nil {
		t.Fatal(err)
	}
	if err := pub.Publish(context.Background(), StreamSubject, identifiedValue{}); !errors.Is(err, ErrPublishAckTimeout) {
		t.Fatalf("expected acknowledgement timeout, got %v", err)
	}
}

func TestPublisherErrorsDoNotEchoPayloadsOrCredentials(t *testing.T) {
	api := &fakeJetStream{publishErr: errors.New("password=secret raw_tx=signed")}
	pub, err := newJetStreamPublisher(context.Background(), api, noopCloser{}, time.Second)
	if err != nil {
		t.Fatal(err)
	}
	err = pub.Publish(context.Background(), StreamSubject, map[string]string{"raw_tx": "signed"})
	if !errors.Is(err, ErrPublishAckUnavailable) || err.Error() != ErrPublishAckUnavailable.Error() {
		t.Fatalf("publisher leaked dependency details: %v", err)
	}
}

func TestJetStreamPublisherIntegration(t *testing.T) {
	url := strings.TrimSpace(os.Getenv("PHOENIX_TEST_NATS_URL"))
	if url == "" {
		t.Skip("PHOENIX_TEST_NATS_URL is not set")
	}
	if !strings.HasPrefix(url, "nats://127.0.0.1:") && !strings.HasPrefix(url, "nats://localhost:") {
		t.Fatal("integration test NATS URL must be loopback-only")
	}
	pub, err := DialJetStream(url, 2*time.Second, ConnectionEvents{})
	if err != nil {
		t.Fatalf("connect local JetStream: %v", err)
	}
	defer pub.Close()
	for range 2 {
		if err := pub.Publish(context.Background(), StreamSubject, identifiedValue{}); err != nil {
			t.Fatalf("publish with persistence acknowledgement: %v", err)
		}
	}
}
