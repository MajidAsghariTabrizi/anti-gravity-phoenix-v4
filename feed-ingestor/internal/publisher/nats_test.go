package publisher

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"os"
	"reflect"
	"strings"
	"testing"
	"time"

	"github.com/nats-io/nats.go"
	"github.com/nats-io/nats.go/jetstream"
)

type fakePublishResult struct {
	ack  *jetstream.PubAck
	err  error
	wait bool
}

type publishCall struct {
	subject   string
	payload   []byte
	messageID string
}

type fakeJetStream struct {
	configs []jetstream.StreamConfig
	results []fakePublishResult
	calls   []publishCall
}

func (f *fakeJetStream) EnsureStream(_ context.Context, config jetstream.StreamConfig) error {
	f.configs = append(f.configs, config)
	return nil
}

func (f *fakeJetStream) Publish(ctx context.Context, subject string, payload []byte, messageID string) (*jetstream.PubAck, error) {
	f.calls = append(f.calls, publishCall{
		subject:   subject,
		payload:   append([]byte(nil), payload...),
		messageID: messageID,
	})
	index := len(f.calls) - 1
	if index >= len(f.results) {
		return nil, errors.New("unexpected publish call")
	}
	result := f.results[index]
	if result.wait {
		<-ctx.Done()
		return nil, ctx.Err()
	}
	return result.ack, result.err
}

type noopCloser struct{}

func (noopCloser) Close() {}

type identifiedValue struct {
	Sequence uint64 `json:"sequence"`
	EventID  string `json:"event_id"`
}

func (value identifiedValue) DurableMessageID() string {
	return fmt.Sprintf("%d:%s", value.Sequence, value.EventID)
}

func testValue(sequence uint64, eventID string) identifiedValue {
	return identifiedValue{Sequence: sequence, EventID: eventID}
}

func acknowledged(sequence uint64, duplicate bool) fakePublishResult {
	return fakePublishResult{ack: &jetstream.PubAck{
		Stream:    StreamName,
		Sequence:  sequence,
		Duplicate: duplicate,
	}}
}

func newTestPublisher(t *testing.T, api *fakeJetStream) *JetStreamPublisher {
	t.Helper()
	pub, err := newJetStreamPublisher(context.Background(), api, noopCloser{}, time.Second)
	if err != nil {
		t.Fatal(err)
	}
	pub.retryBackoff = 0
	pub.retryBudget = time.Second
	return pub
}

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
	api := &fakeJetStream{results: []fakePublishResult{
		acknowledged(7, false),
		{ack: &jetstream.PubAck{Stream: "OTHER", Sequence: 8}},
	}}
	pub := newTestPublisher(t, api)
	value := testValue(1, "0xabc")
	if err := pub.Publish(context.Background(), StreamSubject, value); err != nil {
		t.Fatalf("acknowledged publish failed: %v", err)
	}

	if err := pub.Publish(context.Background(), StreamSubject, value); !errors.Is(err, ErrInvalidPublishAck) {
		t.Fatalf("invalid acknowledgement was accepted: %v", err)
	}
}

func TestPublishRequiresCanonicalIdentity(t *testing.T) {
	api := &fakeJetStream{}
	pub := newTestPublisher(t, api)
	for _, value := range []any{
		map[string]string{"event": "missing identity"},
		identifiedValue{Sequence: 1},
		identifiedValue{Sequence: 1, EventID: " event"},
		identifiedValue{Sequence: 1, EventID: strings.Repeat("a", PublishMaxMessageIDBytes)},
	} {
		if err := pub.Publish(context.Background(), StreamSubject, value); !errors.Is(err, ErrPublishIdentity) {
			t.Fatalf("identity-less publication was accepted: %v", err)
		}
	}
	if len(api.calls) != 0 {
		t.Fatalf("identity-less values reached JetStream: %+v", api.calls)
	}
}

func TestAmbiguousTimeoutRetriesByteIdenticalPublicationAndRecoversNormally(t *testing.T) {
	api := &fakeJetStream{results: []fakePublishResult{
		{err: context.DeadlineExceeded},
		acknowledged(9, false),
	}}
	pub := newTestPublisher(t, api)
	var events []PublishEvent
	pub.publishState = func(event PublishEvent) { events = append(events, event) }

	if err := pub.Publish(context.Background(), StreamSubject, testValue(41, "0xabc")); err != nil {
		t.Fatalf("normal acknowledgement did not recover timeout: %v", err)
	}
	assertSamePublication(t, api.calls)
	if len(events) != 2 || events[0].Kind != PublishEventAckTimeoutRetry || events[1].Kind != PublishEventRecoveredNormal {
		t.Fatalf("unexpected normal recovery events: %+v", events)
	}
	if events[0].Attempt != 1 || events[1].Attempt != 2 {
		t.Fatalf("unexpected recovery attempt numbers: %+v", events)
	}
}

