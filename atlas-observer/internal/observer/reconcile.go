package observer

import (
	"bufio"
	"bytes"
	"crypto/sha256"
	"encoding/hex"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"math/big"
	"os"
	"path/filepath"
	"sort"
	"strings"
	"time"

	"github.com/ethereum/go-ethereum/accounts/abi"
	"github.com/ethereum/go-ethereum/common"
)

const atlasEvidenceABI = `[
  {"type":"function","name":"metacall","stateMutability":"payable","inputs":[
    {"name":"userOp","type":"tuple","components":[
      {"name":"from","type":"address"},{"name":"to","type":"address"},{"name":"value","type":"uint256"},
      {"name":"gas","type":"uint256"},{"name":"maxFeePerGas","type":"uint256"},{"name":"nonce","type":"uint256"},
      {"name":"deadline","type":"uint256"},{"name":"dapp","type":"address"},{"name":"control","type":"address"},
      {"name":"callConfig","type":"uint32"},{"name":"dappGasLimit","type":"uint32"},{"name":"solverGasLimit","type":"uint32"},
      {"name":"bundlerSurchargeRate","type":"uint24"},{"name":"sessionKey","type":"address"},
      {"name":"data","type":"bytes"},{"name":"signature","type":"bytes"}
    ]},
    {"name":"solverOps","type":"tuple[]","components":[
      {"name":"from","type":"address"},{"name":"to","type":"address"},{"name":"value","type":"uint256"},
      {"name":"gas","type":"uint256"},{"name":"maxFeePerGas","type":"uint256"},{"name":"deadline","type":"uint256"},
      {"name":"solver","type":"address"},{"name":"control","type":"address"},{"name":"userOpHash","type":"bytes32"},
      {"name":"bidToken","type":"address"},{"name":"bidAmount","type":"uint256"},{"name":"data","type":"bytes"},
      {"name":"signature","type":"bytes"}
    ]},
    {"name":"dAppOp","type":"tuple","components":[
      {"name":"from","type":"address"},{"name":"to","type":"address"},{"name":"nonce","type":"uint256"},
      {"name":"deadline","type":"uint256"},{"name":"control","type":"address"},{"name":"bundler","type":"address"},
      {"name":"userOpHash","type":"bytes32"},{"name":"callChainHash","type":"bytes32"},{"name":"signature","type":"bytes"}
    ]},
    {"name":"gasRefundBeneficiary","type":"address"}
  ],"outputs":[{"name":"auctionWon","type":"bool"}]},
  {"type":"event","name":"SolverTxResult","anonymous":false,"inputs":[
    {"name":"solverTo","type":"address","indexed":true},{"name":"solverFrom","type":"address","indexed":true},
    {"name":"dAppControl","type":"address","indexed":true},{"name":"bidToken","type":"address","indexed":false},
    {"name":"bidAmount","type":"uint256","indexed":false},{"name":"executed","type":"bool","indexed":false},
    {"name":"success","type":"bool","indexed":false},{"name":"result","type":"uint256","indexed":false}
  ]},
  {"type":"event","name":"MetacallResult","anonymous":false,"inputs":[
    {"name":"bundler","type":"address","indexed":true},{"name":"user","type":"address","indexed":true},
    {"name":"solverSuccessful","type":"bool","indexed":false},{"name":"ethPaidToBundler","type":"uint256","indexed":false},
    {"name":"netGasSurcharge","type":"uint256","indexed":false}
  ]},
  {"type":"event","name":"LiquidationCall","anonymous":false,"inputs":[
    {"name":"collateralAsset","type":"address","indexed":true},{"name":"debtAsset","type":"address","indexed":true},
    {"name":"user","type":"address","indexed":true},{"name":"debtToCover","type":"uint256","indexed":false},
    {"name":"liquidatedCollateralAmount","type":"uint256","indexed":false},{"name":"liquidator","type":"address","indexed":false},
    {"name":"receiveAToken","type":"bool","indexed":false}
  ]}
]`

