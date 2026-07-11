package feed

import (
	"bytes"
	"context"
	"crypto/sha1"
	"encoding/base64"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"log"
	"net"
	"net/http"
	"net/http/httptest"
	"strings"
	"sync/atomic"
	"testing"
	"time"

	"anti-gravity-phoenix-v4/feed-ingestor/internal/nitro"

	"github.com/gorilla/websocket"
)

func TestNewRelaySourceValidatesSchemes(t *testing.T) {
	if _, err := NewRelaySource("http://nitro-feed-relay:9642/feed", 42161, nil, RelaySourceOptions{}); err == nil {
		t.Fatal("expected HTTP relay URL to be rejected")
	}
	for _, rawURL := range []string{
		"ws://nitro-feed-relay:9642/feed",
		"wss://arb1-feed.arbitrum.io/feed",
	} {
		if _, err := NewRelaySource(rawURL, 42161, nil, RelaySourceOptions{}); err != nil {
			t.Fatalf("expected %s to be accepted: %v", rawURL, err)
		}
	}
}

func TestRelaySourceAssemblesFragmentedTextMessage(t *testing.T) {
	rawURL := newRawRelayServer(t, func(_ int, conn net.Conn) error {
		if err := writeServerFrame(conn, false, 0x1, []byte("hello "), false); err != nil {
			return err
		}
		return writeServerFrame(conn, true, 0x0, []byte("nitro"), false)
	})
	source := newTestRelaySource(t, rawURL, RelaySourceOptions{})

	message, err := source.Next(context.Background())
	if err != nil {
		t.Fatalf("read fragmented text message: %v", err)
	}
	if string(message.Data) != "hello nitro" {
		t.Fatalf("unexpected message: %q", message.Data)
	}
}

func TestRelaySourceAssemblesFragmentedBinaryMessage(t *testing.T) {
	rawURL := newRawRelayServer(t, func(_ int, conn net.Conn) error {
		if err := writeServerFrame(conn, false, 0x2, []byte{0x01, 0x02}, false); err != nil {
			return err
		}
		return writeServerFrame(conn, true, 0x0, []byte{0x03, 0x04}, false)
	})
	source := newTestRelaySource(t, rawURL, RelaySourceOptions{})

	message, err := source.Next(context.Background())
	if err != nil {
		t.Fatalf("read fragmented binary message: %v", err)
	}
	if !bytes.Equal(message.Data, []byte{0x01, 0x02, 0x03, 0x04}) {
		t.Fatalf("unexpected message: %x", message.Data)
	}
}

func TestRelaySourceAcceptsMultipleContinuationFrames(t *testing.T) {
	rawURL := newRawRelayServer(t, func(_ int, conn net.Conn) error {
		frames := []struct {
			final   bool
			opcode  byte
			payload string
		}{
			{final: false, opcode: 0x1, payload: "one"},
			{final: false, opcode: 0x0, payload: "-two"},
			{final: true, opcode: 0x0, payload: "-three"},
		}
		for _, frame := range frames {
			if err := writeServerFrame(conn, frame.final, frame.opcode, []byte(frame.payload), false); err != nil {
				return err
			}
		}
		return nil
	})
	source := newTestRelaySource(t, rawURL, RelaySourceOptions{})

	message, err := source.Next(context.Background())
	if err != nil {
		t.Fatalf("read continuation frames: %v", err)
	}
	if string(message.Data) != "one-two-three" {
		t.Fatalf("unexpected message: %q", message.Data)
	}
}

func TestRelaySourceRespondsToPingBetweenFragments(t *testing.T) {
	rawURL := newRawRelayServer(t, func(_ int, conn net.Conn) error {
		if err := writeServerFrame(conn, false, 0x1, []byte("before-"), false); err != nil {
			return err
		}
		if err := writeServerFrame(conn, true, 0x9, []byte("probe"), false); err != nil {
			return err
		}
		if err := conn.SetReadDeadline(time.Now().Add(time.Second)); err != nil {
			return err
		}
		opcode, payload, err := readClientFrame(conn)
		if err != nil {
			return fmt.Errorf("read pong: %w", err)
		}
		if opcode != 0xA || string(payload) != "probe" {
			return fmt.Errorf("unexpected pong opcode=0x%x payload=%q", opcode, payload)
		}
		return writeServerFrame(conn, true, 0x0, []byte("after"), false)
	})
	source := newTestRelaySource(t, rawURL, RelaySourceOptions{})

	message, err := source.Next(context.Background())
	if err != nil {
		t.Fatalf("read ping-interleaved message: %v", err)
	}
	if string(message.Data) != "before-after" {
		t.Fatalf("unexpected message: %q", message.Data)
	}
}

