package feed

import (
	"bufio"
	"bytes"
	"context"
	"encoding/binary"
	"net"
	"strings"
	"testing"
	"time"
)

func TestNewRelaySourceRejectsUnsupportedSchemes(t *testing.T) {
	if _, err := NewRelaySource("wss://nitro-feed-relay:9642/feed", 42161, nil, time.Second); err == nil {
		t.Fatal("expected wss relay URL to be rejected")
	}
	if _, err := NewRelaySource("ws://nitro-feed-relay:9642/feed", 42161, nil, time.Second); err != nil {
		t.Fatalf("expected ws relay URL to be accepted: %v", err)
	}
}

func TestReadFrameAcceptsUnmaskedTextAndBinaryFrames(t *testing.T) {
	tests := []struct {
		name    string
		opcode  byte
		payload []byte
	}{
		{name: "text", opcode: 0x1, payload: []byte(`{"version":1}`)},
		{name: "binary", opcode: 0x2, payload: []byte{0x01, 0x02, 0x03}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			source := sourceWithFrame(serverFrame(tt.opcode, tt.payload))
			payload, opcode, err := source.readFrame()
			if err != nil {
				t.Fatalf("read frame: %v", err)
			}
			if opcode != tt.opcode || !bytes.Equal(payload, tt.payload) {
				t.Fatalf("unexpected frame opcode=%x payload=%x", opcode, payload)
			}
		})
	}
}

func TestReadFrameRejectsUnsupportedServerFrames(t *testing.T) {
	tests := []struct {
		name string
		raw  []byte
		want string
	}{
		{name: "reserved bits", raw: []byte{0xc1, 0x00}, want: "reserved bits"},
		{name: "fragmented", raw: []byte{0x01, 0x00}, want: "fragmented"},
		{name: "masked server frame", raw: []byte{0x81, 0x81, 0, 0, 0, 0, 'x'}, want: "must not be masked"},
		{name: "oversized data frame", raw: oversizedFrameHeader(16*1024*1024 + 1), want: "too large"},
		{name: "oversized control frame", raw: append([]byte{0x89, 126, 0, 126}, bytes.Repeat([]byte{'x'}, 126)...), want: "control frame too large"},
		{name: "unexpected opcode", raw: []byte{0x83, 0x00}, want: "unsupported websocket opcode"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			source := sourceWithFrame(tt.raw)
			if _, _, err := source.readFrame(); err == nil || !strings.Contains(err.Error(), tt.want) {
				t.Fatalf("expected %q error, got %v", tt.want, err)
			}
		})
	}
}

func TestReadDataFrameRespondsToPingAndReturnsNextMessage(t *testing.T) {
	server, client := net.Pipe()
	defer server.Close()
	defer client.Close()

	source := &RelaySource{
		conn:    client,
		reader:  bufio.NewReader(client),
		timeout: time.Second,
	}

	pongDone := make(chan error, 1)
	go func() {
		_, _ = server.Write(serverFrame(0x9, []byte("hi")))
		pongHeader := make([]byte, 6)
		if _, err := server.Read(pongHeader); err != nil {
			pongDone <- err
			return
		}
		if pongHeader[0] != 0x8a || pongHeader[1] != 0x82 {
			pongDone <- &unexpectedFrameError{header: pongHeader[:2]}
			return
		}
		maskedPayload := make([]byte, 2)
		if _, err := server.Read(maskedPayload); err != nil {
			pongDone <- err
			return
		}
		for i := range maskedPayload {
			maskedPayload[i] ^= pongHeader[2+i%4]
		}
		if string(maskedPayload) != "hi" {
			pongDone <- &unexpectedPayloadError{payload: string(maskedPayload)}
			return
		}
		pongDone <- nil
		_, _ = server.Write(serverFrame(0x1, []byte("next")))
	}()

	payload, err := source.readDataFrame(context.Background())
	if err != nil {
		t.Fatalf("read data frame: %v", err)
	}
	if string(payload) != "next" {
		t.Fatalf("unexpected payload: %q", payload)
	}
	if err := <-pongDone; err != nil {
		t.Fatal(err)
	}
}

type unexpectedFrameError struct {
	header []byte
}

func (e *unexpectedFrameError) Error() string {
	return "unexpected pong header: " + string(e.header)
}

type unexpectedPayloadError struct {
	payload string
}

func (e *unexpectedPayloadError) Error() string {
	return "unexpected pong payload: " + e.payload
}

func sourceWithFrame(raw []byte) *RelaySource {
	return &RelaySource{reader: bufio.NewReader(bytes.NewReader(raw))}
}

func serverFrame(opcode byte, payload []byte) []byte {
	frame := []byte{0x80 | opcode}
	switch {
	case len(payload) <= 125:
		frame = append(frame, byte(len(payload)))
	case len(payload) <= 0xffff:
		frame = append(frame, 126, byte(len(payload)>>8), byte(len(payload)))
	default:
		frame = append(frame, 127)
		var length [8]byte
		binary.BigEndian.PutUint64(length[:], uint64(len(payload)))
		frame = append(frame, length[:]...)
	}
	return append(frame, payload...)
}

func oversizedFrameHeader(length uint64) []byte {
	frame := []byte{0x82, 127}
	var encoded [8]byte
	binary.BigEndian.PutUint64(encoded[:], length)
	return append(frame, encoded[:]...)
}