type RPCTranscript struct {
	Schema       string              `json:"schema"`
	ChainID      string              `json:"chain_id"`
	Atlas        string              `json:"atlas"`
	FromBlock    string              `json:"from_block"`
	ToBlock      string              `json:"to_block"`
	LatestBlock  string              `json:"latest_block"`
	Transactions []PublicTransaction `json:"transactions"`
}

type PublicTransaction struct {
	Hash             string        `json:"hash"`
	To               string        `json:"to"`
	Input            string        `json:"input"`
	BlockNumber      string        `json:"blockNumber"`
	TransactionIndex string        `json:"transactionIndex"`
	Receipt          PublicReceipt `json:"receipt"`
}

type PublicReceipt struct {
	TransactionHash   string      `json:"transactionHash"`
	BlockNumber       string      `json:"blockNumber"`
	Status            string      `json:"status"`
	GasUsed           string      `json:"gasUsed"`
	EffectiveGasPrice string      `json:"effectiveGasPrice"`
	GasUsedForL1      *string     `json:"gasUsedForL1,omitempty"`
	Logs              []PublicLog `json:"logs"`
}

type PublicLog struct {
	Address  string   `json:"address"`
	Topics   []string `json:"topics"`
	Data     string   `json:"data"`
	LogIndex string   `json:"logIndex"`
}

type BidEvidence struct {
	SolverFrom   string  `json:"solver_from"`
	SolverTo     string  `json:"solver_to"`
	BidToken     string  `json:"bid_token"`
	SubmittedBid string  `json:"submitted_bid"`
	SettledBid   *string `json:"settled_bid"`
	Executed     *bool   `json:"executed"`
	Success      *bool   `json:"success"`
	Result       *string `json:"result"`
}

type FailedOperation struct {
	SolverFrom string `json:"solver_from"`
	SolverTo   string `json:"solver_to"`
	Executed   bool   `json:"executed"`
	Result     string `json:"result"`
}

type MetacallEvidence struct {
	Bundler          string `json:"bundler"`
	User             string `json:"user"`
	SolverSuccessful bool   `json:"solver_successful"`
	EthPaidToBundler string `json:"eth_paid_to_bundler"`
	NetGasSurcharge  string `json:"net_gas_surcharge"`
}

type PublicLiquidation struct {
	CollateralAsset            string `json:"collateral_asset"`
	DebtAsset                  string `json:"debt_asset"`
	Borrower                   string `json:"borrower"`
	DebtToCover                string `json:"debt_to_cover"`
	LiquidatedCollateralAmount string `json:"liquidated_collateral_amount"`
	Liquidator                 string `json:"liquidator"`
	ReceiveAToken              bool   `json:"receive_a_token"`
}

type ReconciliationRecord struct {
	Schema                       string              `json:"schema"`
	ReconciledAt                 time.Time           `json:"reconciled_at"`
	AuctionID                    string              `json:"auction_id"`
	UserOpHash                   string              `json:"user_operation_hash"`
	TranscriptSHA256             string              `json:"transcript_sha256"`
	SearchFromBlock              uint64              `json:"search_from_block"`
	SearchToBlock                uint64              `json:"search_to_block"`
	PublicSettlementFound        bool                `json:"public_settlement_found"`
	OnchainTransaction           *string             `json:"onchain_transaction"`
	SuccessfulOnchainTransaction *string             `json:"successful_onchain_auction_transaction"`
	TransactionBlock             *uint64             `json:"transaction_block"`
	ReceiptStatus                *uint64             `json:"receipt_status"`
	AttemptedSolvers             []string            `json:"attempted_solvers"`
	WinningSolver                *string             `json:"winning_solver"`
	Bids                         []BidEvidence       `json:"bids"`
	GasUsed                      *string             `json:"gas_used"`
	EffectiveGasPriceWei         *string             `json:"effective_gas_price_wei"`
	ExecutionGasCostWei          *string             `json:"execution_gas_cost_wei"`
	GasUsedForL1                 *string             `json:"gas_used_for_l1"`
	FailedOperations             []FailedOperation   `json:"failed_operations"`
	FallbackOccurred             *bool               `json:"fallback_occurred"`
	Metacall                     *MetacallEvidence   `json:"metacall"`
	PublicLiquidations           []PublicLiquidation `json:"public_liquidations"`
	EstimatedGrossBonus          *string             `json:"estimated_gross_liquidation_bonus"`
	EstimatedWinnerResidualPnL   *string             `json:"estimated_winner_residual_pnl"`
}

