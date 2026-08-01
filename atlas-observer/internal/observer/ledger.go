package observer

import (
	"bufio"
	"encoding/json"
	"errors"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"sync"
	"time"
)

type LedgerState struct {
	Schema                  string            `json:"schema"`
	StartedAt               time.Time         `json:"started_at"`
	StopAt                  time.Time         `json:"stop_at"`
	MaximumAuctions         uint64            `json:"maximum_auctions"`
	UniqueAuctionCount      uint64            `json:"unique_auction_count"`
	ValidArbitrumCount      uint64            `json:"valid_arbitrum_count"`
	RelevantAaveCount       uint64            `json:"relevant_aave_count"`
	FilteredOtherChainCount uint64            `json:"filtered_other_chain_count"`
	DuplicateCount          uint64            `json:"duplicate_count"`
	InvalidCount            uint64            `json:"invalid_count"`
	ReconnectCount          uint64            `json:"reconnect_count"`
	LastObservedAt          *time.Time        `json:"last_observed_at"`
	LastSubscriptionID      string            `json:"last_subscription_id,omitempty"`
	PerFeed                 map[string]uint64 `json:"per_feed"`
	Completed               bool              `json:"completed"`
	CompletionReason        string            `json:"completion_reason,omitempty"`
}

type Ledger struct {
	dir          string
	auctionsPath string
	invalidPath  string
	statePath    string
	mu           sync.Mutex
	state        LedgerState
	seen         map[string]string
}

func OpenLedger(dir string, now time.Time, maximumAuctions uint64, maximumDuration time.Duration) (*Ledger, error) {
	if !filepath.IsAbs(dir) {
		return nil, errors.New("ledger directory must be absolute")
	}
	if maximumAuctions == 0 || maximumDuration <= 0 {
		return nil, errors.New("observation bounds must be positive")
	}
	if err := os.MkdirAll(dir, 0o700); err != nil {
		return nil, fmt.Errorf("create ledger directory: %w", err)
	}
	if err := os.Chmod(dir, 0o700); err != nil {
		return nil, fmt.Errorf("secure ledger directory: %w", err)
	}
	l := &Ledger{
		dir:          dir,
		auctionsPath: filepath.Join(dir, "auctions.ndjson"),
		invalidPath:  filepath.Join(dir, "invalid.ndjson"),
		statePath:    filepath.Join(dir, "state.json"),
		seen:         make(map[string]string),
	}
	if err := l.loadSeen(); err != nil {
		return nil, err
	}
	if err := l.loadOrInitializeState(now.UTC(), maximumAuctions, maximumDuration); err != nil {
		return nil, err
	}
	return l, nil
}

func (l *Ledger) Append(record *LedgerRecord) (bool, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	if digest, exists := l.seen[record.AuctionID]; exists {
		l.state.DuplicateCount++
		if digest != record.NotificationSHA256 {
			l.state.InvalidCount++
			invalid := InvalidRecord{
				ObservedAt:         record.ObservedAt,
				Reason:             "duplicate auction identity changed payload",
				AuctionID:          record.AuctionID,
				NotificationSHA256: record.NotificationSHA256,
			}
			if err := appendJSONLine(l.invalidPath, invalid); err != nil {
				return false, err
			}
		}
		return false, l.persistStateLocked(record.ObservedAt)
	}
	if err := appendJSONLine(l.auctionsPath, record); err != nil {
		return false, err
	}
	l.seen[record.AuctionID] = record.NotificationSHA256
	l.state.UniqueAuctionCount++
	l.state.ValidArbitrumCount++
	l.state.LastSubscriptionID = record.SubscriptionID
	if record.RelevantAaveAuction {
		l.state.RelevantAaveCount++
		if record.OracleUpdate != nil && record.OracleUpdate.Asset != nil {
			l.state.PerFeed[*record.OracleUpdate.Asset]++
		}
	}
	return true, l.persistStateLocked(record.ObservedAt)
}

func (l *Ledger) AppendInvalid(record InvalidRecord) error {
	l.mu.Lock()
	defer l.mu.Unlock()
	if err := appendJSONLine(l.invalidPath, record); err != nil {
		return err
	}
	l.state.InvalidCount++
	return l.persistStateLocked(record.ObservedAt)
}

func (l *Ledger) RecordReconnect(now time.Time) error {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.state.ReconnectCount++
	return l.persistStateLocked(now)
}

func (l *Ledger) RecordFilteredOtherChain(now time.Time) error {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.state.FilteredOtherChainCount++
	return l.persistStateLocked(now)
}

func (l *Ledger) RecordSubscription(now time.Time, subscriptionID string) error {
	if subscriptionID == "" {
		return errors.New("subscription identity is empty")
	}
	l.mu.Lock()
	defer l.mu.Unlock()
	l.state.LastSubscriptionID = subscriptionID
	return l.persistStateLocked(now)
}

func (l *Ledger) Snapshot(now time.Time) LedgerState {
	l.mu.Lock()
	defer l.mu.Unlock()
	l.applyCompletionLocked(now.UTC())
	copy := l.state
	copy.PerFeed = make(map[string]uint64, len(l.state.PerFeed))
	for key, value := range l.state.PerFeed {
		copy.PerFeed[key] = value
	}
	return copy
}

func (l *Ledger) Complete(now time.Time) (bool, error) {
	l.mu.Lock()
	defer l.mu.Unlock()
	changed := l.applyCompletionLocked(now.UTC())
	if changed {
		return true, l.persistStateLocked(now.UTC())
	}
	return l.state.Completed, nil
}

