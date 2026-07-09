package feed

import (
	"bufio"
	"context"
	"crypto/rand"
	"crypto/sha1"
	"encoding/base64"
	"encoding/binary"
	"errors"
	"fmt"
	"io"
	"net"
	"net/http"
	"net/url"
	"strconv"
	"strings"
	"time"

	"anti-gravity-phoenix-v4/feed-ingestor/internal/nitro"
)

const websocketGUID = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11"

type RelayMessage struct {
	Data           []byte
	Connected      bool
	AfterReconnect bool
}

type RelaySource struct {
	url          *url.URL
	chainID      uint64
	nextSequence func() uint64
	timeout      time.Duration
	minBackoff   time.Duration
	maxBackoff   time.Duration

	conn          net.Conn
	reader        *bufio.Reader
	hadConnection bool
}

func NewRelaySource(rawURL string, chainID uint64, nextSequence func() uint64, timeout time.Duration) (*RelaySource, error) {
	parsed, err := url.Parse(rawURL)
	if err != nil {
		return nil, err
	}
	if parsed.Scheme != "ws" {
		return nil, fmt.Errorf("unsupported relay websocket scheme %q", parsed.Scheme)
	}
	if parsed.Host == "" {
		return nil, errors.New("relay websocket URL is missing host")
	}
	if nextSequence == nil {
		nextSequence = func() uint64 { return 0 }
	}
	if timeout <= 0 {
		timeout = 30 * time.Second
	}
	return &RelaySource{
		url:          parsed,
		chainID:      chainID,
		nextSequence: nextSequence,
		timeout:      timeout,
		minBackoff:   250 * time.Millisecond,
		maxBackoff:   5 * time.Second,
	}, nil
}

func (s *RelaySource) Next(ctx context.Context) (RelayMessage, error) {
	backoff := s.minBackoff
	for {
		connected := false
		afterReconnect := false
		if s.conn == nil {
			afterReconnect = s.hadConnection
			if err := s.connect(ctx); err != nil {
				if err := sleepContext(ctx, backoff); err != nil {
					return RelayMessage{}, err
				}
				backoff = nextBackoff(backoff, s.maxBackoff)
				continue
			}
			s.hadConnection = true
			connected = true
			backoff = s.minBackoff
		}

		payload, err := s.readDataFrame(ctx)
		if err == nil {
			return RelayMessage{Data: payload, Connected: connected, AfterReconnect: afterReconnect}, nil
		}
		s.closeConn()
		if err := sleepContext(ctx, backoff); err != nil {
			return RelayMessage{}, err
		}
		backoff = nextBackoff(backoff, s.maxBackoff)
	}
}

func (s *RelaySource) Close() error {
	if s.conn == nil {
		return nil
	}
	conn := s.conn
	s.conn = nil
	s.reader = nil
	return conn.Close()
}

func (s *RelaySource) connect(ctx context.Context) error {
	dialer := net.Dialer{Timeout: s.timeout}
	conn, err := dialer.DialContext(ctx, "tcp", s.url.Host)
	if err != nil {
		return err
	}
	s.conn = conn
	s.reader = bufio.NewReader(conn)
	if err := s.handshake(); err != nil {
		s.closeConn()
		return err
	}
	return nil
}

func (s *RelaySource) handshake() error {
	key, err := websocketKey()
	if err != nil {
		return err
	}
	path := s.url.EscapedPath()
	if path == "" {
		path = "/"
	}
	if s.url.RawQuery != "" {
		path += "?" + s.url.RawQuery
	}
	request := strings.Builder{}
	fmt.Fprintf(&request, "GET %s HTTP/1.1\r\n", path)
	fmt.Fprintf(&request, "Host: %s\r\n", s.url.Host)
	request.WriteString("Upgrade: websocket\r\n")
	request.WriteString("Connection: Upgrade\r\n")
	fmt.Fprintf(&request, "Sec-WebSocket-Key: %s\r\n", key)
	request.WriteString("Sec-WebSocket-Version: 13\r\n")
	fmt.Fprintf(&request, "%s: %d\r\n", nitro.HeaderFeedClientVersion, nitro.FeedClientVersion)
	fmt.Fprintf(&request, "%s: %d\r\n", nitro.HeaderRequestedSequence, s.nextSequence())
	fmt.Fprintf(&request, "%s: %d\r\n", nitro.HeaderChainID, s.chainID)
	request.WriteString("\r\n")
	if _, err := io.WriteString(s.conn, request.String()); err != nil {
		return err
	}

	response, err := http.ReadResponse(s.reader, &http.Request{Method: http.MethodGet})
	if err != nil {
		return err
	}
	defer response.Body.Close()
	if response.StatusCode != http.StatusSwitchingProtocols {
		return fmt.Errorf("relay websocket handshake returned %s", response.Status)
	}
	if !strings.EqualFold(response.Header.Get("Upgrade"), "websocket") {
		return errors.New("relay websocket handshake missing upgrade header")
	}
	if !strings.Contains(strings.ToLower(response.Header.Get("Connection")), "upgrade") {
		return errors.New("relay websocket handshake missing connection upgrade header")
	}
	if got := response.Header.Get("Sec-WebSocket-Accept"); got != websocketAccept(key) {
		return errors.New("relay websocket handshake accept mismatch")
	}
	if got := response.Header.Get(nitro.HeaderFeedServerVersion); got != strconv.Itoa(nitro.FeedServerVersion) {
		return fmt.Errorf("relay feed server version mismatch: %q", got)
	}
	if got := response.Header.Get(nitro.HeaderChainID); got != strconv.FormatUint(s.chainID, 10) {
		return fmt.Errorf("relay feed chain id mismatch: %q", got)
	}
	return nil
}