type atlasUserOperation struct {
	From                 common.Address
	To                   common.Address
	Value                *big.Int
	Gas                  *big.Int
	MaxFeePerGas         *big.Int
	Nonce                *big.Int
	Deadline             *big.Int
	Dapp                 common.Address
	Control              common.Address
	CallConfig           uint32
	DappGasLimit         uint32
	SolverGasLimit       uint32
	BundlerSurchargeRate *big.Int
	SessionKey           common.Address
	Data                 []byte
	Signature            []byte
}

type atlasSolverOperation struct {
	From         common.Address
	To           common.Address
	Value        *big.Int
	Gas          *big.Int
	MaxFeePerGas *big.Int
	Deadline     *big.Int
	Solver       common.Address
	Control      common.Address
	UserOpHash   [32]byte
	BidToken     common.Address
	BidAmount    *big.Int
	Data         []byte
	Signature    []byte
}

type atlasDAppOperation struct {
	From          common.Address
	To            common.Address
	Nonce         *big.Int
	Deadline      *big.Int
	Control       common.Address
	Bundler       common.Address
	UserOpHash    [32]byte
	CallChainHash [32]byte
	Signature     []byte
}

type decodedMetacall struct {
	UserOp    atlasUserOperation
	SolverOps []atlasSolverOperation
	DAppOp    atlasDAppOperation
}

type solverResultEvent struct {
	SolverTo   common.Address
	SolverFrom common.Address
	Control    common.Address
	BidToken   common.Address
	BidAmount  *big.Int
	Executed   bool
	Success    bool
	Result     *big.Int
}

func DecodeRPCTranscript(reader io.Reader) (*RPCTranscript, []byte, error) {
	raw, err := io.ReadAll(io.LimitReader(reader, 128*1024*1024))
	if err != nil {
		return nil, nil, fmt.Errorf("read RPC transcript: %w", err)
	}
	raw = bytes.TrimPrefix(raw, []byte{0xef, 0xbb, 0xbf})
	var transcript RPCTranscript
	if err := json.Unmarshal(raw, &transcript); err != nil {
		return nil, nil, fmt.Errorf("decode RPC transcript: %w", err)
	}
	if transcript.Schema != RPCTranscriptSchema {
		return nil, nil, errors.New("unexpected RPC transcript schema")
	}
	if !strings.EqualFold(transcript.ChainID, ArbitrumChainIDHex) || !strings.EqualFold(transcript.Atlas, ArbitrumAtlas) {
		return nil, nil, errors.New("RPC transcript identity mismatch")
	}
	return &transcript, raw, nil
}

