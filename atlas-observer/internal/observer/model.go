package observer

import (
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"math/big"
	"regexp"
	"strings"
	"time"
)

var (
	addressPattern = regexp.MustCompile(`^0x[0-9a-fA-F]{40}$`)
	hashPattern    = regexp.MustCompile(`^0x[0-9a-fA-F]{64}$`)
	ErrOtherChain  = errors.New("notification is for another chain")
)

type notificationEnvelope struct {
	JSONRPC string `json:"jsonrpc"`
	Method  string `json:"method"`
	Params  struct {
		Subscription string              `json:"subscription"`
		Result       AuctionNotification `json:"result"`
	} `json:"params"`
}

type AuctionNotification struct {
	AuctionID            string               `json:"auction_id"`
	PartialUserOperation PartialUserOperation `json:"partial_user_operation"`
}

type PartialUserOperation struct {
	ChainID      string                     `json:"chainId"`
	UserOpHash   string                     `json:"userOpHash"`
	To           string                     `json:"to"`
	Gas          string                     `json:"gas"`
	MaxFeePerGas string                     `json:"maxFeePerGas"`
	Deadline     string                     `json:"deadline"`
	Dapp         string                     `json:"dapp"`
	Control      string                     `json:"control"`
	Hints        map[string]json.RawMessage `json:"hints,omitempty"`
	Value        *string                    `json:"value,omitempty"`
	Data         *string                    `json:"data,omitempty"`
	From         *string                    `json:"from,omitempty"`
}

type OracleUpdate struct {
	Aggregator  string          `json:"aggregator"`
	Asset       *string         `json:"asset"`
	FeedType    string          `json:"feed_type"`
	MedianPrice *string         `json:"median_price"`
	RawReport   json.RawMessage `json:"raw_report"`
}

type LedgerRecord struct {
	Schema                       string               `json:"schema"`
	ObservedAt                   time.Time            `json:"observed_at"`
	AuctionID                    string               `json:"auction_id"`
	SubscriptionID               string               `json:"subscription_id"`
	NotificationSHA256           string               `json:"notification_sha256"`
	ChainID                      uint64               `json:"chain_id"`
	Atlas                        string               `json:"atlas"`
	DappControl                  string               `json:"dapp_control"`
	Dapp                         string               `json:"dapp"`
	UserOpHash                   string               `json:"user_operation_hash"`
	AuctionDeadlineBlock         string               `json:"auction_deadline_block"`
	OracleGasPriceWei            string               `json:"oracle_gas_price_wei"`
	SolverGasLimit               uint64               `json:"solver_gas_limit"`
	ParallelEligible             bool                 `json:"parallel_eligible"`
	ParallelAuctionIdentity      string               `json:"parallel_auction_identity"`
	RelevantAaveAuction          bool                 `json:"relevant_aave_auction"`
	OracleUpdate                 *OracleUpdate        `json:"oracle_update"`
	PartialUserOperation         PartialUserOperation `json:"partial_user_operation"`
	RawNotification              json.RawMessage      `json:"raw_notification"`
	SuccessfulOnchainTransaction *string              `json:"successful_onchain_auction_transaction"`
	AttemptedSolvers             []string             `json:"attempted_solvers"`
	WinningSolver                *string              `json:"winning_solver"`
	Bids                         any                  `json:"bids"`
	GasUsed                      *string              `json:"gas_used"`
	FailedOperations             any                  `json:"failed_operations"`
	FallbackOccurred             *bool                `json:"fallback_occurred"`
	PublicLiquidationResult      any                  `json:"public_liquidation_result"`
	CollateralAsset              *string              `json:"collateral_asset"`
	DebtAsset                    *string              `json:"debt_asset"`
	EstimatedGrossBonus          *string              `json:"estimated_gross_liquidation_bonus"`
	EstimatedWinnerResidualPnL   *string              `json:"estimated_winner_residual_pnl"`
}

type InvalidRecord struct {
	ObservedAt         time.Time `json:"observed_at"`
	Reason             string    `json:"reason"`
	AuctionID          string    `json:"auction_id,omitempty"`
	NotificationSHA256 string    `json:"notification_sha256"`
}

