package observer

import (
	"encoding/hex"
	"encoding/json"
	"math/big"
	"strings"
	"testing"
	"time"

	"github.com/ethereum/go-ethereum/accounts/abi"
	"github.com/ethereum/go-ethereum/common"
)

const (
	testTxHash     = "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"
	testSolverFrom = "0x3333333333333333333333333333333333333333"
	testSolverTo   = "0x4444444444444444444444444444444444444444"
	testCollateral = "0x5555555555555555555555555555555555555555"
	testDebt       = "0x6666666666666666666666666666666666666666"
	testBorrower   = "0x7777777777777777777777777777777777777777"
)

func TestReconcileTranscriptDecodesExactAtlasAndAaveEvidence(t *testing.T) {
	dir := t.TempDir()
	appendTestAuction(t, dir, validUserHash, "1500")
	transcript := buildTestTranscript(t, validUserHash, true)
	raw, err := json.Marshal(transcript)
	if err != nil {
		t.Fatal(err)
	}
	records, err := ReconcileTranscript(dir, transcript, raw, time.Unix(1_700_000_100, 0))
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 1 {
		t.Fatalf("got %d records, want 1", len(records))
	}
	record := records[0]
	if !record.PublicSettlementFound || record.SuccessfulOnchainTransaction == nil || *record.SuccessfulOnchainTransaction != testTxHash {
		t.Fatalf("settlement evidence not retained: %+v", record)
	}
	if len(record.AttemptedSolvers) != 1 || record.AttemptedSolvers[0] != testSolverFrom {
		t.Fatalf("attempted solvers = %v", record.AttemptedSolvers)
	}
	if record.WinningSolver == nil || *record.WinningSolver != testSolverFrom {
		t.Fatalf("winning solver = %v", record.WinningSolver)
	}
	if len(record.Bids) != 1 || record.Bids[0].SettledBid == nil || *record.Bids[0].SettledBid != "123" || record.Bids[0].Success == nil || !*record.Bids[0].Success {
		t.Fatalf("bid evidence = %+v", record.Bids)
	}
	if record.FallbackOccurred == nil || *record.FallbackOccurred {
		t.Fatalf("fallback evidence = %v", record.FallbackOccurred)
	}
	if record.ExecutionGasCostWei == nil || *record.ExecutionGasCostWei != "4200000" {
		t.Fatalf("gas cost = %v", record.ExecutionGasCostWei)
	}
	if len(record.PublicLiquidations) != 1 {
		t.Fatalf("liquidations = %+v", record.PublicLiquidations)
	}
	liquidation := record.PublicLiquidations[0]
	if liquidation.CollateralAsset != testCollateral || liquidation.DebtAsset != testDebt || liquidation.Borrower != testBorrower || liquidation.DebtToCover != "50" || liquidation.LiquidatedCollateralAmount != "55" {
		t.Fatalf("liquidation evidence = %+v", liquidation)
	}

	repeated, err := ReconcileTranscript(dir, transcript, raw, time.Unix(1_700_000_200, 0))
	if err != nil {
		t.Fatal(err)
	}
	if len(repeated) != 0 {
		t.Fatalf("repeated reconciliation appended %d records", len(repeated))
	}
}

func TestReconcileTranscriptPersistsNullsWhenNoSettlementExists(t *testing.T) {
	dir := t.TempDir()
	appendTestAuction(t, dir, validUserHash, "1500")
	transcript := buildTestTranscript(t, validUserHash, false)
	raw, err := json.Marshal(transcript)
	if err != nil {
		t.Fatal(err)
	}
	records, err := ReconcileTranscript(dir, transcript, raw, time.Unix(1_700_000_100, 0))
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 1 || records[0].PublicSettlementFound || records[0].OnchainTransaction != nil || records[0].AttemptedSolvers != nil || records[0].Bids != nil || records[0].FallbackOccurred != nil || records[0].PublicLiquidations != nil {
		t.Fatalf("unknown settlement fields were not null: %+v", records)
	}
}