func ReconcileTranscript(ledgerDir string, transcript *RPCTranscript, transcriptRaw []byte, now time.Time) ([]ReconciliationRecord, error) {
	if !filepath.IsAbs(ledgerDir) {
		return nil, errors.New("ledger directory must be absolute")
	}
	fromBlock, err := parseHexUint64(transcript.FromBlock)
	if err != nil {
		return nil, fmt.Errorf("invalid transcript from block: %w", err)
	}
	toBlock, err := parseHexUint64(transcript.ToBlock)
	if err != nil {
		return nil, fmt.Errorf("invalid transcript to block: %w", err)
	}
	latestBlock, err := parseHexUint64(transcript.LatestBlock)
	if err != nil || fromBlock > toBlock || toBlock > latestBlock {
		return nil, errors.New("invalid transcript block bounds")
	}

	auctions, err := readAuctionLedger(filepath.Join(ledgerDir, "auctions.ndjson"))
	if err != nil {
		return nil, err
	}
	existing, err := readExistingReconciliations(filepath.Join(ledgerDir, "reconciliation.ndjson"))
	if err != nil {
		return nil, err
	}
	parsedABI, err := abi.JSON(strings.NewReader(atlasEvidenceABI))
	if err != nil {
		return nil, fmt.Errorf("parse evidence ABI: %w", err)
	}
	decodedTransactions, err := decodeTranscriptTransactions(transcript, parsedABI)
	if err != nil {
		return nil, err
	}
	digest := sha256.Sum256(transcriptRaw)
	digestText := hex.EncodeToString(digest[:])

	var records []ReconciliationRecord
	for _, auction := range auctions {
		if _, ok := existing[auction.AuctionID]; ok {
			continue
		}
		deadline, err := decimalUint64(auction.AuctionDeadlineBlock)
		if err != nil {
			return nil, fmt.Errorf("auction %s has invalid deadline: %w", auction.AuctionID, err)
		}
		if latestBlock < deadline+ReconciliationFinality {
			continue
		}
		requiredFrom := uint64(0)
		if deadline > ReconciliationLookback {
			requiredFrom = deadline - ReconciliationLookback
		}
		if fromBlock > requiredFrom || toBlock < deadline+ReconciliationFinality {
			continue
		}
		record := ReconciliationRecord{
			Schema:           ReconciliationSchema,
			ReconciledAt:     now.UTC(),
			AuctionID:        auction.AuctionID,
			UserOpHash:       strings.ToLower(auction.UserOpHash),
			TranscriptSHA256: digestText,
			SearchFromBlock:  fromBlock,
			SearchToBlock:    toBlock,
		}
		matches := decodedTransactions[strings.ToLower(auction.UserOpHash)]
		if len(matches) > 1 {
			return nil, fmt.Errorf("auction %s maps to multiple onchain metacalls", auction.AuctionID)
		}
		if len(matches) == 1 {
			if err := validateAuctionBinding(auction, matches[0]); err != nil {
				return nil, fmt.Errorf("bind auction %s: %w", auction.AuctionID, err)
			}
			if err := populateReconciliation(&record, matches[0], parsedABI); err != nil {
				return nil, fmt.Errorf("reconcile auction %s: %w", auction.AuctionID, err)
			}
		}
		recordHash, err := reconciliationIdentity(record)
		if err != nil {
			return nil, err
		}
		existing[record.AuctionID] = recordHash
		if err := appendJSONLine(filepath.Join(ledgerDir, "reconciliation.ndjson"), record); err != nil {
			return nil, err
		}
		records = append(records, record)
	}
	return records, nil
}

type decodedTransaction struct {
	public   PublicTransaction
	metacall decodedMetacall
}

