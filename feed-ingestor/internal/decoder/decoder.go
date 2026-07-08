package decoder

import (
	"encoding/json"
	"errors"
	"time"

	"anti-gravity-phoenix-v4/feed-ingestor/internal/normalizer"
)

type RelayFrame struct {
	Sequence        uint64                 `json:"sequence"`
	TimestampUnixMS uint64                 `json:"timestamp_unix_ms"`
	Transactions    []normalizer.RelayTx   `json:"transactions"`
	Metadata        map[string]interface{} `json:"metadata,omitempty"`
}

type DecodeResult struct {
	Transactions []normalizer.NormalizedTx
	Duplicate    bool
	Gap          bool
	GapFrom      uint64
	GapTo        uint64
}

type OrderedDecoder struct {
	lastSequence uint64
	haveLast     bool
	seen         map[uint64]struct{}
	now          func() time.Time
}

func NewOrderedDecoder(now func() time.Time) *OrderedDecoder {
	if now == nil {
		now = time.Now
	}
	return &OrderedDecoder{
		seen: make(map[uint64]struct{}),
		now:  now,
	}
}

func (d *OrderedDecoder) DecodeJSONFrame(raw []byte) (DecodeResult, error) {
	var frame RelayFrame
	if err := json.Unmarshal(raw, &frame); err != nil {
		return DecodeResult{}, err
	}
	if frame.Sequence == 0 {
		return DecodeResult{}, errors.New("missing sequence")
	}
	if _, ok := d.seen[frame.Sequence]; ok {
		return DecodeResult{Duplicate: true}, nil
	}

	result := DecodeResult{}
	if d.haveLast && frame.Sequence > d.lastSequence+1 {
		result.Gap = true
		result.GapFrom = d.lastSequence + 1
		result.GapTo = frame.Sequence - 1
	}

	normalized := make([]normalizer.NormalizedTx, 0, len(frame.Transactions))
	for _, tx := range frame.Transactions {
		n, err := normalizer.Normalize(frame.Sequence, frame.TimestampUnixMS, tx, d.now())
		if err != nil {
			return DecodeResult{}, err
		}
		normalized = append(normalized, n)
	}

	d.seen[frame.Sequence] = struct{}{}
	d.lastSequence = frame.Sequence
	d.haveLast = true
	result.Transactions = normalized
	return result, nil
}
