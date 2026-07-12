package publisher

import (
	"context"
	"encoding/json"
	"errors"
	"time"

	"github.com/nats-io/nats.go"
	"github.com/nats-io/nats.go/jetstream"
)

const (
	StreamName            = "PHOENIX_FEED_TX"
	StreamSubject         = "phoenix.feed.tx"
	StreamMaxMessages     = 5_000_000
	StreamMaxBytes        = 2 * 1024 * 1024 * 1024
	StreamMaxMessageBytes = 1024 * 1024
	StreamMaxAge          = 24 * time.Hour
	StreamDuplicateWindow = 2 * time.Minute
)

var (
	ErrNATSUnavailable       = errors.New("NATS connection unavailable")
	ErrStreamUnavailable     = errors.New("JetStream stream unavailable")
	ErrPublishAckTimeout     = errors.New("JetStream publish acknowledgement timed out")
	ErrPublishAckUnavailable = errors.New("JetStream publish acknowledgement unavailable")
	ErrInvalidPublishAck     = errors.New("JetStream returned an invalid publish acknowledgement")
)

type Publisher interface {
	Publish(context.Context, string, any) error
	Close() error
}

type ConnectionEvents struct {
	Disconnected func()
	Reconnected  func()
}

type durableMessageID interface {
	DurableMessageID() string
}

type jetStreamAPI interface {
	EnsureStream(context.Context, jetstream.StreamConfig) error
	Publish(context.Context, string, []byte, ...jetstream.PublishOpt) (*jetstream.PubAck, error)
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

type JetStreamPublisher struct {
	api        jetStreamAPI
	connection connectionCloser
	timeout    time.Duration
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
		api:        natsJetStreamAPI{JetStream: js},
		connection: connection,
		timeout:    timeout,
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
	publisher := &JetStreamPublisher{api: api, connection: connection, timeout: timeout}
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
	payload, err := json.Marshal(value)
	if err != nil {
		return ErrPublishAckUnavailable
	}
	ctx, cancel := context.WithTimeout(parent, p.timeout)
	defer cancel()

	options := []jetstream.PublishOpt{jetstream.WithExpectStream(StreamName)}
	if identified, ok := value.(durableMessageID); ok {
		options = append(options, jetstream.WithMsgID(identified.DurableMessageID()))
	}
	ack, err := p.api.Publish(ctx, subject, payload, options...)
	if err != nil {
		if errors.Is(ctx.Err(), context.DeadlineExceeded) || errors.Is(err, context.DeadlineExceeded) {
			return ErrPublishAckTimeout
		}
		if errors.Is(err, jetstream.ErrStreamNotFound) || errors.Is(err, jetstream.ErrNoStreamResponse) || errors.Is(err, jetstream.ErrJetStreamNotEnabled) {
			return ErrStreamUnavailable
		}
		return ErrPublishAckUnavailable
	}
	if ack == nil || ack.Stream != StreamName || ack.Sequence == 0 {
		return ErrInvalidPublishAck
	}
	return nil
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