func decodeTranscriptTransactions(transcript *RPCTranscript, parsedABI abi.ABI) (map[string][]decodedTransaction, error) {
	result := make(map[string][]decodedTransaction)
	seenTransactions := make(map[string]struct{})
	fromBlock, err := parseHexUint64(transcript.FromBlock)
	if err != nil {
		return nil, errors.New("invalid transcript from block")
	}
	toBlock, err := parseHexUint64(transcript.ToBlock)
	if err != nil {
		return nil, errors.New("invalid transcript to block")
	}
	for _, tx := range transcript.Transactions {
		if !hashPattern.MatchString(tx.Hash) || !strings.EqualFold(tx.To, ArbitrumAtlas) {
			return nil, errors.New("transcript contains transaction with invalid identity")
		}
		if !strings.EqualFold(tx.Receipt.TransactionHash, tx.Hash) {
			return nil, fmt.Errorf("receipt identity mismatch for %s", tx.Hash)
		}
		key := strings.ToLower(tx.Hash)
		if _, ok := seenTransactions[key]; ok {
			return nil, fmt.Errorf("duplicate transaction %s", tx.Hash)
		}
		seenTransactions[key] = struct{}{}
		txBlock, err := parseHexUint64(tx.BlockNumber)
		if err != nil || txBlock < fromBlock || txBlock > toBlock {
			return nil, fmt.Errorf("transaction %s is outside transcript bounds", tx.Hash)
		}
		receiptBlock, err := parseHexUint64(tx.Receipt.BlockNumber)
		if err != nil || receiptBlock != txBlock {
			return nil, fmt.Errorf("transaction %s block identity mismatch", tx.Hash)
		}
		input, err := decodeHexData(tx.Input)
		if err != nil {
			return nil, fmt.Errorf("decode transaction %s input: %w", tx.Hash, err)
		}
		metacall, err := decodeMetacall(input, parsedABI)
		if err != nil {
			return nil, fmt.Errorf("decode transaction %s metacall: %w", tx.Hash, err)
		}
		if !strings.EqualFold(metacall.UserOp.To.Hex(), ArbitrumAtlas) || !strings.EqualFold(metacall.UserOp.Control.Hex(), ArbitrumDappControl) || !strings.EqualFold(metacall.DAppOp.Control.Hex(), ArbitrumDappControl) {
			return nil, fmt.Errorf("transaction %s metacall identity mismatch", tx.Hash)
		}
		hash := "0x" + hex.EncodeToString(metacall.DAppOp.UserOpHash[:])
		for _, solverOp := range metacall.SolverOps {
			if solverOp.UserOpHash != metacall.DAppOp.UserOpHash || !strings.EqualFold(solverOp.Control.Hex(), ArbitrumDappControl) {
				return nil, fmt.Errorf("transaction %s contains mismatched solver operation", tx.Hash)
			}
		}
		result[strings.ToLower(hash)] = append(result[strings.ToLower(hash)], decodedTransaction{public: tx, metacall: metacall})
	}
	return result, nil
}

func validateAuctionBinding(auction LedgerRecord, tx decodedTransaction) error {
	deadline, err := decimalUint64(auction.AuctionDeadlineBlock)
	if err != nil {
		return err
	}
	txBlock, err := parseHexUint64(tx.public.BlockNumber)
	if err != nil || txBlock > deadline {
		return errors.New("settlement block exceeds the auction deadline")
	}
	if tx.metacall.UserOp.Deadline == nil || !tx.metacall.UserOp.Deadline.IsUint64() || tx.metacall.UserOp.Deadline.Uint64() != deadline {
		return errors.New("metacall deadline does not match gateway evidence")
	}
	if !strings.EqualFold(tx.metacall.UserOp.Dapp.Hex(), auction.Dapp) {
		return errors.New("metacall dapp does not match gateway evidence")
	}
	if tx.metacall.UserOp.MaxFeePerGas == nil || tx.metacall.UserOp.MaxFeePerGas.String() != auction.OracleGasPriceWei {
		return errors.New("metacall gas price does not match gateway evidence")
	}
	if auction.PartialUserOperation.From != nil && !strings.EqualFold(*auction.PartialUserOperation.From, common.Address{}.Hex()) && !strings.EqualFold(tx.metacall.UserOp.From.Hex(), *auction.PartialUserOperation.From) {
		return errors.New("metacall sender does not match gateway evidence")
	}
	return nil
}

func decodeMetacall(input []byte, parsedABI abi.ABI) (decodedMetacall, error) {
	method, ok := parsedABI.Methods["metacall"]
	if !ok || len(input) < 4 || !bytes.Equal(input[:4], method.ID) {
		return decodedMetacall{}, errors.New("transaction is not the exact Atlas metacall")
	}
	values, err := method.Inputs.Unpack(input[4:])
	if err != nil {
		return decodedMetacall{}, err
	}
	if len(values) != 4 {
		return decodedMetacall{}, errors.New("unexpected metacall argument count")
	}
	userOp := *abi.ConvertType(values[0], new(atlasUserOperation)).(*atlasUserOperation)
	solverOps := *abi.ConvertType(values[1], new([]atlasSolverOperation)).(*[]atlasSolverOperation)
	dAppOp := *abi.ConvertType(values[2], new(atlasDAppOperation)).(*atlasDAppOperation)
	return decodedMetacall{UserOp: userOp, SolverOps: solverOps, DAppOp: dAppOp}, nil
}

