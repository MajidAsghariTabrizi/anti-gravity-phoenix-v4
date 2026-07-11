package feed

import (
	"context"
	"errors"
	"fmt"
	"io"
	"log"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"

	"anti-gravity-phoenix-v4/feed-ingestor/internal/nitro"

	"github.com/gorilla/websocket"
)

const (
	defaultRelayTimeout     = 30 * time.Second
	defaultRelayMinBackoff  = 250 * time.Millisecond
	defaultRelayMaxBackoff  = 5 * time.Second
	defaultRelayMessageSize = 64 * 1024 * 1024
)

type RelayMessage struct {
	Data           []byte
	AfterReconnect bool
}

type RelayEventKind string

const (
	RelayEventConnected        RelayEventKind = "connected"
	RelayEventReconnectAttempt RelayEventKind = "reconnect_attempt"
)

type RelayEvent struct {
	Kind        RelayEventKind
	Reconnected bool
	Attempt     uint64
	Backoff     time.Duration
}

type RelaySourceOptions struct {
	Timeout        time.Duration
	MinBackoff     time.Duration
	MaxBackoff     time.Duration
	MaxMessageSize int64
	Logger         *log.Logger
	OnEvent        func(RelayEvent)
}

type RelaySource struct {
	url            *url.URL
	chainID        uint64
	nextSequence   func() uint64
	timeout        time.Duration
	minBackoff     time.Duration
	maxBackoff     time.Duration
	maxMessageSize int64
	logger         *log.Logger
	onEvent        func(RelayEvent)

	conn                      *websocket.Conn
	hadConnection             bool
	nextMessageAfterReconnect bool
	connectAttempts           uint64
}

type relayTransportError struct {
	kind       string
	statusCode int
	err        error
}

func (e *relayTransportError) Error() string {
	return e.err.Error()
}

func (e *relayTransportError) Unwrap() error {
	return e.err
}

func NewRelaySource(rawURL string, chainID uint64, nextSequence func() uint64, options RelaySourceOptions) (*RelaySource, error) {
	parsed, err := url.Parse(rawURL)
	if err != nil {
		return nil, err
	}
	if parsed.Scheme != "ws" && parsed.Scheme != "wss" {
		return nil, fmt.Errorf("unsupported relay websocket scheme %q", parsed.Scheme)
	}
	if parsed.Host == "" {
		return nil, errors.New("relay websocket URL is missing host")
	}
	if nextSequence == nil {
		nextSequence = func() uint64 { return 0 }
	}
	if options.Timeout <= 0 {
		options.Timeout = defaultRelayTimeout
	}
	if options.MinBackoff <= 0 {
		options.MinBackoff = defaultRelayMinBackoff
	}
	if options.MaxBackoff <= 0 {
		options.MaxBackoff = defaultRelayMaxBackoff
	}
	if options.MaxBackoff < options.MinBackoff {
		options.MaxBackoff = options.MinBackoff
	}
	if options.MaxMessageSize <= 0 {
		options.MaxMessageSize = defaultRelayMessageSize
	}
	if options.Logger == nil {
		options.Logger = log.Default()
	}

	return &RelaySource{
		url:            parsed,
		chainID:        chainID,
		nextSequence:   nextSequence,
		timeout:        options.Timeout,
		minBackoff:     options.MinBackoff,
		maxBackoff:     options.MaxBackoff,
		maxMessageSize: options.MaxMessageSize,
		logger:         options.Logger,
		onEvent:        options.OnEvent,
	}, nil
}

