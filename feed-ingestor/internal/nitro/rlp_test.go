package nitro

import (
	"bytes"
	"strings"
	"testing"
)

func TestParseRLPValidValues(t *testing.T) {
	longPayload := bytes.Repeat([]byte{0x42}, 56)
	longListPayload := bytes.Repeat([]byte{0x80}, 56)

	tests := []struct {
		name    string
		input   []byte
		payload []byte
		listLen int
		isList  bool
	}{
		{name: "single byte", input: []byte{0x7f}, payload: []byte{0x7f}},
		{name: "empty string", input: []byte{0x80}, payload: []byte{}},
		{name: "short string", input: []byte{0x83, 'c', 'a', 't'}, payload: []byte("cat")},
		{name: "long string", input: append([]byte{0xb8, 0x38}, longPayload...), payload: longPayload},
		{name: "empty list", input: []byte{0xc0}, isList: true, listLen: 0},
		{name: "short list", input: []byte{0xc8, 0x83, 'c', 'a', 't', 0x83, 'd', 'o', 'g'}, isList: true, listLen: 2},
		{name: "long list", input: append([]byte{0xf8, 0x38}, longListPayload...), isList: true, listLen: 56},
		{name: "nested list", input: []byte{0xc3, 0xc2, 0x01, 0x02}, isList: true, listLen: 1},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			value, rest, err := parseRLP(tt.input)
			if err != nil {
				t.Fatalf("parse rlp: %v", err)
			}
			if len(rest) != 0 {
				t.Fatalf("unexpected trailing bytes: %x", rest)
			}
			if value.isList != tt.isList {
				t.Fatalf("isList mismatch: got %v want %v", value.isList, tt.isList)
			}
			if tt.isList {
				if len(value.list) != tt.listLen {
					t.Fatalf("list length mismatch: got %d want %d", len(value.list), tt.listLen)
				}
				return
			}
			if !bytes.Equal(value.payload, tt.payload) {
				t.Fatalf("payload mismatch: got %x want %x", value.payload, tt.payload)
			}
		})
	}
}

func TestParseRLPRejectsMalformedOrNonCanonicalValues(t *testing.T) {
	tests := []struct {
		name  string
		input []byte
	}{
		{name: "empty input", input: nil},
		{name: "truncated short string", input: []byte{0x82, 0x01}},
		{name: "truncated long string length", input: []byte{0xb8}},
		{name: "truncated long string payload", input: []byte{0xb8, 0x38, 0x01}},
		{name: "truncated short list", input: []byte{0xc2, 0x01}},
		{name: "truncated long list length", input: []byte{0xf8}},
		{name: "truncated long list payload", input: []byte{0xf8, 0x38, 0x80}},
		{name: "non-canonical single byte string", input: []byte{0x81, 0x7f}},
		{name: "non-canonical long string length", input: append([]byte{0xb8, 0x37}, bytes.Repeat([]byte{0x42}, 55)...)},
		{name: "non-canonical long string length leading zero", input: []byte{0xb9, 0x00, 0x38}},
		{name: "non-canonical long list length", input: append([]byte{0xf8, 0x37}, bytes.Repeat([]byte{0x80}, 55)...)},
		{name: "non-canonical long list length leading zero", input: []byte{0xf9, 0x00, 0x38}},
		{name: "string length overflow", input: []byte{0xbf, 0x80, 0, 0, 0, 0, 0, 0, 0}},
		{name: "list length overflow", input: []byte{0xff, 0x80, 0, 0, 0, 0, 0, 0, 0}},
		{name: "malformed nested value", input: []byte{0xc2, 0x82, 0x01}},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			if _, _, err := parseRLP(tt.input); err == nil {
				t.Fatal("expected parse failure")
			}
		})
	}
}

func TestParseRLPTrailingBytesRemainVisible(t *testing.T) {
	value, rest, err := parseRLP([]byte{0x01, 0x02})
	if err != nil {
		t.Fatal(err)
	}
	if !bytes.Equal(value.payload, []byte{0x01}) || !bytes.Equal(rest, []byte{0x02}) {
		t.Fatalf("unexpected parse result value=%x rest=%x", value.payload, rest)
	}
}

func TestRLPIntegersRejectNonCanonicalEncodings(t *testing.T) {
	tests := []struct {
		name string
		raw  []byte
		err  string
	}{
		{name: "zero as single byte", raw: []byte{0x00}, err: "non-canonical rlp uint"},
		{name: "leading zero uint", raw: []byte{0x82, 0x00, 0x01}, err: "non-canonical rlp uint"},
		{name: "uint overflow", raw: []byte{0x89, 1, 2, 3, 4, 5, 6, 7, 8, 9}, err: "rlp uint overflow"},
	}

	for _, tt := range tests {
		t.Run(tt.name, func(t *testing.T) {
			value, rest, err := parseRLP(tt.raw)
			if err != nil {
				t.Fatal(err)
			}
			if len(rest) != 0 {
				t.Fatalf("unexpected rest: %x", rest)
			}
			if _, err := rlpUint(value); err == nil || !strings.Contains(err.Error(), tt.err) {
				t.Fatalf("expected %q error, got %v", tt.err, err)
			}
		})
	}
}

func TestRLPBigIntRejectsLeadingZero(t *testing.T) {
	value, _, err := parseRLP([]byte{0x82, 0x00, 0x01})
	if err != nil {
		t.Fatal(err)
	}
	if _, err := rlpBig(value); err == nil || !strings.Contains(err.Error(), "non-canonical rlp big int") {
		t.Fatalf("expected non-canonical big int error, got %v", err)
	}
}