func TestReconcileTranscriptRejectsWrongIdentityAndIncompleteCoverage(t *testing.T) {
	dir := t.TempDir()
	appendTestAuction(t, dir, validUserHash, "1500")
	transcript := buildTestTranscript(t, validUserHash, false)
	transcript.Atlas = testSolverTo
	raw, _ := json.Marshal(transcript)
	decoded, _, err := DecodeRPCTranscript(strings.NewReader(string(raw)))
	if err == nil || decoded != nil {
		t.Fatal("wrong Atlas identity was accepted")
	}

	transcript = buildTestTranscript(t, validUserHash, false)
	transcript.FromBlock = "0x5dc"
	raw, _ = json.Marshal(transcript)
	records, err := ReconcileTranscript(dir, transcript, raw, time.Unix(1_700_000_100, 0))
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 0 {
		t.Fatal("auction reconciled without the required bounded lookback")
	}
}

func TestDecodeRPCTranscriptAcceptsUTF8TransportBOM(t *testing.T) {
	transcript := buildTestTranscript(t, validUserHash, false)
	raw, err := json.Marshal(transcript)
	if err != nil {
		t.Fatal(err)
	}
	raw = append([]byte{0xef, 0xbb, 0xbf}, raw...)
	decoded, canonical, err := DecodeRPCTranscript(strings.NewReader(string(raw)))
	if err != nil {
		t.Fatal(err)
	}
	if decoded.Schema != RPCTranscriptSchema || len(canonical) == 0 || canonical[0] != '{' {
		t.Fatal("transport BOM was not removed before canonical transcript hashing")
	}
}

func TestReconcileTranscriptAcceptsZeroSenderPlaceholderForHintedOperation(t *testing.T) {
	dir := t.TempDir()
	appendTestAuctionFrom(t, dir, validUserHash, "1500", common.Address{}.Hex())
	transcript := buildTestTranscript(t, validUserHash, true)
	raw, err := json.Marshal(transcript)
	if err != nil {
		t.Fatal(err)
	}
	records, err := ReconcileTranscript(dir, transcript, raw, time.Unix(1_700_000_100, 0))
	if err != nil {
		t.Fatal(err)
	}
	if len(records) != 1 || !records[0].PublicSettlementFound {
		t.Fatal("zero sender placeholder prevented exact hash-bound reconciliation")
	}
}

func appendTestAuction(t *testing.T, dir, userOpHash, deadline string) {
	t.Helper()
	appendTestAuctionFrom(t, dir, userOpHash, deadline, "0x1111111111111111111111111111111111111111")
}

func appendTestAuctionFrom(t *testing.T, dir, userOpHash, deadline, from string) {
	t.Helper()
	start := time.Unix(1_700_000_000, 0).UTC()
	ledger, err := OpenLedger(dir, start, 500, 72*time.Hour)
	if err != nil {
		t.Fatal(err)
	}
	_, err = ledger.Append(&LedgerRecord{
		Schema:               Schema,
		ObservedAt:           start,
		AuctionID:            validAuctionID,
		NotificationSHA256:   strings.Repeat("1", 64),
		Dapp:                 validDapp,
		UserOpHash:           userOpHash,
		AuctionDeadlineBlock: deadline,
		OracleGasPriceWei:    "20",
		PartialUserOperation: PartialUserOperation{
			From: func() *string {
				value := from
				return &value
			}(),
		},
	})
	if err != nil {
		t.Fatal(err)
	}
}