func TestRelaySourceReadsLargePayloadUsing64BitLength(t *testing.T) {
	payload := bytes.Repeat([]byte{0xA5}, 70*1024)
	rawURL := newRawRelayServer(t, func(_ int, conn net.Conn) error {
		return writeServerFrame(conn, true, 0x2, payload, false)
	})
	source := newTestRelaySource(t, rawURL, RelaySourceOptions{})

	message, err := source.Next(context.Background())
	if err != nil {
		t.Fatalf("read 64-bit-length message: %v", err)
	}
	if !bytes.Equal(message.Data, payload) {
		t.Fatalf("large payload mismatch: got=%d want=%d", len(message.Data), len(payload))
	}
}

func TestRelaySourceReadsPayloadUsing16BitLength(t *testing.T) {
	payload := bytes.Repeat([]byte{0x5A}, 1024)
	rawURL := newRawRelayServer(t, func(_ int, conn net.Conn) error {
		return writeServerFrame(conn, true, 0x2, payload, false)
	})
	source := newTestRelaySource(t, rawURL, RelaySourceOptions{})

	message, err := source.Next(context.Background())
	if err != nil {
		t.Fatalf("read 16-bit-length message: %v", err)
	}
	if !bytes.Equal(message.Data, payload) {
		t.Fatalf("payload mismatch: got=%d want=%d", len(message.Data), len(payload))
	}
}

func TestRelaySourceRejectsMalformedContinuationSequence(t *testing.T) {
	var logs bytes.Buffer
	rawURL := newRawRelayServer(t, firstFailureThenMessage(
		func(conn net.Conn) error {
			return writeServerFrame(conn, true, 0x0, []byte("orphan"), false)
		},
		[]byte("recovered"),
	))
	source := newTestRelaySource(t, rawURL, RelaySourceOptions{Logger: log.New(&logs, "", 0)})

	message, err := source.Next(context.Background())
	if err != nil {
		t.Fatalf("recover after malformed continuation: %v", err)
	}
	if string(message.Data) != "recovered" || !message.AfterReconnect {
		t.Fatalf("unexpected recovered message: %+v", message)
	}
	assertLogContains(t, logs.String(), "type=fragmented_frame_failure")
}

func TestRelaySourceRejectsMaskedServerFrame(t *testing.T) {
	var logs bytes.Buffer
	rawURL := newRawRelayServer(t, firstFailureThenMessage(
		func(conn net.Conn) error {
			return writeServerFrame(conn, true, 0x1, []byte("masked"), true)
		},
		[]byte("recovered"),
	))
	source := newTestRelaySource(t, rawURL, RelaySourceOptions{Logger: log.New(&logs, "", 0)})

	message, err := source.Next(context.Background())
	if err != nil {
		t.Fatalf("recover after masked frame: %v", err)
	}
	if string(message.Data) != "recovered" {
		t.Fatalf("unexpected recovered message: %q", message.Data)
	}
	assertLogContains(t, logs.String(), "type=unexpected_masked_server_frame")
}

func TestRelaySourceLogsCloseFrameReason(t *testing.T) {
	var logs bytes.Buffer
	rawURL := newRawRelayServer(t, firstFailureThenMessage(
		func(conn net.Conn) error {
			payload := websocket.FormatCloseMessage(websocket.CloseNormalClosure, "maintenance")
			return writeServerFrame(conn, true, 0x8, payload, false)
		},
		[]byte("recovered"),
	))
	source := newTestRelaySource(t, rawURL, RelaySourceOptions{Logger: log.New(&logs, "", 0)})

	if _, err := source.Next(context.Background()); err != nil {
		t.Fatalf("recover after close frame: %v", err)
	}
	assertLogContains(t, logs.String(), "type=connection_close")
	assertLogContains(t, logs.String(), "close_code=1000")
	assertLogContains(t, logs.String(), `close_reason="maintenance"`)
}

