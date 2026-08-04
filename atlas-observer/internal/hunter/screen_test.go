package hunter

import (
	"crypto/sha256"
	"encoding/hex"
	"io"
	"os"
	"path/filepath"
	"strings"
	"testing"
	"time"
)

func TestNewRequiresCanonicalGitReleaseSHA(t *testing.T) {
	directory := t.TempDir()
	discovery := filepath.Join(directory, "discovery.json")
	content := []byte(`{"borrowers":[]}`)
	if err := os.WriteFile(discovery, content, 0o600); err != nil {
		t.Fatal(err)
	}
	digest := sha256.Sum256(content)
	config := Config{
		DiscoveryPath:          discovery,
		DiscoverySHA256:        hex.EncodeToString(digest[:]),
		StateDir:               filepath.Join(directory, "state"),
		GatewayURL:             "http://rpc-gateway:9300",
		StartingCursor:         1100,
		BatchSize:              1,
		Pace:                   time.Second,
		RetainedProfitFloorWei: "1",
		MaximumGasLimit:        1,
		MaximumFeePerGasWei:    "1",
		FlashPremiumBPS:        9,
		EconomicReserveBPS:     500,
		ExecutorAddress:        "0x1111111111111111111111111111111111111111",
		ExecutorCodeHash:       strings.Repeat("a", 64),
		CallerAddress:          "0x2222222222222222222222222222222222222222",
		ReleaseSHA:             strings.Repeat("b", 40),
		MaximumPriorityFeeWei:  "1",
	}
	if _, err := New(config); err != nil {
		t.Fatalf("canonical Git release SHA was rejected: %v", err)
	}
	for name, releaseSHA := range map[string]string{
		"legacy 64-character digest": strings.Repeat("b", 64),
		"uppercase Git SHA":          strings.Repeat("B", 40),
	} {
		t.Run(name, func(t *testing.T) {
			invalid := config
			invalid.ReleaseSHA = releaseSHA
			if _, err := New(invalid); err == nil || err.Error() != "hunter execution identity is invalid" {
				t.Fatalf("unexpected error: %v", err)
			}
		})
	}
}

func TestClassificationIsIntegerAndFailClosed(t *testing.T) {
	tests := map[string]struct{ debt, hf, want string }{
		"no debt":      {"0", "0", "no_debt"},
		"liquidatable": {"1", "999999999999999999", "liquidatable"},
		"urgent":       {"1", "1000000000000000000", "urgent"},
		"watch":        {"1", "1020000000000000000", "watch"},
		"safe":         {"1", "1100000000000000000", "debt_safe"},
		"malformed":    {"1", "not-an-integer", "incomplete"},
	}
	for name, test := range tests {
		t.Run(name, func(t *testing.T) {
			if observed := classify(test.debt, test.hf); observed != test.want {
				t.Fatalf("classification=%s want=%s", observed, test.want)
			}
		})
	}
}

func TestDiscoveryStreamPreservesOrderAndRejectsSubstitution(t *testing.T) {
	directory := t.TempDir()
	path := filepath.Join(directory, "discovery.json")
	content := []byte(`{"schema":"fixture","borrowers":["0x1111111111111111111111111111111111111111","0x2222222222222222222222222222222222222222"],"content_sha256":"fixture"}`)
	if err := os.WriteFile(path, content, 0o600); err != nil {
		t.Fatal(err)
	}
	stream, err := streamBorrowers(path)
	if err != nil {
		t.Fatal(err)
	}
	defer stream.Close()
	first, err := stream.Next()
	if err != nil || first != "0x1111111111111111111111111111111111111111" {
		t.Fatalf("first=%s err=%v", first, err)
	}
	second, err := stream.Next()
	if err != nil || second != "0x2222222222222222222222222222222222222222" {
		t.Fatalf("second=%s err=%v", second, err)
	}
	if _, err := stream.Next(); err != io.EOF {
		t.Fatalf("expected EOF, got %v", err)
	}
}

func TestBorrowerIndexIsDurableAndFailClosed(t *testing.T) {
	directory := t.TempDir()
	first := &Screener{
		config:       Config{StateDir: directory},
		state:        State{},
		debtBearing:  make(map[string]bool),
		refreshKnown: make(map[string]bool),
	}
	borrower := "0x1111111111111111111111111111111111111111"
	if err := first.updateBorrowerActivityLocked(borrower, true); err != nil {
		t.Fatal(err)
	}
	if !first.debtBearing[borrower] || first.state.DebtBearingCount != 1 {
		t.Fatal("active borrower was not indexed")
	}
	second := &Screener{
		config:       Config{StateDir: directory},
		state:        State{},
		debtBearing:  make(map[string]bool),
		refreshKnown: make(map[string]bool),
	}
	if err := second.loadBorrowerIndex(); err != nil {
		t.Fatal(err)
	}
	if !second.debtBearing[borrower] || len(second.refreshOrder) != 1 {
		t.Fatal("durable borrower index did not replay")
	}
	if err := second.updateBorrowerActivityLocked(borrower, false); err != nil {
		t.Fatal(err)
	}
	third := &Screener{
		config:       Config{StateDir: directory},
		state:        State{},
		debtBearing:  make(map[string]bool),
		refreshKnown: make(map[string]bool),
	}
	if err := third.loadBorrowerIndex(); err != nil {
		t.Fatal(err)
	}
	if third.debtBearing[borrower] || third.state.DebtBearingCount != 0 {
		t.Fatal("inactive borrower remained debt-bearing")
	}
}