func populateReconciliation(record *ReconciliationRecord, tx decodedTransaction, parsedABI abi.ABI) error {
	receiptStatus, err := parseHexUint64(tx.public.Receipt.Status)
	if err != nil || receiptStatus > 1 {
		return errors.New("invalid receipt status")
	}
	block, err := parseHexUint64(tx.public.Receipt.BlockNumber)
	if err != nil {
		return errors.New("invalid receipt block")
	}
	gasUsed, err := parseHexUint(tx.public.Receipt.GasUsed)
	if err != nil {
		return errors.New("invalid receipt gas used")
	}
	gasPrice, err := parseHexUint(tx.public.Receipt.EffectiveGasPrice)
	if err != nil {
		return errors.New("invalid receipt effective gas price")
	}
	txHash := strings.ToLower(tx.public.Hash)
	record.PublicSettlementFound = true
	record.OnchainTransaction = &txHash
	record.TransactionBlock = &block
	record.ReceiptStatus = &receiptStatus
	gasUsedText := gasUsed.String()
	gasPriceText := gasPrice.String()
	gasCostText := new(big.Int).Mul(gasUsed, gasPrice).String()
	record.GasUsed = &gasUsedText
	record.EffectiveGasPriceWei = &gasPriceText
	record.ExecutionGasCostWei = &gasCostText
	record.GasUsedForL1 = tx.public.Receipt.GasUsedForL1
	record.AttemptedSolvers = []string{}
	record.Bids = []BidEvidence{}
	record.FailedOperations = []FailedOperation{}
	record.PublicLiquidations = []PublicLiquidation{}
	if receiptStatus == 1 {
		record.SuccessfulOnchainTransaction = &txHash
	}

	bySolver := make(map[string]int)
	for _, solverOp := range tx.metacall.SolverOps {
		from := strings.ToLower(solverOp.From.Hex())
		to := strings.ToLower(solverOp.Solver.Hex())
		record.AttemptedSolvers = append(record.AttemptedSolvers, from)
		bid := BidEvidence{
			SolverFrom:   from,
			SolverTo:     to,
			BidToken:     strings.ToLower(solverOp.BidToken.Hex()),
			SubmittedBid: solverOp.BidAmount.String(),
		}
		record.Bids = append(record.Bids, bid)
		bySolver[from+"|"+to] = len(record.Bids) - 1
	}
	sort.Strings(record.AttemptedSolvers)

	var metacallSeen bool
	for _, log := range tx.public.Receipt.Logs {
		if !addressPattern.MatchString(log.Address) {
			return errors.New("receipt contains invalid log address")
		}
		for _, topic := range log.Topics {
			if !hashPattern.MatchString(topic) {
				return errors.New("receipt contains invalid log topic")
			}
		}
		if len(log.Topics) == 0 {
			continue
		}
		switch {
		case strings.EqualFold(log.Address, ArbitrumAtlas) && strings.EqualFold(log.Topics[0], parsedABI.Events["SolverTxResult"].ID.Hex()):
			event, err := decodeSolverResult(log, parsedABI)
			if err != nil {
				return err
			}
			if !strings.EqualFold(event.Control.Hex(), ArbitrumDappControl) {
				return errors.New("solver result has unexpected DappControl")
			}
			key := strings.ToLower(event.SolverFrom.Hex()) + "|" + strings.ToLower(event.SolverTo.Hex())
			bidIndex, ok := bySolver[key]
			if !ok {
				return errors.New("solver result does not bind to submitted solver operation")
			}
			settledBid := event.BidAmount.String()
			executed := event.Executed
			success := event.Success
			result := event.Result.String()
			bid := &record.Bids[bidIndex]
			bid.SettledBid = &settledBid
			bid.Executed = &executed
			bid.Success = &success
			bid.Result = &result
			if event.Success {
				winner := strings.ToLower(event.SolverFrom.Hex())
				if record.WinningSolver != nil && *record.WinningSolver != winner {
					return errors.New("multiple winning solvers in single-winner SVR auction")
				}
				record.WinningSolver = &winner
			} else {
				record.FailedOperations = append(record.FailedOperations, FailedOperation{
					SolverFrom: strings.ToLower(event.SolverFrom.Hex()),
					SolverTo:   strings.ToLower(event.SolverTo.Hex()),
					Executed:   event.Executed,
					Result:     event.Result.String(),
				})
			}
		case strings.EqualFold(log.Address, ArbitrumAtlas) && strings.EqualFold(log.Topics[0], parsedABI.Events["MetacallResult"].ID.Hex()):
			if metacallSeen {
				return errors.New("multiple MetacallResult events")
			}
			metacall, err := decodeMetacallResult(log, parsedABI)
			if err != nil {
				return err
			}
			record.Metacall = metacall
			fallback := !metacall.SolverSuccessful
			record.FallbackOccurred = &fallback
			metacallSeen = true
		case strings.EqualFold(log.Address, AaveV3ArbitrumPool) && strings.EqualFold(log.Topics[0], parsedABI.Events["LiquidationCall"].ID.Hex()):
			liquidation, err := decodeLiquidation(log, parsedABI)
			if err != nil {
				return err
			}
			record.PublicLiquidations = append(record.PublicLiquidations, *liquidation)
		}
	}
	if receiptStatus == 1 && !metacallSeen {
		return errors.New("successful Atlas metacall is missing MetacallResult")
	}
	return nil
}