func TestRelaySourceLogsUnexpectedEOF(t *testing.T) {
	var logs bytes.Buffer
	rawURL := newRawRelayServer(t, firstFailureThenMessage(
		func(net.Conn) error { return nil },
		[]byte("recovered"),
	))
	source := newTestRelaySource(t, rawURL, RelaySourceOptions{Logger: log.New(&logs, "", 0)})

	if _, err := source.Next(context.Background()); err != nil {
		t.Fatalf("recover after unexpected EOF: %v", err)
	}
	assertLogContains(t, logs.String(), "type=unexpected_eof")
}

func TestRelaySourceRejectsUnsupportedOpcode(t *testing.T) {
	var logs bytes.Buffer
	rawURL := newRawRelayServer(t, firstFailureThenMessage(
		func(conn net.Conn) error {
			return writeServerFrame(conn, true, 0x3, nil, false)
		},
		[]byte("recovered"),
	))
	source := newTestRelaySource(t, rawURL, RelaySourceOptions{Logger: log.New(&logs, "", 0)})

	if _, err := source.Next(context.Background()); err != nil {
		t.Fatalf("recover after unsupported opcode: %v", err)
	}
	assertLogContains(t, logs.String(), "type=unsupported_opcode")
}

func TestRelaySourceRejectsOversizedPayload(t *testing.T) {
	var logs bytes.Buffer
	rawURL := newRawRelayServer(t, firstFailureThenMessage(
		func(conn net.Conn) error {
			return writeServerFrame(conn, true, 0x2, bytes.Repeat([]byte{0x01}, 65), false)
		},
		[]byte("ok"),
	))
	source := newTestRelaySource(t, rawURL, RelaySourceOptions{
		Logger:         log.New(&logs, "", 0),
		MaxMessageSize: 64,
	})

	if _, err := source.Next(context.Background()); err != nil {
		t.Fatalf("recover after oversized payload: %v", err)
	}
	assertLogContains(t, logs.String(), "type=oversized_payload")
}

func TestRelaySourceEmitsReconnectLifecycleEvents(t *testing.T) {
	var events []RelayEvent
	rawURL := newRawRelayServer(t, firstFailureThenMessage(
		func(conn net.Conn) error {
			payload := websocket.FormatCloseMessage(websocket.CloseServiceRestart, "restart")
			return writeServerFrame(conn, true, 0x8, payload, false)
		},
		[]byte("recovered"),
	))
	source := newTestRelaySource(t, rawURL, RelaySourceOptions{
		Logger: log.New(io.Discard, "", 0),
		OnEvent: func(event RelayEvent) {
			events = append(events, event)
		},
	})

	message, err := source.Next(context.Background())
	if err != nil {
		t.Fatalf("recover after reconnect: %v", err)
	}
	if !message.AfterReconnect {
		t.Fatal("expected first message after the second connection to be marked as reconnected")
	}
	if len(events) != 3 {
		t.Fatalf("unexpected lifecycle events: %+v", events)
	}
	if events[0].Kind != RelayEventConnected || events[0].Reconnected {
		t.Fatalf("unexpected initial connection event: %+v", events[0])
	}
	if events[1].Kind != RelayEventReconnectAttempt || events[1].Attempt != 2 || events[1].Backoff <= 0 {
		t.Fatalf("unexpected reconnect event: %+v", events[1])
	}
	if events[2].Kind != RelayEventConnected || !events[2].Reconnected {
		t.Fatalf("unexpected reconnected event: %+v", events[2])
	}
}

func TestRelaySourceLogsDialFailureAndReconnectBackoff(t *testing.T) {
	listener, err := net.Listen("tcp", "127.0.0.1:0")
	if err != nil {
		t.Fatalf("reserve local address: %v", err)
	}
	address := listener.Addr().String()
	if err := listener.Close(); err != nil {
		t.Fatalf("close reserved address: %v", err)
	}

	var logs bytes.Buffer
	source := newTestRelaySource(t, "ws://"+address+"/feed", RelaySourceOptions{
		Logger: log.New(&logs, "", 0),
	})
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Millisecond)
	defer cancel()
	if _, err := source.Next(ctx); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("expected deadline after dial retries, got %v", err)
	}
	assertLogContains(t, logs.String(), "type=websocket_dial_failure")
	assertLogContains(t, logs.String(), "event=feed_websocket_reconnect_attempt")
	assertLogContains(t, logs.String(), "backoff=")
}