func (s *RelaySource) Next(ctx context.Context) (RelayMessage, error) {
	backoff := s.minBackoff
	for {
		if s.conn == nil {
			if s.connectAttempts > 0 {
				if err := sleepContext(ctx, backoff); err != nil {
					return RelayMessage{}, err
				}
				s.emitEvent(RelayEvent{
					Kind:    RelayEventReconnectAttempt,
					Attempt: s.connectAttempts + 1,
					Backoff: backoff,
				})
			}

			s.connectAttempts++
			afterReconnect := s.hadConnection
			if err := s.connect(ctx); err != nil {
				if ctx.Err() != nil {
					return RelayMessage{}, ctx.Err()
				}
				s.logTransportError(err)
				backoff = nextBackoff(backoff, s.maxBackoff)
				continue
			}
			s.hadConnection = true
			s.nextMessageAfterReconnect = afterReconnect
			backoff = s.minBackoff
			s.emitEvent(RelayEvent{
				Kind:        RelayEventConnected,
				Reconnected: afterReconnect,
				Attempt:     s.connectAttempts,
			})
		}

		payload, err := s.readMessage(ctx)
		if err == nil {
			afterReconnect := s.nextMessageAfterReconnect
			s.nextMessageAfterReconnect = false
			return RelayMessage{Data: payload, AfterReconnect: afterReconnect}, nil
		}
		if ctx.Err() != nil {
			_ = s.Close()
			return RelayMessage{}, ctx.Err()
		}
		s.logTransportError(err)
		s.dropConn()
	}
}

func (s *RelaySource) Close() error {
	if s.conn == nil {
		return nil
	}
	conn := s.conn
	s.conn = nil
	deadline := time.Now().Add(time.Second)
	writeErr := conn.WriteControl(
		websocket.CloseMessage,
		websocket.FormatCloseMessage(websocket.CloseNormalClosure, "shutdown"),
		deadline,
	)
	closeErr := conn.Close()
	if closeErr != nil {
		return closeErr
	}
	return writeErr
}

func (s *RelaySource) connect(ctx context.Context) error {
	dialCtx, cancel := context.WithTimeout(ctx, s.timeout)
	defer cancel()

	headers := http.Header{}
	headers.Set(nitro.HeaderFeedClientVersion, strconv.Itoa(nitro.FeedClientVersion))
	headers.Set(nitro.HeaderRequestedSequence, strconv.FormatUint(s.nextSequence(), 10))
	headers.Set(nitro.HeaderChainID, strconv.FormatUint(s.chainID, 10))

	dialer := websocket.Dialer{
		Proxy:            http.ProxyFromEnvironment,
		HandshakeTimeout: s.timeout,
	}
	conn, response, err := dialer.DialContext(dialCtx, s.url.String(), headers)
	if err != nil {
		kind := "websocket_dial_failure"
		statusCode := 0
		if errors.Is(err, websocket.ErrBadHandshake) || response != nil {
			kind = "websocket_handshake_failure"
			if response != nil {
				statusCode = response.StatusCode
			}
		}
		return &relayTransportError{kind: kind, statusCode: statusCode, err: err}
	}

	if err := s.validateHandshake(response); err != nil {
		_ = conn.Close()
		statusCode := 0
		if response != nil {
			statusCode = response.StatusCode
		}
		return &relayTransportError{
			kind:       "websocket_handshake_failure",
			statusCode: statusCode,
			err:        err,
		}
	}

	conn.SetReadLimit(s.maxMessageSize)
	s.conn = conn
	return nil
}

func (s *RelaySource) validateHandshake(response *http.Response) error {
	if response == nil {
		return errors.New("relay websocket handshake returned no response")
	}
	if got := response.Header.Get(nitro.HeaderFeedServerVersion); got != strconv.Itoa(nitro.FeedServerVersion) {
		return fmt.Errorf("relay feed server version mismatch: %q", got)
	}
	if got := response.Header.Get(nitro.HeaderChainID); got != strconv.FormatUint(s.chainID, 10) {
		return fmt.Errorf("relay feed chain id mismatch: %q", got)
	}
	return nil
}