func decodeSolverResult(log PublicLog, parsedABI abi.ABI) (*solverResultEvent, error) {
	if len(log.Topics) != 4 {
		return nil, errors.New("invalid SolverTxResult topic count")
	}
	data, err := decodeHexData(log.Data)
	if err != nil {
		return nil, err
	}
	values, err := parsedABI.Events["SolverTxResult"].Inputs.NonIndexed().Unpack(data)
	if err != nil || len(values) != 5 {
		return nil, errors.New("invalid SolverTxResult data")
	}
	return &solverResultEvent{
		SolverTo:   topicAddress(log.Topics[1]),
		SolverFrom: topicAddress(log.Topics[2]),
		Control:    topicAddress(log.Topics[3]),
		BidToken:   values[0].(common.Address),
		BidAmount:  values[1].(*big.Int),
		Executed:   values[2].(bool),
		Success:    values[3].(bool),
		Result:     values[4].(*big.Int),
	}, nil
}

func decodeMetacallResult(log PublicLog, parsedABI abi.ABI) (*MetacallEvidence, error) {
	if len(log.Topics) != 3 {
		return nil, errors.New("invalid MetacallResult topic count")
	}
	data, err := decodeHexData(log.Data)
	if err != nil {
		return nil, err
	}
	values, err := parsedABI.Events["MetacallResult"].Inputs.NonIndexed().Unpack(data)
	if err != nil || len(values) != 3 {
		return nil, errors.New("invalid MetacallResult data")
	}
	return &MetacallEvidence{
		Bundler:          strings.ToLower(topicAddress(log.Topics[1]).Hex()),
		User:             strings.ToLower(topicAddress(log.Topics[2]).Hex()),
		SolverSuccessful: values[0].(bool),
		EthPaidToBundler: values[1].(*big.Int).String(),
		NetGasSurcharge:  values[2].(*big.Int).String(),
	}, nil
}