func (l *Ledger) loadSeen() error {
	f, err := secureOpenForRead(l.auctionsPath)
	if errors.Is(err, os.ErrNotExist) {
		return nil
	}
	if err != nil {
		return err
	}
	defer f.Close()
	scanner := bufio.NewScanner(f)
	scanner.Buffer(make([]byte, 64*1024), WebSocketReadLimitBytes*2)
	for scanner.Scan() {
		var record struct {
			AuctionID          string `json:"auction_id"`
			NotificationSHA256 string `json:"notification_sha256"`
		}
		if err := json.Unmarshal(scanner.Bytes(), &record); err != nil {
			return fmt.Errorf("decode existing ledger: %w", err)
		}
		if record.AuctionID == "" || record.NotificationSHA256 == "" {
			return errors.New("existing ledger contains incomplete identity")
		}
		if _, exists := l.seen[record.AuctionID]; exists {
			return errors.New("existing ledger contains duplicate auction identity")
		}
		l.seen[record.AuctionID] = record.NotificationSHA256
	}
	return scanner.Err()
}

func (l *Ledger) loadOrInitializeState(now time.Time, maximumAuctions uint64, maximumDuration time.Duration) error {
	f, err := secureOpenForRead(l.statePath)
	if err == nil {
		defer f.Close()
		if err := json.NewDecoder(f).Decode(&l.state); err != nil {
			return fmt.Errorf("decode ledger state: %w", err)
		}
		if l.state.Schema != StateSchema || l.state.MaximumAuctions != maximumAuctions || !l.state.StopAt.Equal(l.state.StartedAt.Add(maximumDuration)) {
			return errors.New("existing ledger observation contract differs from requested bounds")
		}
		if l.state.UniqueAuctionCount != uint64(len(l.seen)) {
			return errors.New("ledger state count does not match durable auction records")
		}
		return nil
	}
	if !errors.Is(err, os.ErrNotExist) {
		return err
	}
	l.state = LedgerState{
		Schema:          StateSchema,
		StartedAt:       now,
		StopAt:          now.Add(maximumDuration),
		MaximumAuctions: maximumAuctions,
		PerFeed:         make(map[string]uint64),
	}
	return l.persistStateLocked(now)
}

func (l *Ledger) applyCompletionLocked(now time.Time) bool {
	if l.state.Completed {
		return false
	}
	if l.state.UniqueAuctionCount >= l.state.MaximumAuctions {
		l.state.Completed = true
		l.state.CompletionReason = "maximum_auction_count"
		return true
	}
	if !now.Before(l.state.StopAt) {
		l.state.Completed = true
		l.state.CompletionReason = "maximum_observation_duration"
		return true
	}
	return false
}

func (l *Ledger) persistStateLocked(observedAt time.Time) error {
	observedAt = observedAt.UTC()
	l.state.LastObservedAt = &observedAt
	l.applyCompletionLocked(observedAt)
	return writeJSONAtomically(l.statePath, &l.state)
}

func appendJSONLine(path string, value any) error {
	f, err := secureOpenAppend(path)
	if err != nil {
		return err
	}
	encoded, err := json.Marshal(value)
	if err == nil {
		_, err = f.Write(append(encoded, '\n'))
	}
	if syncErr := f.Sync(); err == nil {
		err = syncErr
	}
	if closeErr := f.Close(); err == nil {
		err = closeErr
	}
	return err
}

func secureOpenAppend(path string) (*os.File, error) {
	if info, err := os.Lstat(path); err == nil {
		if !info.Mode().IsRegular() || info.Mode()&os.ModeSymlink != 0 {
			return nil, errors.New("ledger target is not a regular file")
		}
	} else if !errors.Is(err, os.ErrNotExist) {
		return nil, err
	}
	f, err := os.OpenFile(path, os.O_CREATE|os.O_APPEND|os.O_WRONLY, 0o600)
	if err != nil {
		return nil, err
	}
	if err := f.Chmod(0o600); err != nil {
		f.Close()
		return nil, err
	}
	return f, nil
}

func secureOpenForRead(path string) (*os.File, error) {
	info, err := os.Lstat(path)
	if err != nil {
		return nil, err
	}
	if !info.Mode().IsRegular() || info.Mode()&os.ModeSymlink != 0 {
		return nil, errors.New("ledger source is not a regular file")
	}
	return os.Open(path)
}

func writeJSONAtomically(path string, value any) error {
	dir := filepath.Dir(path)
	tmp, err := os.CreateTemp(dir, ".state-*.tmp")
	if err != nil {
		return err
	}
	if err := tmp.Chmod(0o600); err != nil {
		tmp.Close()
		os.Remove(tmp.Name())
		return err
	}
	cleanup := func() {
		tmp.Close()
		os.Remove(tmp.Name())
	}
	encoder := json.NewEncoder(tmp)
	encoder.SetIndent("", "  ")
	if err := encoder.Encode(value); err != nil {
		cleanup()
		return err
	}
	if err := tmp.Sync(); err != nil {
		cleanup()
		return err
	}
	if err := tmp.Close(); err != nil {
		os.Remove(tmp.Name())
		return err
	}
	if err := os.Rename(tmp.Name(), path); err != nil {
		os.Remove(tmp.Name())
		return err
	}
	return nil
}

func decodeOneJSONLine(reader io.Reader, target any) error {
	return json.NewDecoder(reader).Decode(target)
}