func TestRelaySourceLogsHandshakeFailure(t *testing.T) {
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, _ *http.Request) {
		http.Error(w, "not a websocket", http.StatusForbidden)
	}))
	t.Cleanup(server.Close)

	var logs bytes.Buffer
	rawURL := "ws" + strings.TrimPrefix(server.URL, "http") + "/feed"
	source := newTestRelaySource(t, rawURL, RelaySourceOptions{Logger: log.New(&logs, "", 0)})
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Millisecond)
	defer cancel()
	if _, err := source.Next(ctx); !errors.Is(err, context.DeadlineExceeded) {
		t.Fatalf("expected deadline after handshake retries, got %v", err)
	}
	assertLogContains(t, logs.String(), "type=websocket_handshake_failure")
	assertLogContains(t, logs.String(), "status_code=403")
}

func TestClassifyTransportError(t *testing.T) {
	tests := []struct {
		name string
		err  error
		want string
	}{
		{name: "frame read", err: errors.New("read tcp: connection reset by peer"), want: "frame_read_failure"},
		{name: "unexpected EOF", err: io.ErrUnexpectedEOF, want: "unexpected_eof"},
		{name: "unsupported opcode", err: errors.New("websocket: bad opcode 3"), want: "unsupported_opcode"},
		{name: "fragmented frame", err: errors.New("websocket: continuation after FIN"), want: "fragmented_frame_failure"},
		{name: "masked server frame", err: errors.New("websocket: bad MASK"), want: "unexpected_masked_server_frame"},
		{name: "oversized", err: websocket.ErrReadLimit, want: "oversized_payload"},
	}
	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			got, _ := classifyTransportError(tt.err)
			if got != tt.want {
				t.Fatalf("classify %q: got=%q want=%q", tt.err, got, tt.want)
			}
		})
	}
}

func newTestRelaySource(t *testing.T, rawURL string, options RelaySourceOptions) *RelaySource {
	t.Helper()
	if options.Timeout <= 0 {
		options.Timeout = time.Second
	}
	if options.MinBackoff <= 0 {
		options.MinBackoff = time.Millisecond
	}
	if options.MaxBackoff <= 0 {
		options.MaxBackoff = 2 * time.Millisecond
	}
	if options.Logger == nil {
		options.Logger = log.New(io.Discard, "", 0)
	}
	source, err := NewRelaySource(rawURL, nitro.ArbitrumOneChainID, func() uint64 { return 17 }, options)
	if err != nil {
		t.Fatalf("create relay source: %v", err)
	}
	t.Cleanup(func() { _ = source.Close() })
	return source
}