func (s *RelaySource) readMessage(ctx context.Context) ([]byte, error) {
	conn := s.conn
	deadline := time.Now().Add(s.timeout)
	if contextDeadline, ok := ctx.Deadline(); ok && contextDeadline.Before(deadline) {
		deadline = contextDeadline
	}
	if err := conn.SetReadDeadline(deadline); err != nil {
		return nil, err
	}

	cancelFinished := make(chan struct{})
	stopCancellation := context.AfterFunc(ctx, func() {
		_ = conn.SetReadDeadline(time.Now())
		close(cancelFinished)
	})
	messageType, payload, err := conn.ReadMessage()
	if !stopCancellation() {
		<-cancelFinished
	}
	if ctx.Err() != nil {
		return nil, ctx.Err()
	}
	if err != nil {
		return nil, err
	}
	if messageType != websocket.TextMessage && messageType != websocket.BinaryMessage {
		return nil, fmt.Errorf("unsupported websocket message type %d", messageType)
	}
	return payload, nil
}

func (s *RelaySource) emitEvent(event RelayEvent) {
	switch event.Kind {
	case RelayEventConnected:
		s.logger.Printf(
			"event=feed_websocket_connected source=relay attempt=%d reconnected=%t",
			event.Attempt,
			event.Reconnected,
		)
	case RelayEventReconnectAttempt:
		s.logger.Printf(
			"event=feed_websocket_reconnect_attempt source=relay attempt=%d backoff=%q",
			event.Attempt,
			event.Backoff.String(),
		)
	}
	if s.onEvent != nil {
		s.onEvent(event)
	}
}

func (s *RelaySource) logTransportError(err error) {
	kind, statusCode := classifyTransportError(err)
	var closeErr *websocket.CloseError
	if errors.As(err, &closeErr) {
		s.logger.Printf(
			"event=feed_websocket_transport_error source=relay type=%s close_code=%d close_reason=%q error=%q",
			kind,
			closeErr.Code,
			closeErr.Text,
			err.Error(),
		)
		return
	}
	if statusCode != 0 {
		s.logger.Printf(
			"event=feed_websocket_transport_error source=relay type=%s status_code=%d error=%q",
			kind,
			statusCode,
			err.Error(),
		)
		return
	}
	s.logger.Printf(
		"event=feed_websocket_transport_error source=relay type=%s error=%q",
		kind,
		err.Error(),
	)
}

func classifyTransportError(err error) (string, int) {
	var transportErr *relayTransportError
	if errors.As(err, &transportErr) {
		return transportErr.kind, transportErr.statusCode
	}
	if errors.Is(err, websocket.ErrReadLimit) {
		return "oversized_payload", 0
	}

	message := strings.ToLower(err.Error())
	if errors.Is(err, io.ErrUnexpectedEOF) || strings.Contains(message, "unexpected eof") {
		return "unexpected_eof", 0
	}
	if strings.Contains(message, "bad mask") || strings.Contains(message, "masked server frame") {
		return "unexpected_masked_server_frame", 0
	}
	if strings.Contains(message, "bad opcode") || strings.Contains(message, "unknown opcode") || strings.Contains(message, "unsupported websocket message") {
		return "unsupported_opcode", 0
	}
	if strings.Contains(message, "continuation") || strings.Contains(message, "data before fin") || strings.Contains(message, "fin not set on control") || strings.Contains(message, "fragment") {
		return "fragmented_frame_failure", 0
	}

	var closeErr *websocket.CloseError
	if errors.As(err, &closeErr) {
		return "connection_close", 0
	}
	return "frame_read_failure", 0
}

func (s *RelaySource) dropConn() {
	if s.conn != nil {
		_ = s.conn.Close()
	}
	s.conn = nil
}

func nextBackoff(current, max time.Duration) time.Duration {
	next := current * 2
	if next > max {
		return max
	}
	return next
}

func sleepContext(ctx context.Context, duration time.Duration) error {
	timer := time.NewTimer(duration)
	defer timer.Stop()
	select {
	case <-ctx.Done():
		return ctx.Err()
	case <-timer.C:
		return nil
	}
}
