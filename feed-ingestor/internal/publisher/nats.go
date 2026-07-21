package publisher

import (
	"context"
	"encoding/json"
	"errors"
	"strings"
	"time"

	"github.com/nats-io/nats.go"
	"github.com/nats-io/nats.go/jetstream"
)

const (
	StreamName               = "PHOENIX_FEED_TX"
	StreamSubject            = "phoenix.feed.tx"
	StreamMaxMessages        = 5_000_000
	StreamMaxBytes           = 2 * 1024 * 1024 * 1024
	StreamMaxMessageBytes    = 1024 * 1024
	StreamMaxAge             = 24 * time.Hour
	StreamDuplicateWindow    = 2 * time.Minute
	PublishMaxAttempts       = 3
	PublishMaxMessageIDBytes = 128
	PublishRetryBackoff      = 250 * time.Millisecond
	PublishRetryBudget       = 10 * time.Second
)

var (
	ErrNATSUnavailable       = errors.New("NATS connection unavailable")
	ErrStreamUnavailable     = errors.New("JetStream stream unavailable")
	ErrPublishAckTimeout     = errors.New("JetStream publish acknowledgement timed out")
	ErrPublishAckUnavailable = errors.New("JetStream publish acknowledgement unavailable")
	ErrInvalidPublishAck     = errors.New("JetStream returned an invalid publish acknowledgement")
	ErrPublishIdentity       = errors.New("JetStream publication identity unavailable")
)

type Publisher interface {
	Publish(context.Context, string, any) error
	Close() error
}

type ConnectionEvents struct {
	Disconnected func()
	Reconnected  func()
	PublishState func(PublishEvent)
}

type PublishEventKind string

const (
	PublishEventAckTimeoutRetry    PublishEventKind = "ack_timeout_retry"
	PublishEventRecoveredNormal    PublishEventKind = "recovered_normal_ack"
	PublishEventRecoveredDuplicate PublishEventKind = "recovered_duplicate_ack"
	PublishEventRetryExhausted     PublishEventKind = "retry_exhausted"
)

type PublishEvent struct {
	Kind        PublishEventKind
	Subject     string
	MessageID   string
	Attempt     int
	MaxAttempts int
	Elapsed     time.Duration
}

type durableMessageID interface {
	DurableMessageID() string
}

type jetStreamAPI interface {
	EnsureStream(context.Context, jetstream.StreamConfig) error
	Publish(context.Context, string, []byte, string) (*jetstream.PubAck, error)
}

type connectionCloser interface {
	Close()
}

type natsJetStreamAPI struct {
	jetstream.JetStream
}

func (api natsJetStreamAPI) EnsureStream(ctx context.Context, config jetstream.StreamConfig) error {
	_, err := api.CreateOrUpdateStream(ctx, config)
	return err
}

func (api natsJetStreamAPI) Publish(ctx context.Context, subject string, payload []byte, messageID string) (*jetstream.PubAck, error) {
	return api.JetStream.Publish(
		ctx,
		subject,
		payload,
		jetstream.WithExpectStream(StreamName),
		jetstream.WithMsgID(messageID),
	)
}

type JetStreamPublisher struct {
	api          jetStreamAPI
	connection   connectionCloser
	timeout      time.Duration
	maxAttempts  int
	retryBackoff time.Duration
	retryBudget  time.Duration
	publishState func(PublishEvent)
}

func DialJetStream(addr string, timeout time.Duration, events ConnectionEvents) (*JetStreamPublisher, error) {
	options := []nats.Option{
		nats.Name("phoenix-feed-ingestor"),
		nats.Timeout(timeout),
		nats.ReconnectWait(500 * time.Millisecond),
		nats.MaxReconnects(-1),
		nats.PingInterval(20 * time.Second),
		nats.MaxPingsOutstanding(2),
	}
	if events.Disconnected != nil {
		options = append(options, nats.DisconnectErrHandler(func(_ *nats.Conn, _ error) {
			events.Disconnected()
		}))
	}
	if events.Reconnected != nil {
		options = append(options, nats.ReconnectHandler(func(_ *nats.Conn) {
			events.Reconnected()
		}))
	}
	connection, err := nats.Connect(addr, options...)
	if err != nil {
		return nil, ErrNATSUnavailable
	}

	js, err := jetstream.New(connection, jetstream.WithDefaultTimeout(timeout))
	if err != nil {
		connection.Close()
		return nil, ErrStreamUnavailable
	}
	publisher := &JetStreamPublisher{
		api:          natsJetStreamAPI{JetStream: js},
		connection:   connection,
		timeout:      timeout,
		maxAttempts:  PublishMaxAttempts,
		retryBackoff: PublishRetryBackoff,
		retryBudget:  PublishRetryBudget,
		publishState: events.PublishState,
	}
	if err := publisher.ensureStream(context.Background()); err != nil {
		connection.Close()
		return nil, err
	}
	return publisher, nil
}

func StreamConfig() jetstream.StreamConfig {
	return jetstream.StreamConfig{
		Name:              StreamName,
		Description:       "Durable normalized Arbitrum transactions for the Phoenix Recorder",
		Subjects:          []string{StreamSubject},
		Retention:         jetstream.WorkQueuePolicy,
		MaxConsumers:      1,
		MaxMsgs:           StreamMaxMessages,
		MaxBytes:          StreamMaxBytes,
		Discard:           jetstream.DiscardNew,
		MaxAge:            StreamMaxAge,
		MaxMsgsPerSubject: -1,
		MaxMsgSize:        StreamMaxMessageBytes,
		Storage:           jetstream.FileStorage,
		Replicas:          1,
		Duplicates:        StreamDuplicateWindow,
	}
}