func newRawRelayServer(t *testing.T, script func(int, net.Conn) error) string {
	t.Helper()
	var connections atomic.Int64
	server := httptest.NewServer(http.HandlerFunc(func(w http.ResponseWriter, request *http.Request) {
		connectionNumber := int(connections.Add(1))
		if got := request.Header.Get(nitro.HeaderFeedClientVersion); got != fmt.Sprint(nitro.FeedClientVersion) {
			t.Errorf("unexpected feed client version header: %q", got)
		}
		if got := request.Header.Get(nitro.HeaderRequestedSequence); got != "17" {
			t.Errorf("unexpected requested sequence header: %q", got)
		}
		if got := request.Header.Get(nitro.HeaderChainID); got != fmt.Sprint(nitro.ArbitrumOneChainID) {
			t.Errorf("unexpected chain header: %q", got)
		}

		hijacker, ok := w.(http.Hijacker)
		if !ok {
			t.Errorf("test server does not support hijacking")
			return
		}
		conn, buffered, err := hijacker.Hijack()
		if err != nil {
			t.Errorf("hijack test connection: %v", err)
			return
		}
		defer conn.Close()

		key := request.Header.Get("Sec-WebSocket-Key")
		if key == "" {
			t.Errorf("missing websocket key")
			return
		}
		_, _ = fmt.Fprintf(buffered, "HTTP/1.1 101 Switching Protocols\r\n")
		_, _ = fmt.Fprintf(buffered, "Upgrade: websocket\r\n")
		_, _ = fmt.Fprintf(buffered, "Connection: Upgrade\r\n")
		_, _ = fmt.Fprintf(buffered, "Sec-WebSocket-Accept: %s\r\n", testWebSocketAccept(key))
		_, _ = fmt.Fprintf(buffered, "%s: %d\r\n", nitro.HeaderFeedServerVersion, nitro.FeedServerVersion)
		_, _ = fmt.Fprintf(buffered, "%s: %d\r\n", nitro.HeaderChainID, nitro.ArbitrumOneChainID)
		_, _ = fmt.Fprintf(buffered, "\r\n")
		if err := buffered.Flush(); err != nil {
			t.Errorf("flush test handshake: %v", err)
			return
		}
		if err := script(connectionNumber, conn); err != nil {
			t.Errorf("relay test script connection %d: %v", connectionNumber, err)
		}
	}))
	t.Cleanup(server.Close)
	return "ws" + strings.TrimPrefix(server.URL, "http") + "/feed"
}

func firstFailureThenMessage(failure func(net.Conn) error, message []byte) func(int, net.Conn) error {
	return func(connectionNumber int, conn net.Conn) error {
		if connectionNumber == 1 {
			return failure(conn)
		}
		return writeServerFrame(conn, true, 0x1, message, false)
	}
}

func writeServerFrame(w io.Writer, final bool, opcode byte, payload []byte, masked bool) error {
	first := opcode
	if final {
		first |= 0x80
	}
	frame := []byte{first}
	maskBit := byte(0)
	if masked {
		maskBit = 0x80
	}
	switch {
	case len(payload) <= 125:
		frame = append(frame, maskBit|byte(len(payload)))
	case len(payload) <= 0xffff:
		frame = append(frame, maskBit|126, byte(len(payload)>>8), byte(len(payload)))
	default:
		frame = append(frame, maskBit|127)
		var length [8]byte
		binary.BigEndian.PutUint64(length[:], uint64(len(payload)))
		frame = append(frame, length[:]...)
	}
	if masked {
		mask := [4]byte{0x10, 0x20, 0x30, 0x40}
		frame = append(frame, mask[:]...)
		for index, value := range payload {
			frame = append(frame, value^mask[index%len(mask)])
		}
	} else {
		frame = append(frame, payload...)
	}
	_, err := w.Write(frame)
	return err
}

func readClientFrame(r io.Reader) (byte, []byte, error) {
	var header [2]byte
	if _, err := io.ReadFull(r, header[:]); err != nil {
		return 0, nil, err
	}
	if header[1]&0x80 == 0 {
		return 0, nil, errors.New("client frame is not masked")
	}
	length := uint64(header[1] & 0x7f)
	switch length {
	case 126:
		var extended [2]byte
		if _, err := io.ReadFull(r, extended[:]); err != nil {
			return 0, nil, err
		}
		length = uint64(binary.BigEndian.Uint16(extended[:]))
	case 127:
		var extended [8]byte
		if _, err := io.ReadFull(r, extended[:]); err != nil {
			return 0, nil, err
		}
		length = binary.BigEndian.Uint64(extended[:])
	}
	var mask [4]byte
	if _, err := io.ReadFull(r, mask[:]); err != nil {
		return 0, nil, err
	}
	payload := make([]byte, length)
	if _, err := io.ReadFull(r, payload); err != nil {
		return 0, nil, err
	}
	for index := range payload {
		payload[index] ^= mask[index%len(mask)]
	}
	return header[0] & 0x0f, payload, nil
}

func testWebSocketAccept(key string) string {
	hash := sha1.Sum([]byte(key + "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"))
	return base64.StdEncoding.EncodeToString(hash[:])
}

func assertLogContains(t *testing.T, logs, want string) {
	t.Helper()
	if !strings.Contains(logs, want) {
		t.Fatalf("log missing %q:\n%s", want, logs)
	}
}