func TestAmbiguousTimeoutRecoversThroughDuplicateAcknowledgement(t *testing.T) {
	api := &fakeJetStream{results: []fakePublishResult{
		{err: context.DeadlineExceeded},
		acknowledged(9, true),
	}}
	pub := newTestPublisher(t, api)
	var events []PublishEvent
	pub.publishState = func(event PublishEvent) { events = append(events, event) }

	if err := pub.Publish(context.Background(), StreamSubject, testValue(42, "0xdef")); err != nil {
		t.Fatalf("duplicate acknowledgement did not recover timeout: %v", err)
	}
	assertSamePublication(t, api.calls)
	if len(events) != 2 || events[1].Kind != PublishEventRecoveredDuplicate {
		t.Fatalf("unexpected duplicate recovery events: %+v", events)
	}
}

func TestPublishAcknowledgementTimeoutExhaustionIsBoundedAndFailClosed(t *testing.T) {
	api := &fakeJetStream{results: []fakePublishResult{
		{wait: true},
		{wait: true},
		{wait: true},
	}}
	pub, err := newJetStreamPublisher(context.Background(), api, noopCloser{}, time.Millisecond)
	if err != nil {
		t.Fatal(err)
	}
	pub.retryBackoff = 0
	pub.retryBudget = 20 * time.Millisecond
	var events []PublishEvent
	pub.publishState = func(event PublishEvent) { events = append(events, event) }

	started := time.Now()
	err = pub.Publish(context.Background(), StreamSubject, testValue(43, "0xaaa"))
	if !errors.Is(err, ErrPublishAckTimeout) {
		t.Fatalf("expected acknowledgement timeout, got %v", err)
	}
	if elapsed := time.Since(started); elapsed >= 250*time.Millisecond {
		t.Fatalf("bounded retry took unexpectedly long: %s", elapsed)
	}
	if len(api.calls) != PublishMaxAttempts {
		t.Fatalf("unexpected attempt count: %d", len(api.calls))
	}
	assertSamePublication(t, api.calls)
	if len(events) != PublishMaxAttempts || events[len(events)-1].Kind != PublishEventRetryExhausted {
		t.Fatalf("unexpected exhausted events: %+v", events)
	}
}

func TestRetriesFinishBeforeNextSourcePublication(t *testing.T) {
	api := &fakeJetStream{results: []fakePublishResult{
		{err: context.DeadlineExceeded},
		acknowledged(10, true),
		acknowledged(11, false),
	}}
	pub := newTestPublisher(t, api)
	first := testValue(50, "0xfirst")
	second := testValue(51, "0xsecond")
	if err := pub.Publish(context.Background(), StreamSubject, first); err != nil {
		t.Fatal(err)
	}
	if err := pub.Publish(context.Background(), StreamSubject, second); err != nil {
		t.Fatal(err)
	}
	got := []string{api.calls[0].messageID, api.calls[1].messageID, api.calls[2].messageID}
	want := []string{first.DurableMessageID(), first.DurableMessageID(), second.DurableMessageID()}
	if !reflect.DeepEqual(got, want) {
		t.Fatalf("retry reordered source publications: got=%v want=%v", got, want)
	}
}

func TestCanonicalIdentityChangesWithSourceEvent(t *testing.T) {
	api := &fakeJetStream{results: []fakePublishResult{
		acknowledged(12, false),
		acknowledged(13, false),
		acknowledged(14, false),
	}}
	pub := newTestPublisher(t, api)
	values := []identifiedValue{
		testValue(60, "0xaaa"),
		testValue(61, "0xaaa"),
		testValue(60, "0xbbb"),
	}
	for _, value := range values {
		if err := pub.Publish(context.Background(), StreamSubject, value); err != nil {
			t.Fatal(err)
		}
	}
	if api.calls[0].messageID == api.calls[1].messageID || api.calls[0].messageID == api.calls[2].messageID {
		t.Fatalf("changed source identity reused a message ID: %+v", api.calls)
	}
	if bytes.Equal(api.calls[0].payload, api.calls[1].payload) || bytes.Equal(api.calls[0].payload, api.calls[2].payload) {
		t.Fatalf("source changes did not produce distinct canonical payloads: %+v", api.calls)
	}
}

func TestRetryPolicyRemainsInsideDuplicateWindow(t *testing.T) {
	maximumAttemptEnvelope := time.Duration(PublishMaxAttempts)*2*time.Second +
		time.Duration(PublishMaxAttempts-1)*PublishRetryBackoff
	if PublishMaxAttempts < 2 || maximumAttemptEnvelope > PublishRetryBudget {
		t.Fatalf("retry policy does not fit its hard budget: attempts=%d envelope=%s budget=%s", PublishMaxAttempts, maximumAttemptEnvelope, PublishRetryBudget)
	}
	if PublishRetryBudget >= StreamDuplicateWindow {
		t.Fatalf("retry budget %s is not below duplicate window %s", PublishRetryBudget, StreamDuplicateWindow)
	}
}

func TestPublisherErrorsDoNotEchoPayloadsOrCredentials(t *testing.T) {
	api := &fakeJetStream{results: []fakePublishResult{{err: errors.New("password=secret raw_tx=signed")}}}
	pub := newTestPublisher(t, api)
	err := pub.Publish(context.Background(), StreamSubject, testValue(70, "0xcredential-free"))
	if !errors.Is(err, ErrPublishAckUnavailable) || err.Error() != ErrPublishAckUnavailable.Error() {
		t.Fatalf("publisher leaked dependency details: %v", err)
	}
}