func buildTestTranscript(t *testing.T, userOpHash string, includeTransaction bool) *RPCTranscript {
	t.Helper()
	transcript := &RPCTranscript{
		Schema:      RPCTranscriptSchema,
		ChainID:     ArbitrumChainIDHex,
		Atlas:       ArbitrumAtlas,
		FromBlock:   "0x3dc",
		ToBlock:     "0x640",
		LatestBlock: "0x7d0",
	}
	if !includeTransaction {
		transcript.Transactions = []PublicTransaction{}
		return transcript
	}
	parsedABI, err := abi.JSON(strings.NewReader(atlasEvidenceABI))
	if err != nil {
		t.Fatal(err)
	}
	hash := common.HexToHash(userOpHash)
	userOp := atlasUserOperation{
		From:                 common.HexToAddress("0x1111111111111111111111111111111111111111"),
		To:                   common.HexToAddress(ArbitrumAtlas),
		Value:                big.NewInt(0),
		Gas:                  big.NewInt(500_000),
		MaxFeePerGas:         big.NewInt(20),
		Nonce:                big.NewInt(1),
		Deadline:             big.NewInt(1500),
		Dapp:                 common.HexToAddress(validDapp),
		Control:              common.HexToAddress(ArbitrumDappControl),
		CallConfig:           1,
		DappGasLimit:         100_000,
		SolverGasLimit:       uint32(ObservedSolverGasLimit),
		BundlerSurchargeRate: big.NewInt(0),
		SessionKey:           common.HexToAddress("0x2222222222222222222222222222222222222222"),
		Data:                 []byte{1},
		Signature:            []byte{2},
	}
	solverOp := atlasSolverOperation{
		From:         common.HexToAddress(testSolverFrom),
		To:           common.HexToAddress(ArbitrumAtlas),
		Value:        big.NewInt(0),
		Gas:          big.NewInt(400_000),
		MaxFeePerGas: big.NewInt(20),
		Deadline:     big.NewInt(1500),
		Solver:       common.HexToAddress(testSolverTo),
		Control:      common.HexToAddress(ArbitrumDappControl),
		UserOpHash:   hash,
		BidToken:     common.Address{},
		BidAmount:    big.NewInt(123),
		Data:         []byte{3},
		Signature:    []byte{4},
	}
	dAppOp := atlasDAppOperation{
		From:          common.HexToAddress("0x8888888888888888888888888888888888888888"),
		To:            common.HexToAddress(ArbitrumAtlas),
		Nonce:         big.NewInt(1),
		Deadline:      big.NewInt(1500),
		Control:       common.HexToAddress(ArbitrumDappControl),
		Bundler:       common.HexToAddress("0x9999999999999999999999999999999999999999"),
		UserOpHash:    hash,
		CallChainHash: common.HexToHash("0xbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"),
		Signature:     []byte{5},
	}
	arguments, err := parsedABI.Methods["metacall"].Inputs.Pack(userOp, []atlasSolverOperation{solverOp}, dAppOp, common.Address{})
	if err != nil {
		t.Fatal(err)
	}
	input := append(append([]byte{}, parsedABI.Methods["metacall"].ID...), arguments...)

	solverData, err := parsedABI.Events["SolverTxResult"].Inputs.NonIndexed().Pack(common.Address{}, big.NewInt(123), true, true, big.NewInt(0))
	if err != nil {
		t.Fatal(err)
	}
	metacallData, err := parsedABI.Events["MetacallResult"].Inputs.NonIndexed().Pack(true, big.NewInt(7), big.NewInt(8))
	if err != nil {
		t.Fatal(err)
	}
	liquidationData, err := parsedABI.Events["LiquidationCall"].Inputs.NonIndexed().Pack(big.NewInt(50), big.NewInt(55), common.HexToAddress(testSolverTo), false)
	if err != nil {
		t.Fatal(err)
	}
	transcript.Transactions = []PublicTransaction{{
		Hash:             testTxHash,
		To:               ArbitrumAtlas,
		Input:            "0x" + hex.EncodeToString(input),
		BlockNumber:      "0x5d2",
		TransactionIndex: "0x1",
		Receipt: PublicReceipt{
			TransactionHash:   testTxHash,
			BlockNumber:       "0x5d2",
			Status:            "0x1",
			GasUsed:           "0x33450",
			EffectiveGasPrice: "0x14",
			Logs: []PublicLog{
				{
					Address: ArbitrumAtlas,
					Topics: []string{
						parsedABI.Events["SolverTxResult"].ID.Hex(),
						common.BytesToHash(common.HexToAddress(testSolverTo).Bytes()).Hex(),
						common.BytesToHash(common.HexToAddress(testSolverFrom).Bytes()).Hex(),
						common.BytesToHash(common.HexToAddress(ArbitrumDappControl).Bytes()).Hex(),
					},
					Data: "0x" + hex.EncodeToString(solverData),
				},
				{
					Address: ArbitrumAtlas,
					Topics: []string{
						parsedABI.Events["MetacallResult"].ID.Hex(),
						common.BytesToHash(dAppOp.Bundler.Bytes()).Hex(),
						common.BytesToHash(userOp.From.Bytes()).Hex(),
					},
					Data: "0x" + hex.EncodeToString(metacallData),
				},
				{
					Address: AaveV3ArbitrumPool,
					Topics: []string{
						parsedABI.Events["LiquidationCall"].ID.Hex(),
						common.BytesToHash(common.HexToAddress(testCollateral).Bytes()).Hex(),
						common.BytesToHash(common.HexToAddress(testDebt).Bytes()).Hex(),
						common.BytesToHash(common.HexToAddress(testBorrower).Bytes()).Hex(),
					},
					Data: "0x" + hex.EncodeToString(liquidationData),
				},
			},
		},
	}}
	return transcript
}