func (s *RelaySource) readDataFrame(ctx context.Context) ([]byte, error) {
	for {
		if deadline, ok := ctx.Deadline(); ok {
			_ = s.conn.SetReadDeadline(deadline)
		} else {
			_ = s.conn.SetReadDeadline(time.Now().Add(s.timeout))
		}
		payload, opcode, err := s.readFrame()
		if err != nil {
			if netErr, ok := err.(net.Error); ok && netErr.Timeout() && ctx.Err() == nil {
				continue
			}
			return nil, err
		}
		switch opcode {
		case 0x1, 0x2:
			return payload, nil
		case 0x8:
			return nil, io.EOF
		case 0x9:
			if err := s.writeClientFrame(0xA, payload); err != nil {
				return nil, err
			}
		case 0xA:
			continue
		default:
			return nil, fmt.Errorf("unsupported websocket opcode 0x%x", opcode)
		}
	}
}

func (s *RelaySource) readFrame() ([]byte, byte, error) {
	header := make([]byte, 2)
	if _, err := io.ReadFull(s.reader, header); err != nil {
		return nil, 0, err
	}
	if header[0]&0x70 != 0 {
		return nil, 0, errors.New("relay websocket frame uses unsupported reserved bits")
	}
	if header[0]&0x80 == 0 {
		return nil, 0, errors.New("fragmented relay websocket frames are not supported")
	}
	opcode := header[0] & 0x0f
	switch opcode {
	case 0x1, 0x2, 0x8, 0x9, 0xA:
	default:
		return nil, 0, fmt.Errorf("unsupported websocket opcode 0x%x", opcode)
	}
	masked := header[1]&0x80 != 0
	length := uint64(header[1] & 0x7f)
	switch length {
	case 126:
		var extended [2]byte
		if _, err := io.ReadFull(s.reader, extended[:]); err != nil {
			return nil, 0, err
		}
		length = uint64(binary.BigEndian.Uint16(extended[:]))
	case 127:
		var extended [8]byte
		if _, err := io.ReadFull(s.reader, extended[:]); err != nil {
			return nil, 0, err
		}
		length = binary.BigEndian.Uint64(extended[:])
	}
	if length > 16*1024*1024 {
		return nil, 0, fmt.Errorf("relay websocket frame too large: %d", length)
	}
	if masked {
		return nil, 0, errors.New("relay websocket server frame must not be masked")
	}
	if opcode >= 0x8 && length > 125 {
		return nil, 0, errors.New("relay websocket control frame too large")
	}

	payload := make([]byte, length)
	if _, err := io.ReadFull(s.reader, payload); err != nil {
		return nil, 0, err
	}
	return payload, opcode, nil
}

func (s *RelaySource) writeClientFrame(opcode byte, payload []byte) error {
	if len(payload) > 125 {
		return errors.New("control frame payload too large")
	}
	var mask [4]byte
	if _, err := rand.Read(mask[:]); err != nil {
		return err
	}
	frame := []byte{0x80 | opcode, 0x80 | byte(len(payload))}
	frame = append(frame, mask[:]...)
	for i, b := range payload {
		frame = append(frame, b^mask[i%4])
	}
	_, err := s.conn.Write(frame)
	return err
}

func (s *RelaySource) closeConn() {
	if s.conn != nil {
		_ = s.conn.Close()
	}
	s.conn = nil
	s.reader = nil
}

func websocketKey() (string, error) {
	var b [16]byte
	if _, err := rand.Read(b[:]); err != nil {
		return "", err
	}
	return base64.StdEncoding.EncodeToString(b[:]), nil
}

func websocketAccept(key string) string {
	hash := sha1.Sum([]byte(key + websocketGUID))
	return base64.StdEncoding.EncodeToString(hash[:])
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