func decodeLiquidation(log PublicLog, parsedABI abi.ABI) (*PublicLiquidation, error) {
	if len(log.Topics) != 4 {
		return nil, errors.New("invalid LiquidationCall topic count")
	}
	data, err := decodeHexData(log.Data)
	if err != nil {
		return nil, err
	}
	values, err := parsedABI.Events["LiquidationCall"].Inputs.NonIndexed().Unpack(data)
	if err != nil || len(values) != 4 {
		return nil, errors.New("invalid LiquidationCall data")
	}
	return &PublicLiquidation{
		CollateralAsset:            strings.ToLower(topicAddress(log.Topics[1]).Hex()),
		DebtAsset:                  strings.ToLower(topicAddress(log.Topics[2]).Hex()),
		Borrower:                   strings.ToLower(topicAddress(log.Topics[3]).Hex()),
		DebtToCover:                values[0].(*big.Int).String(),
		LiquidatedCollateralAmount: values[1].(*big.Int).String(),
		Liquidator:                 strings.ToLower(values[2].(common.Address).Hex()),
		ReceiveAToken:              values[3].(bool),
	}, nil
}

func readAuctionLedger(path string) ([]LedgerRecord, error) {
	f, err := secureOpenForRead(path)
	if err != nil {
		return nil, err
	}
	defer f.Close()
	var records []LedgerRecord
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 64*1024), WebSocketReadLimitBytes*2)
	for scanner.Scan() {
		var record LedgerRecord
		if err := json.Unmarshal(scanner.Bytes(), &record); err != nil {
			return nil, fmt.Errorf("decode auction ledger: %w", err)
		}
		if record.Schema != Schema || record.AuctionID == "" || !hashPattern.MatchString(record.UserOpHash) {
			return nil, errors.New("auction ledger contains invalid record")
		}
		records = append(records, record)
	}
	return records, scanner.Err()
}

func readExistingReconciliations(path string) (map[string]string, error) {
	result := make(map[string]string)
	f, err := secureOpenForRead(path)
	if errors.Is(err, os.ErrNotExist) {
		return result, nil
	}
	if err != nil {
		return nil, err
	}
	defer f.Close()
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 64*1024), WebSocketReadLimitBytes*2)
	for scanner.Scan() {
		var record ReconciliationRecord
		if err := json.Unmarshal(scanner.Bytes(), &record); err != nil {
			return nil, fmt.Errorf("decode reconciliation ledger: %w", err)
		}
		if record.Schema != ReconciliationSchema || record.AuctionID == "" {
			return nil, errors.New("reconciliation ledger contains invalid record")
		}
		identity, err := reconciliationIdentity(record)
		if err != nil {
			return nil, err
		}
		if _, exists := result[record.AuctionID]; exists {
			return nil, errors.New("reconciliation ledger contains duplicate auction identity")
		}
		result[record.AuctionID] = identity
	}
	return result, scanner.Err()
}

func reconciliationIdentity(record ReconciliationRecord) (string, error) {
	encoded, err := json.Marshal(record)
	if err != nil {
		return "", err
	}
	digest := sha256.Sum256(encoded)
	return hex.EncodeToString(digest[:]), nil
}

func parseHexUint64(value string) (uint64, error) {
	n, err := parseHexUint(value)
	if err != nil || !n.IsUint64() {
		return 0, errors.New("hexadecimal quantity exceeds uint64")
	}
	return n.Uint64(), nil
}

func decimalUint64(value string) (uint64, error) {
	n := new(big.Int)
	if _, ok := n.SetString(value, 10); !ok || !n.IsUint64() {
		return 0, errors.New("decimal quantity exceeds uint64")
	}
	return n.Uint64(), nil
}

func decodeHexData(value string) ([]byte, error) {
	if !strings.HasPrefix(value, "0x") || len(value)%2 != 0 {
		return nil, errors.New("invalid hexadecimal data")
	}
	return hex.DecodeString(value[2:])
}

func topicAddress(value string) common.Address {
	return common.HexToAddress(value)
}
