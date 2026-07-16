package nitro

import (
	"bytes"
	"encoding/json"
	"math/big"
	"os"
	"strings"
	"testing"
)

const (
	numericBaseFeeFixtureValue = "9007199254740993"
	maxUint256Decimal          = "115792089237316195423570985008687907853269984665640564039457584007913129639935"
)

func TestBaseFeeL1CanonicalNumbersPreserveExactUint256(t *testing.T) {
	tests := []struct {
		name  string
		value string
	}{
		{name: "zero", value: "0"},
		{name: "larger than JavaScript safe integer", value: numericBaseFeeFixtureValue},
		{name: "maximum uint256", value: maxUint256Decimal},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var header L1IncomingMessageHeader
			if err := json.Unmarshal([]byte(`{"baseFeeL1":`+test.value+`}`), &header); err != nil {
				t.Fatalf("decode canonical baseFeeL1: %v", err)
			}
			want, ok := new(big.Int).SetString(test.value, 10)
			if !ok {
				t.Fatal("invalid test integer")
			}
			if header.BaseFeeL1 == nil || header.BaseFeeL1.value.Cmp(want) != 0 {
				t.Fatalf("baseFeeL1 precision changed: got %v want %s", header.BaseFeeL1, test.value)
			}
			encoded, err := json.Marshal(header.BaseFeeL1)
			if err != nil {
				t.Fatalf("marshal canonical baseFeeL1: %v", err)
			}
			if string(encoded) != test.value {
				t.Fatalf("baseFeeL1 changed on round trip: got %s want %s", encoded, test.value)
			}
		})
	}
}

func TestBaseFeeL1NullAndOmittedRemainNil(t *testing.T) {
	for _, raw := range []string{`{"baseFeeL1":null}`, `{}`} {
		var header L1IncomingMessageHeader
		if err := json.Unmarshal([]byte(raw), &header); err != nil {
			t.Fatalf("decode official nil representation %s: %v", raw, err)
		}
		if header.BaseFeeL1 != nil {
			t.Fatalf("nil baseFeeL1 became non-nil for %s", raw)
		}
	}
}

func TestBaseFeeL1RejectsNonCanonicalOrOutOfRangeValues(t *testing.T) {
	tests := []struct {
		name  string
		value string
	}{
		{name: "decimal string", value: `"123"`},
		{name: "hexadecimal string", value: `"0x7b"`},
		{name: "overflow", value: "115792089237316195423570985008687907853269984665640564039457584007913129639936"},
		{name: "fraction", value: "1.5"},
		{name: "negative", value: "-1"},
		{name: "boolean", value: "true"},
		{name: "object", value: `{}`},
		{name: "array", value: `[]`},
		{name: "empty string", value: `""`},
		{name: "exponent", value: "1e3"},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var header L1IncomingMessageHeader
			err := json.Unmarshal([]byte(`{"baseFeeL1":`+test.value+`}`), &header)
			if err == nil {
				t.Fatalf("accepted invalid baseFeeL1 %s", test.name)
			}
			if !strings.Contains(err.Error(), "baseFeeL1") || strings.Contains(err.Error(), test.value) {
				t.Fatalf("baseFeeL1 error was not sanitized: %v", err)
			}
		})
	}
}

func TestNumericBaseFeeL1RealShapeFixtureNormalizesTransaction(t *testing.T) {
	raw := readNumericBaseFeeFixture(t)
	frames, report, err := DecodeBroadcast(raw)
	if err != nil {
		t.Fatalf("decode numeric baseFeeL1 fixture: %v", err)
	}
	if len(report.Malformed) != 0 || len(report.Unsupported) != 0 {
		t.Fatalf("numeric baseFeeL1 produced issues: %+v", report)
	}
	if len(frames) != 1 || len(frames[0].Transactions) != 1 {
		t.Fatalf("numeric baseFeeL1 did not normalize one transaction: %+v", frames)
	}
	if frames[0].Sequence != 460530858 || frames[0].Transactions[0].ChainID != ArbitrumOneChainID {
		t.Fatalf("numeric baseFeeL1 changed transaction identity: %+v", frames[0])
	}

	var message BroadcastMessage
	if err := json.Unmarshal(raw, &message); err != nil {
		t.Fatal(err)
	}
	got := &message.Messages[0].Message.Message.Header.BaseFeeL1.value
	want, _ := new(big.Int).SetString(numericBaseFeeFixtureValue, 10)
	if got.Cmp(want) != 0 {
		t.Fatalf("fixture baseFeeL1 lost precision: got %s want %s", got, want)
	}
}

func TestRealShapeFixtureMatchesOfficialNilSemantics(t *testing.T) {
	raw := readNumericBaseFeeFixture(t)
	canonical := []byte(`"baseFeeL1": 9007199254740993`)
	tests := []struct {
		name        string
		replacement []byte
		removeComma bool
	}{
		{name: "null", replacement: []byte(`"baseFeeL1": null`)},
		{name: "omitted", removeComma: true},
	}

	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			target := canonical
			if test.removeComma {
				target = append(append([]byte{}, canonical...), ',')
			}
			candidate := bytes.Replace(raw, target, test.replacement, 1)
			frames, report, err := DecodeBroadcast(candidate)
			if err != nil {
				t.Fatalf("decode %s baseFeeL1 fixture: %v", test.name, err)
			}
			if len(frames) != 1 || len(frames[0].Transactions) != 1 || len(report.Malformed) != 0 {
				t.Fatalf("official nil semantics changed fixture output: frames=%+v report=%+v", frames, report)
			}
		})
	}
}

func TestMalformedBaseFeeL1FixtureReturnsPayloadFreeError(t *testing.T) {
	raw := bytes.Replace(readNumericBaseFeeFixture(t), []byte(numericBaseFeeFixtureValue), []byte("1.5"), 1)
	_, _, err := DecodeBroadcast(raw)
	if err == nil {
		t.Fatal("accepted fractional baseFeeL1 fixture")
	}
	for _, forbidden := range []string{"l2Msg", "BAL4ZoK", "raw_tx", "signatureV2"} {
		if strings.Contains(err.Error(), forbidden) {
			t.Fatalf("decoder error exposed fixture data %q: %v", forbidden, err)
		}
	}
}

func readNumericBaseFeeFixture(t *testing.T) []byte {
	t.Helper()
	raw, err := os.ReadFile("testdata/numeric_base_fee_l1.json")
	if err != nil {
		t.Fatalf("read numeric baseFeeL1 fixture: %v", err)
	}
	return raw
}