func DecodeAndValidateNotification(raw []byte, observedAt time.Time) (*LedgerRecord, error) {
	var envelope notificationEnvelope
	if err := json.Unmarshal(raw, &envelope); err != nil {
		return nil, fmt.Errorf("decode notification: %w", err)
	}
	if envelope.JSONRPC != "2.0" || envelope.Method != "solver_subscription" {
		return nil, errors.New("unexpected JSON-RPC notification")
	}
	if envelope.Params.Subscription == "" || envelope.Params.Result.AuctionID == "" {
		return nil, errors.New("missing subscription or auction identity")
	}

	uop := envelope.Params.Result.PartialUserOperation
	chainID, err := parseHexUint(uop.ChainID)
	if err != nil {
		return nil, errors.New("invalid chain ID")
	}
	if chainID.Uint64() != ArbitrumChainID {
		return nil, ErrOtherChain
	}
	if !strings.EqualFold(uop.To, ArbitrumAtlas) {
		return nil, errors.New("unexpected Atlas address")
	}
	if !strings.EqualFold(uop.Control, ArbitrumDappControl) {
		return nil, errors.New("unexpected DappControl address")
	}
	if !addressPattern.MatchString(uop.Dapp) || !hashPattern.MatchString(uop.UserOpHash) {
		return nil, errors.New("invalid dapp or user operation hash")
	}
	if _, err := parseHexUint(uop.Gas); err != nil {
		return nil, errors.New("invalid user operation gas")
	}
	gasPrice, err := parseHexUint(uop.MaxFeePerGas)
	if err != nil {
		return nil, errors.New("invalid oracle gas price")
	}
	deadline, err := parseHexUint(uop.Deadline)
	if err != nil {
		return nil, errors.New("invalid auction deadline")
	}
	hasHints := len(uop.Hints) > 0
	if hasHints {
		if uop.Value != nil || uop.Data != nil {
			return nil, errors.New("hinted partial user operation contains value or data")
		}
	} else if uop.Value == nil || uop.Data == nil || uop.From == nil {
		return nil, errors.New("non-hinted partial user operation is incomplete")
	}

	oracleUpdate, relevant, parallel, err := classifyOracleUpdate(uop.Hints)
	if err != nil {
		return nil, err
	}
	parallelIdentity, err := buildParallelIdentity(uop, oracleUpdate)
	if err != nil {
		return nil, err
	}
	canonicalNotification, err := json.Marshal(envelope)
	if err != nil {
		return nil, fmt.Errorf("encode canonical notification: %w", err)
	}
	notificationDigest := sha256.Sum256(canonicalNotification)

	return &LedgerRecord{
		Schema:                  Schema,
		ObservedAt:              observedAt.UTC(),
		AuctionID:               envelope.Params.Result.AuctionID,
		SubscriptionID:          envelope.Params.Subscription,
		NotificationSHA256:      hex.EncodeToString(notificationDigest[:]),
		ChainID:                 ArbitrumChainID,
		Atlas:                   ArbitrumAtlas,
		DappControl:             ArbitrumDappControl,
		Dapp:                    uop.Dapp,
		UserOpHash:              uop.UserOpHash,
		AuctionDeadlineBlock:    deadline.String(),
		OracleGasPriceWei:       gasPrice.String(),
		SolverGasLimit:          ObservedSolverGasLimit,
		ParallelEligible:        parallel,
		ParallelAuctionIdentity: parallelIdentity,
		RelevantAaveAuction:     relevant,
		OracleUpdate:            oracleUpdate,
		PartialUserOperation:    uop,
		RawNotification:         append(json.RawMessage(nil), raw...),
	}, nil
}

func classifyOracleUpdate(hints map[string]json.RawMessage) (*OracleUpdate, bool, bool, error) {
	if len(hints) == 0 {
		return nil, false, false, nil
	}
	var aggregator string
	if raw, ok := hints["aggregator"]; ok {
		if err := json.Unmarshal(raw, &aggregator); err != nil || !addressPattern.MatchString(aggregator) {
			return nil, false, false, errors.New("invalid aggregator hint")
		}
	} else {
		return nil, false, false, errors.New("missing aggregator hint")
	}
	var medianPrice *string
	if raw, ok := hints["medianPrice"]; ok {
		var value string
		if err := json.Unmarshal(raw, &value); err != nil {
			return nil, false, false, errors.New("invalid median price hint")
		}
		if _, err := parseHexUint(value); err != nil {
			return nil, false, false, errors.New("invalid median price hint")
		}
		medianPrice = &value
	}
	rawReport := json.RawMessage(nil)
	if raw, ok := hints["rawReport"]; ok {
		rawReport = append(json.RawMessage(nil), raw...)
	}
	feed, relevant := aaveSVRFeeds[strings.ToLower(aggregator)]
	update := &OracleUpdate{
		Aggregator:  aggregator,
		FeedType:    "unknown",
		MedianPrice: medianPrice,
		RawReport:   rawReport,
	}
	if relevant {
		asset := feed.Asset
		update.Asset = &asset
		update.FeedType = "Aave-SVR"
		update.Aggregator = feed.Aggregator
	}
	return update, relevant, relevant && feed.Parallel, nil
}

func buildParallelIdentity(uop PartialUserOperation, update *OracleUpdate) (string, error) {
	payload := struct {
		ChainID    string          `json:"chain_id"`
		Atlas      string          `json:"atlas"`
		Dapp       string          `json:"dapp"`
		Control    string          `json:"control"`
		Aggregator string          `json:"aggregator,omitempty"`
		Median     *string         `json:"median_price,omitempty"`
		RawReport  json.RawMessage `json:"raw_report,omitempty"`
		Data       *string         `json:"data,omitempty"`
	}{
		ChainID: strings.ToLower(uop.ChainID),
		Atlas:   strings.ToLower(uop.To),
		Dapp:    strings.ToLower(uop.Dapp),
		Control: strings.ToLower(uop.Control),
		Data:    uop.Data,
	}
	if update != nil {
		payload.Aggregator = strings.ToLower(update.Aggregator)
		payload.Median = update.MedianPrice
		payload.RawReport = update.RawReport
	}
	encoded, err := json.Marshal(payload)
	if err != nil {
		return "", fmt.Errorf("encode parallel identity: %w", err)
	}
	digest := sha256.Sum256(encoded)
	return hex.EncodeToString(digest[:]), nil
}

func parseHexUint(value string) (*big.Int, error) {
	if len(value) < 3 || !strings.HasPrefix(strings.ToLower(value), "0x") {
		return nil, errors.New("not a hexadecimal quantity")
	}
	n := new(big.Int)
	if _, ok := n.SetString(value[2:], 16); !ok || n.Sign() < 0 {
		return nil, errors.New("invalid hexadecimal quantity")
	}
	return n, nil
}