func assertSamePublication(t *testing.T, calls []publishCall) {
	t.Helper()
	if len(calls) < 2 {
		t.Fatalf("expected a retry, got %d calls", len(calls))
	}
	for _, call := range calls[1:] {
		if call.subject != calls[0].subject || call.messageID != calls[0].messageID || !bytes.Equal(call.payload, calls[0].payload) {
			t.Fatalf("retry changed publication identity or bytes: %+v", calls)
		}
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
	adminConnection, err := nats.Connect(url)
	if err != nil {
		t.Fatalf("connect local JetStream administrator: %v", err)
	}
	defer adminConnection.Close()
	admin, err := jetstream.New(adminConnection)
	if err != nil {
		t.Fatalf("create local JetStream administrator: %v", err)
	}
	_ = admin.DeleteStream(context.Background(), StreamName)

	pub, err := DialJetStream(url, 2*time.Second, ConnectionEvents{})
	if err != nil {
		t.Fatalf("connect local JetStream: %v", err)
	}
	defer pub.Close()
	defer admin.DeleteStream(context.Background(), StreamName)
	pub.retryBackoff = time.Millisecond
	base := pub.api

	acceptedAckLost := &ambiguousAckAPI{delegate: base, mode: ackLostAfterAccepted}
	pub.api = acceptedAckLost
	if err := pub.Publish(context.Background(), StreamSubject, testValue(80, "0xaccepted")); err != nil {
		t.Fatalf("accepted publication did not recover through duplicate ACK: %v", err)
	}
	assertSamePublication(t, acceptedAckLost.calls)
	if len(acceptedAckLost.returnedAcks) != 1 || !acceptedAckLost.returnedAcks[0].Duplicate {
		t.Fatalf("accepted publication was not proven by duplicate ACK: %+v", acceptedAckLost.returnedAcks)
	}
	assertStreamMessages(t, admin, 1)

	notAccepted := &ambiguousAckAPI{delegate: base, mode: firstAttemptNotAccepted}
	pub.api = notAccepted
	if err := pub.Publish(context.Background(), StreamSubject, testValue(81, "0xnot-accepted")); err != nil {
		t.Fatalf("unaccepted publication did not recover through normal ACK: %v", err)
	}
	assertSamePublication(t, notAccepted.calls)
	if len(notAccepted.returnedAcks) != 1 || notAccepted.returnedAcks[0].Duplicate {
		t.Fatalf("unaccepted publication was not proven by normal ACK: %+v", notAccepted.returnedAcks)
	}
	assertStreamMessages(t, admin, 2)

	allAcksLost := &ambiguousAckAPI{delegate: base, mode: everyAckLostAfterAccepted}
	pub.api = allAcksLost
	if err := pub.Publish(context.Background(), StreamSubject, testValue(82, "0xexhausted")); !errors.Is(err, ErrPublishAckTimeout) {
		t.Fatalf("lost acknowledgements did not fail closed: %v", err)
	}
	assertSamePublication(t, allAcksLost.calls)
	assertStreamMessages(t, admin, 3)
}

type ambiguousAckMode int

const (
	ackLostAfterAccepted ambiguousAckMode = iota
	firstAttemptNotAccepted
	everyAckLostAfterAccepted
)

type ambiguousAckAPI struct {
	delegate     jetStreamAPI
	mode         ambiguousAckMode
	calls        []publishCall
	returnedAcks []*jetstream.PubAck
}

func (api *ambiguousAckAPI) EnsureStream(ctx context.Context, config jetstream.StreamConfig) error {
	return api.delegate.EnsureStream(ctx, config)
}

func (api *ambiguousAckAPI) Publish(ctx context.Context, subject string, payload []byte, messageID string) (*jetstream.PubAck, error) {
	api.calls = append(api.calls, publishCall{
		subject:   subject,
		payload:   append([]byte(nil), payload...),
		messageID: messageID,
	})
	attempt := len(api.calls)
	if api.mode == firstAttemptNotAccepted && attempt == 1 {
		return nil, context.DeadlineExceeded
	}
	ack, err := api.delegate.Publish(ctx, subject, payload, messageID)
	if err != nil {
		return nil, err
	}
	if (api.mode == ackLostAfterAccepted && attempt == 1) || api.mode == everyAckLostAfterAccepted {
		return nil, context.DeadlineExceeded
	}
	api.returnedAcks = append(api.returnedAcks, ack)
	return ack, nil
}

func assertStreamMessages(t *testing.T, admin jetstream.JetStream, expected uint64) {
	t.Helper()
	stream, err := admin.Stream(context.Background(), StreamName)
	if err != nil {
		t.Fatalf("open integration stream: %v", err)
	}
	info, err := stream.Info(context.Background())
	if err != nil {
		t.Fatalf("inspect integration stream: %v", err)
	}
	if info.State.Msgs != expected {
		t.Fatalf("unexpected logical stream message count: got=%d want=%d", info.State.Msgs, expected)
	}
}