func newJetStreamPublisher(ctx context.Context, api jetStreamAPI, connection connectionCloser, timeout time.Duration) (*JetStreamPublisher, error) {
	publisher := &JetStreamPublisher{
		api:          api,
		connection:   connection,
		timeout:      timeout,
		maxAttempts:  PublishMaxAttempts,
		retryBackoff: PublishRetryBackoff,
		retryBudget:  PublishRetryBudget,
	}
	if err := publisher.ensureStream(ctx); err != nil {
		return nil, err
	}
	return publisher, nil
}

func (p *JetStreamPublisher) ensureStream(parent context.Context) error {
	ctx, cancel := context.WithTimeout(parent, p.timeout)
	defer cancel()
	if err := p.api.EnsureStream(ctx, StreamConfig()); err != nil {
		return ErrStreamUnavailable
	}
	return nil
}

func (p *JetStreamPublisher) Publish(parent context.Context, subject string, value any) error {
	identified, ok := value.(durableMessageID)
	if !ok {
		return ErrPublishIdentity
	}
	messageID := identified.DurableMessageID()
	if !validMessageID(messageID) {
		return ErrPublishIdentity
	}
	payload, err := json.Marshal(value)
	if err != nil {
		return ErrPublishAckUnavailable
	}
	started := time.Now()
	budgetCtx, cancel := context.WithTimeout(parent, p.retryBudget)
	defer cancel()

	sawTimeout := false
	for attempt := 1; attempt <= p.maxAttempts; attempt++ {
		if attempt > 1 && !waitForRetry(budgetCtx, p.retryBackoff) {
			if errors.Is(budgetCtx.Err(), context.DeadlineExceeded) {
				p.notify(PublishEventRetryExhausted, subject, messageID, attempt-1, started)
				return ErrPublishAckTimeout
			}
			return ErrPublishAckUnavailable
		}

		attemptCtx, attemptCancel := context.WithTimeout(budgetCtx, p.timeout)
		ack, publishErr := p.api.Publish(attemptCtx, subject, payload, messageID)
		attemptTimedOut := errors.Is(attemptCtx.Err(), context.DeadlineExceeded) ||
			errors.Is(publishErr, context.DeadlineExceeded)
		attemptCancel()

		if publishErr != nil {
			if attemptTimedOut {
				sawTimeout = true
				if attempt < p.maxAttempts && budgetCtx.Err() == nil {
					p.notify(PublishEventAckTimeoutRetry, subject, messageID, attempt, started)
					continue
				}
				p.notify(PublishEventRetryExhausted, subject, messageID, attempt, started)
				return ErrPublishAckTimeout
			}
			if errors.Is(publishErr, jetstream.ErrStreamNotFound) || errors.Is(publishErr, jetstream.ErrNoStreamResponse) || errors.Is(publishErr, jetstream.ErrJetStreamNotEnabled) {
				return ErrStreamUnavailable
			}
			return ErrPublishAckUnavailable
		}
		if ack == nil || ack.Stream != StreamName || ack.Sequence == 0 {
			return ErrInvalidPublishAck
		}
		if sawTimeout {
			kind := PublishEventRecoveredNormal
			if ack.Duplicate {
				kind = PublishEventRecoveredDuplicate
			}
			p.notify(kind, subject, messageID, attempt, started)
		}
		return nil
	}
	p.notify(PublishEventRetryExhausted, subject, messageID, p.maxAttempts, started)
	return ErrPublishAckTimeout
}

func validMessageID(messageID string) bool {
	separator := strings.IndexByte(messageID, ':')
	return separator > 0 &&
		separator < len(messageID)-1 &&
		len(messageID) <= PublishMaxMessageIDBytes &&
		strings.TrimSpace(messageID) == messageID &&
		!strings.ContainsAny(messageID, " \t\r\n")
}

func waitForRetry(ctx context.Context, delay time.Duration) bool {
	if delay <= 0 {
		return ctx.Err() == nil
	}
	timer := time.NewTimer(delay)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return false
	case <-timer.C:
		return true
	}
}

func (p *JetStreamPublisher) notify(kind PublishEventKind, subject, messageID string, attempt int, started time.Time) {
	if p.publishState == nil {
		return
	}
	p.publishState(PublishEvent{
		Kind:        kind,
		Subject:     subject,
		MessageID:   messageID,
		Attempt:     attempt,
		MaxAttempts: p.maxAttempts,
		Elapsed:     time.Since(started),
	})
}

func (p *JetStreamPublisher) Close() error {
	if p.connection != nil {
		p.connection.Close()
	}
	return nil
}

type MemoryPublisher struct {
	Messages []PublishedMessage
}

type PublishedMessage struct {
	Subject string
	Value   any
}

func (p *MemoryPublisher) Publish(_ context.Context, subject string, value any) error {
	p.Messages = append(p.Messages, PublishedMessage{Subject: subject, Value: value})
	return nil
}

func (p *MemoryPublisher) Close() error { return nil }
