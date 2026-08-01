package observer

import (
	"encoding/json"
	"errors"
	"strings"
	"testing"
	"time"
)

const (
	validAuctionID = "1bc9a4ce-4fcf-4eb1-8632-959e2273953e"
	validUserHash  = "0x1111111111111111111111111111111111111111111111111111111111111111"
	validDapp      = "0x2222222222222222222222222222222222222222"
)

func validNotification(t *testing.T, aggregator string) []byte {
	t.Helper()
	payload := map[string]any{
		"jsonrpc": "2.0",
		"method":  "solver_subscription",
		"params": map[string]any{
			"subscription": "5ed17bf8-ed27-4625-97ef-447554594a3c",
			"result": map[string]any{
				"auction_id": validAuctionID,
				"partial_user_operation": map[string]any{
					"chainId":      ArbitrumChainIDHex,
					"userOpHash":   validUserHash,
					"to":           ArbitrumAtlas,
					"gas":          "0x4e20",
					"maxFeePerGas": "0x4c4b40",
					"deadline":     "0x1dcd6500",
					"dapp":         validDapp,
					"control":      ArbitrumDappControl,
					"from":         "0x5555555555555555555555555555555555555555",
					"hints": map[string]any{
						"aggregator":  aggregator,
						"medianPrice": "0x174876e800",
						"rawReport":   "0x1234",
					},
				},
			},
		},
	}
	raw, err := json.Marshal(payload)
	if err != nil {
		t.Fatal(err)
	}
	return raw
}

func TestHintedNotificationRejectsValueOrData(t *testing.T) {
	raw := validNotification(t, "0xc1720A8240Dbd992d95D6c865A15e490901879B1")
	var payload map[string]any
	if err := json.Unmarshal(raw, &payload); err != nil {
		t.Fatal(err)
	}
	uop := payload["params"].(map[string]any)["result"].(map[string]any)["partial_user_operation"].(map[string]any)
	uop["data"] = "0x1234"
	encoded, err := json.Marshal(payload)
	if err != nil {
		t.Fatal(err)
	}
	if _, err := DecodeAndValidateNotification(encoded, time.Now()); err == nil {
		t.Fatal("hinted notification with full data was accepted")
	}
}

func TestDecodeAndValidateRelevantAaveNotification(t *testing.T) {
	raw := validNotification(t, "0x4c76F02E484e8ce9B6C2358CF9624BabC5531E9e")
	record, err := DecodeAndValidateNotification(raw, time.Unix(1_700_000_000, 0))
	if err != nil {
		t.Fatal(err)
	}
	if record.ChainID != ArbitrumChainID || record.Atlas != ArbitrumAtlas || record.DappControl != ArbitrumDappControl {
		t.Fatalf("verified identity mismatch: %#v", record)
	}
	if !record.RelevantAaveAuction || record.OracleUpdate == nil || record.OracleUpdate.Asset == nil || *record.OracleUpdate.Asset != "LINK" {
		t.Fatalf("Aave-SVR classification mismatch: %#v", record.OracleUpdate)
	}
	if record.ParallelEligible {
		t.Fatal("LINK must not be classified as an official parallel feed")
	}
	if record.OracleGasPriceWei != "5000000" || record.AuctionDeadlineBlock != "500000000" {
		t.Fatalf("integer decoding mismatch: gas=%s deadline=%s", record.OracleGasPriceWei, record.AuctionDeadlineBlock)
	}
	if record.SolverGasLimit != ObservedSolverGasLimit || record.SuccessfulOnchainTransaction != nil || record.Bids != nil {
		t.Fatal("unreconciled evidence must remain null")
	}
}

func TestDecodeFailsClosedOnIdentityMismatch(t *testing.T) {
	cases := []struct {
		name string
		old  string
		new  string
	}{
		{name: "atlas", old: ArbitrumAtlas, new: "0x3333333333333333333333333333333333333333"},
		{name: "control", old: ArbitrumDappControl, new: "0x4444444444444444444444444444444444444444"},
	}
	for _, tc := range cases {
		t.Run(tc.name, func(t *testing.T) {
			raw := []byte(strings.Replace(string(validNotification(t, "0xc1720A8240Dbd992d95D6c865A15e490901879B1")), tc.old, tc.new, 1))
			if _, err := DecodeAndValidateNotification(raw, time.Now()); err == nil {
				t.Fatal("identity mismatch was accepted")
			}
		})
	}
}

func TestOtherChainsAreFilteredWithoutInvariantFailure(t *testing.T) {
	raw := []byte(strings.Replace(string(validNotification(t, "0xc1720A8240Dbd992d95D6c865A15e490901879B1")), ArbitrumChainIDHex, "0x2105", 1))
	if _, err := DecodeAndValidateNotification(raw, time.Now()); !errors.Is(err, ErrOtherChain) {
		t.Fatalf("expected other-chain filter, got %v", err)
	}
}

func TestParallelIdentityIgnoresAuctionTransportFields(t *testing.T) {
	rawA := validNotification(t, "0xa5E1a36938769cbd5a26f5e19D8FCB379f597c83")
	rawB := []byte(strings.ReplaceAll(string(rawA), validAuctionID, "aaaaaaaa-bbbb-cccc-dddd-eeeeeeeeeeee"))
	rawB = []byte(strings.ReplaceAll(string(rawB), validUserHash, "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"))
	rawB = []byte(strings.ReplaceAll(string(rawB), "0x1dcd6500", "0x1dcd6501"))
	a, err := DecodeAndValidateNotification(rawA, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	b, err := DecodeAndValidateNotification(rawB, time.Now())
	if err != nil {
		t.Fatal(err)
	}
	if !a.ParallelEligible || !b.ParallelEligible || a.ParallelAuctionIdentity != b.ParallelAuctionIdentity {
		t.Fatalf("parallel identity changed across transport fields: %s != %s", a.ParallelAuctionIdentity, b.ParallelAuctionIdentity)
	}
}

func TestAllOfficialAaveFeedsClassify(t *testing.T) {
	if len(aaveSVRFeeds) != 11 {
		t.Fatalf("expected 11 official Arbitrum Aave-SVR feeds, got %d", len(aaveSVRFeeds))
	}
	for _, feed := range aaveSVRFeeds {
		record, err := DecodeAndValidateNotification(validNotification(t, feed.Aggregator), time.Now())
		if err != nil {
			t.Fatalf("%s: %v", feed.Asset, err)
		}
		if !record.RelevantAaveAuction || record.OracleUpdate == nil || record.OracleUpdate.Asset == nil || *record.OracleUpdate.Asset != feed.Asset {
			t.Fatalf("feed %s was not classified exactly", feed.Asset)
		}
	}
}
