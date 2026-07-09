package nitro

import (
	"encoding/binary"
	"errors"
	"fmt"
	"math/big"
)

type rlpValue struct {
	payload []byte
	list    []rlpValue
	isList  bool
}

func parseRLP(input []byte) (rlpValue, []byte, error) {
	if len(input) == 0 {
		return rlpValue{}, nil, errors.New("empty rlp input")
	}
	prefix := input[0]
	switch {
	case prefix <= 0x7f:
		return rlpValue{payload: []byte{prefix}}, input[1:], nil
	case prefix <= 0xb7:
		length := int(prefix - 0x80)
		return parseRLPString(input[1:], length)
	case prefix <= 0xbf:
		lengthOfLength := int(prefix - 0xb7)
		if len(input) < 1+lengthOfLength {
			return rlpValue{}, nil, errors.New("short rlp long string length")
		}
		length, err := decodeLength(input[1 : 1+lengthOfLength])
		if err != nil {
			return rlpValue{}, nil, err
		}
		if length <= 55 {
			return rlpValue{}, nil, errors.New("non-canonical rlp long string length")
		}
		return parseRLPString(input[1+lengthOfLength:], length)
	case prefix <= 0xf7:
		length := int(prefix - 0xc0)
		return parseRLPList(input[1:], length)
	default:
		lengthOfLength := int(prefix - 0xf7)
		if len(input) < 1+lengthOfLength {
			return rlpValue{}, nil, errors.New("short rlp long list length")
		}
		length, err := decodeLength(input[1 : 1+lengthOfLength])
		if err != nil {
			return rlpValue{}, nil, err
		}
		if length <= 55 {
			return rlpValue{}, nil, errors.New("non-canonical rlp long list length")
		}
		return parseRLPList(input[1+lengthOfLength:], length)
	}
}

func parseRLPString(input []byte, length int) (rlpValue, []byte, error) {
	if length < 0 || len(input) < length {
		return rlpValue{}, nil, errors.New("short rlp string")
	}
	if length == 1 && input[0] <= 0x7f {
		return rlpValue{}, nil, errors.New("non-canonical rlp single byte string")
	}
	return rlpValue{payload: input[:length]}, input[length:], nil
}

func parseRLPList(input []byte, length int) (rlpValue, []byte, error) {
	if length < 0 || len(input) < length {
		return rlpValue{}, nil, errors.New("short rlp list")
	}
	remaining := input[:length]
	values := make([]rlpValue, 0)
	for len(remaining) > 0 {
		value, rest, err := parseRLP(remaining)
		if err != nil {
			return rlpValue{}, nil, err
		}
		values = append(values, value)
		remaining = rest
	}
	return rlpValue{list: values, isList: true}, input[length:], nil
}

func decodeLength(input []byte) (int, error) {
	if len(input) == 0 {
		return 0, errors.New("empty rlp length")
	}
	if len(input) > 8 {
		return 0, errors.New("rlp length overflow")
	}
	if input[0] == 0 {
		return 0, errors.New("non-canonical rlp length")
	}
	var padded [8]byte
	copy(padded[8-len(input):], input)
	length := binary.BigEndian.Uint64(padded[:])
	maxInt := uint64(^uint(0) >> 1)
	if length > maxInt {
		return 0, errors.New("rlp length overflow")
	}
	return int(length), nil
}

func rlpUint(value rlpValue) (uint64, error) {
	if value.isList {
		return 0, errors.New("rlp uint is list")
	}
	if len(value.payload) == 0 {
		return 0, nil
	}
	if value.payload[0] == 0 {
		return 0, errors.New("non-canonical rlp uint")
	}
	if len(value.payload) > 8 {
		return 0, errors.New("rlp uint overflow")
	}
	var padded [8]byte
	copy(padded[8-len(value.payload):], value.payload)
	return binary.BigEndian.Uint64(padded[:]), nil
}

func rlpBig(value rlpValue) (*big.Int, error) {
	if value.isList {
		return nil, errors.New("rlp big int is list")
	}
	if len(value.payload) == 0 {
		return big.NewInt(0), nil
	}
	if value.payload[0] == 0 {
		return nil, errors.New("non-canonical rlp big int")
	}
	return new(big.Int).SetBytes(value.payload), nil
}

func rlpAddress(value rlpValue) (string, error) {
	if value.isList {
		return "", errors.New("rlp address is list")
	}
	if len(value.payload) != 20 {
		return "", fmt.Errorf("rlp address has %d bytes", len(value.payload))
	}
	return "0x" + hexLower(value.payload), nil
}

func rlpOptionalAddress(value rlpValue) (string, error) {
	if value.isList {
		return "", errors.New("rlp optional address is list")
	}
	if len(value.payload) == 0 {
		return "", nil
	}
	return rlpAddress(value)
}
