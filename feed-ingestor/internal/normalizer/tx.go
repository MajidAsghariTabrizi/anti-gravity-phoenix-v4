package normalizer

import (
	"encoding/hex"
	"errors"
	"fmt"
	"strings"
	"time"
)

const SchemaVersion = "phoenix.v4.normalized_tx.v1"

type RelayTx struct {
	Hash                 string `json:"hash"`
	Type                 string `json:"type"`
	ChainID              uint64 `json:"chain_id"`
	From                 string `json:"from"`
	To                   string `json:"to"`
	Nonce                uint64 `json:"nonce"`
	Value                string `json:"value"`
	Calldata             string `json:"calldata"`
	GasLimit             string `json:"gas_limit"`
	MaxFeePerGas         string `json:"max_fee_per_gas"`
	MaxPriorityFeePerGas string `json:"max_priority_fee_per_gas"`
	RawTx                string `json:"raw_tx"`
}

type NormalizedTx struct {
	SchemaVersion        string `json:"schema_version"`
	Sequence             uint64 `json:"sequence"`
	TimestampUnixMS      uint64 `json:"timestamp_unix_ms"`
	TxHash               string `json:"tx_hash"`
	TxType               string `json:"tx_type"`
	ChainID              uint64 `json:"chain_id"`
	From                 string `json:"from"`
	To                   string `json:"to"`
	Nonce                uint64 `json:"nonce"`
	Value                string `json:"value"`
	Calldata             string `json:"calldata"`
	GasLimit             string `json:"gas_limit"`
	MaxFeePerGas         string `json:"max_fee_per_gas"`
	MaxPriorityFeePerGas string `json:"max_priority_fee_per_gas"`
	RawTx                []byte `json:"raw_tx"`
	IngestedAtUnixNS     int64  `json:"ingested_at_unix_ns"`
}

func Normalize(sequence uint64, timestampMS uint64, tx RelayTx, now time.Time) (NormalizedTx, error) {
	if tx.ChainID != 42161 {
		return NormalizedTx{}, errors.New("unsupported chain id")
	}
	if !isHexAddress(tx.From) {
		return NormalizedTx{}, errors.New("invalid from address")
	}
	if tx.To != "" && !isHexAddress(tx.To) {
		return NormalizedTx{}, errors.New("invalid to address")
	}
	if !isHex(tx.Hash, 32) {
		return NormalizedTx{}, errors.New("invalid tx hash")
	}
	raw, err := decodeOptionalHex(tx.RawTx)
	if err != nil {
		return NormalizedTx{}, err
	}
	return NormalizedTx{
		SchemaVersion:        SchemaVersion,
		Sequence:             sequence,
		TimestampUnixMS:      timestampMS,
		TxHash:               lowerHex(tx.Hash),
		TxType:               tx.Type,
		ChainID:              tx.ChainID,
		From:                 lowerHex(tx.From),
		To:                   lowerHex(tx.To),
		Nonce:                tx.Nonce,
		Value:                tx.Value,
		Calldata:             lowerHex(tx.Calldata),
		GasLimit:             tx.GasLimit,
		MaxFeePerGas:         tx.MaxFeePerGas,
		MaxPriorityFeePerGas: tx.MaxPriorityFeePerGas,
		RawTx:                raw,
		IngestedAtUnixNS:     now.UnixNano(),
	}, nil
}

func (tx NormalizedTx) DurableMessageID() string {
	return fmt.Sprintf("%d:%s", tx.Sequence, tx.TxHash)
}

func isHexAddress(v string) bool {
	return isHex(v, 20)
}

func isHex(v string, bytes int) bool {
	if len(v) != 2+bytes*2 || !strings.HasPrefix(v, "0x") {
		return false
	}
	_, err := hex.DecodeString(v[2:])
	return err == nil
}

func decodeOptionalHex(v string) ([]byte, error) {
	if v == "" || v == "0x" {
		return nil, nil
	}
	if !strings.HasPrefix(v, "0x") {
		return nil, errors.New("hex value missing 0x prefix")
	}
	return hex.DecodeString(v[2:])
}

func lowerHex(v string) string {
	if strings.HasPrefix(v, "0x") {
		return "0x" + strings.ToLower(v[2:])
	}
	return v
}
