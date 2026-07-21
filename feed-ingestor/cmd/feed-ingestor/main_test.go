package main

import (
	"bytes"
	"context"
	"errors"
	"io"
	"log"
	"os"
	"path/filepath"
	"reflect"
	"strings"
	"testing"
	"time"

	"anti-gravity-phoenix-v4/feed-ingestor/internal/feed"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/metrics"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/nitro"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/normalizer"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/publisher"
	"anti-gravity-phoenix-v4/feed-ingestor/internal/sequence"
)

func TestResolveSourceConfigBlocksProductionFixture(t *testing.T) {
	_, err := resolveSourceConfig(mapEnv(map[string]string{
		"PHOENIX_ENV":          "production",
		"PHOENIX_FEED_FIXTURE": "/app/fixtures/feed/profitable.ndjson",
	}))
	if err == nil {
		t.Fatal("expected production fixture source to fail")
	}
}

func TestResolveSourceConfigAllowsProductionRelay(t *testing.T) {
	cfg, err := resolveSourceConfig(mapEnv(map[string]string{
		"PHOENIX_ENV":            "production",
		"PHOENIX_FEED_SOURCE":    "relay",
		"PHOENIX_FEED_RELAY_URL": "ws://nitro-feed-relay:9642/feed",
	}))
	if err != nil {
		t.Fatalf("unexpected production relay error: %v", err)
	}
	if cfg.kind != "relay" || cfg.relayURL != "ws://nitro-feed-relay:9642/feed" {
		t.Fatalf("unexpected config: %+v", cfg)
	}
}

func TestResolveSourceConfigAllowsShadowFixture(t *testing.T) {
	cfg, err := resolveSourceConfig(mapEnv(map[string]string{
		"PHOENIX_FEED_FIXTURE": "fixtures/feed/profitable.ndjson",
	}))
	if err != nil {
		t.Fatalf("unexpected fixture error: %v", err)
	}
	if cfg.kind != "fixture" || cfg.fixturePath != "fixtures/feed/profitable.ndjson" {
		t.Fatalf("unexpected config: %+v", cfg)
	}
}

func TestResolveSourceConfigAllowsShadowRelay(t *testing.T) {
	cfg, err := resolveSourceConfig(mapEnv(map[string]string{
		"PHOENIX_FEED_SOURCE":    "relay",
		"PHOENIX_FEED_RELAY_URL": "ws://nitro-feed-relay:9642/feed",
	}))
	if err != nil {
		t.Fatalf("unexpected relay error: %v", err)
	}
	if cfg.kind != "relay" || cfg.relayURL != "ws://nitro-feed-relay:9642/feed" {
		t.Fatalf("unexpected config: %+v", cfg)
	}
}

func TestPublishTransactionsCountsFailure(t *testing.T) {
	registry := metrics.NewRegistry()
	published, err := publishTransactions(context.Background(), failingPublisher{}, registry, []normalizer.NormalizedTx{{TxHash: "0xabc"}}, time.Now())
	if err == nil {
		t.Fatal("expected publish failure")
	}
	if published != 0 {
		t.Fatalf("failed publish counted as successful: %d", published)
	}
	rendered := registry.Render()
	if !strings.Contains(rendered, "feed_publish_failures_total 1") {
		t.Fatalf("missing publish failure counter: %s", rendered)
	}
	if !strings.Contains(rendered, "feed_publish_success_total 0") {
		t.Fatalf("unexpected publish success counter: %s", rendered)
	}
}

func TestPublishTransactionsDoesNotAdvancePastUnprovenSourceItem(t *testing.T) {
	registry := metrics.NewRegistry()
	pub := &orderedFailingPublisher{}
	transactions := []normalizer.NormalizedTx{
		{Sequence: 100, TxHash: "0xaaa"},
		{Sequence: 101, TxHash: "0xbbb"},
	}
	published, err := publishTransactions(context.Background(), pub, registry, transactions, time.Now())
	if !errors.Is(err, publisher.ErrPublishAckTimeout) {
		t.Fatalf("expected fail-closed acknowledgement timeout, got %v", err)
	}
	if published != 0 || !reflect.DeepEqual(pub.messageIDs, []string{transactions[0].DurableMessageID()}) {
		t.Fatalf("source advanced past an unproven publication: published=%d ids=%v", published, pub.messageIDs)
	}
	rendered := registry.Render()
	if !strings.Contains(rendered, "feed_publish_success_total 0") || !strings.Contains(rendered, "feed_publish_failures_total 1") {
		t.Fatalf("fail-closed publication metrics are incorrect: %s", rendered)
	}
}

func TestJetStreamPublishStateObservabilityIsBoundedAndRedacted(t *testing.T) {
	registry := metrics.NewRegistry()
	var output bytes.Buffer
	handler := jetStreamPublishStateHandler(registry, log.New(&output, "", 0))
	for _, event := range []publisher.PublishEvent{
		{Kind: publisher.PublishEventAckTimeoutRetry, Subject: txSubject, MessageID: "10:0xaaa", Attempt: 1, MaxAttempts: 3, Elapsed: time.Second},
		{Kind: publisher.PublishEventRecoveredNormal, Subject: txSubject, MessageID: "11:0xbbb", Attempt: 2, MaxAttempts: 3, Elapsed: 2 * time.Second},
		{Kind: publisher.PublishEventRecoveredDuplicate, Subject: txSubject, MessageID: "12:0xccc", Attempt: 2, MaxAttempts: 3, Elapsed: 2 * time.Second},
		{Kind: publisher.PublishEventRetryExhausted, Subject: txSubject, MessageID: "13:0xddd", Attempt: 3, MaxAttempts: 3, Elapsed: 7 * time.Second},
	} {
		handler(event)
	}
	rendered := registry.Render()
	for _, expected := range []string{
		"feed_jetstream_publish_ack_timeout_retries_total 1",
		"feed_jetstream_publish_recovered_normal_total 1",
		"feed_jetstream_publish_recovered_duplicate_total 1",
		"feed_jetstream_publish_retry_exhausted_total 1",
	} {
		if !strings.Contains(rendered, expected) {
			t.Fatalf("missing publish recovery metric %q: %s", expected, rendered)
		}
	}
	logs := output.String()
	for _, expected := range []string{
		"feed_jetstream_publish_ack_timeout_retry",
		"acknowledgement=normal",
		"acknowledgement=duplicate",
		"action=fail_closed",
	} {
		if !strings.Contains(logs, expected) {
			t.Fatalf("missing publish recovery log %q: %s", expected, logs)
		}
	}
	for _, forbidden := range []string{"private_key", "password=", "postgres://", "raw_tx", "signed"} {
		if strings.Contains(strings.ToLower(logs), forbidden) {
			t.Fatalf("publish recovery log exposed restricted data %q: %s", forbidden, logs)
		}
	}
}

func TestNormalizeRelayTransactionsRejectsUnsupportedChain(t *testing.T) {
	normalized, failures := normalizeRelayTransactions(nitroFrameWithChain(1))
	if len(normalized) != 0 || len(failures) != 1 || !strings.Contains(failures[0], "unsupported chain id") {
		t.Fatalf("expected unsupported chain to fail normalization: normalized=%+v failures=%+v", normalized, failures)
	}
}

func TestNormalizeRelayTransactionsKeepsValidSibling(t *testing.T) {
	valid := nitroFrameWithChain(nitro.ArbitrumOneChainID).Transactions[0]
	invalid := nitroFrameWithChain(1).Transactions[0]
	frame := nitro.Frame{Sequence: 1, TimestampUnixMS: 1700000000000, Transactions: []normalizer.RelayTx{valid, invalid}}
	normalized, failures := normalizeRelayTransactions(frame)
	if len(normalized) != 1 || len(failures) != 1 {
		t.Fatalf("expected one valid sibling and one failure: normalized=%+v failures=%+v", normalized, failures)
	}
}

func TestRelayLifecycleCountsReconnectsWithoutClaimingReadiness(t *testing.T) {
	registry := metrics.NewRegistry()
	readiness := &metrics.Readiness{}
	readiness.MarkSourceInitialized()
	readiness.MarkAdapterInitialized()
	readiness.MarkNATSReachable()
	handle := relayLifecycleHandler(registry, readiness)

	handle(feed.RelayEvent{Kind: feed.RelayEventConnected, Attempt: 1})
	handle(feed.RelayEvent{Kind: feed.RelayEventDisconnected})
	handle(feed.RelayEvent{Kind: feed.RelayEventReconnectAttempt, Attempt: 2, Backoff: time.Millisecond})
	handle(feed.RelayEvent{Kind: feed.RelayEventConnected, Attempt: 2, Reconnected: true})

	rendered := registry.Render()
	if !strings.Contains(rendered, "feed_connections_total 2") {
		t.Fatalf("connection metric does not reflect successful handshakes: %s", rendered)
	}
	if !strings.Contains(rendered, "feed_reconnects_total 1") {
		t.Fatalf("reconnect metric does not reflect retry attempts: %s", rendered)
	}
	if !strings.Contains(rendered, "feed_messages_total 0") || !strings.Contains(rendered, "feed_decode_failures_total 0") {
		t.Fatalf("lifecycle events must not count as delivered or rejected messages: %s", rendered)
	}
	if ok, reason := readiness.Ready(); ok || reason != "no successful feed transaction published" {
		t.Fatalf("readiness must remain false before valid live evidence, ok=%v reason=%q", ok, reason)
	}
}

func TestRelayForwardGapIsCountedOnceAdvancesAndReadinessRecovers(t *testing.T) {
	registry := metrics.NewRegistry()
	readiness := relayDependencyReadyState()
	state := sequence.New()
	pub := &publisher.MemoryPublisher{}
	issueLogger := newSampledIssueLogger(log.New(io.Discard, "", 0), time.Minute, nil)

	processRelayFrameForTest(t, relayFrame(100, 'a', 1), false, state, pub, registry, readiness, issueLogger)
	if ok, reason := readiness.Ready(); !ok || reason != "ready" {
		t.Fatalf("initial live evidence was not ready ok=%v reason=%q", ok, reason)
	}
	processRelayFrameForTest(t, relayFrame(103, 'b', 1), false, state, pub, registry, readiness, issueLogger)
	if ok, reason := readiness.Ready(); ok || reason != "unresolved feed sequence gap" {
		t.Fatalf("gap should make readiness transiently false ok=%v reason=%q", ok, reason)
	}
	processRelayFrameForTest(t, relayFrame(104, 'c', 1), false, state, pub, registry, readiness, issueLogger)
	if ok, reason := readiness.Ready(); !ok || reason != "ready" {
		t.Fatalf("contiguous traffic did not recover readiness ok=%v reason=%q", ok, reason)
	}

	last, haveLast := state.LastSequence()
	if !haveLast || last != 104 || len(pub.Messages) != 3 {
		t.Fatalf("gap baseline or publication mismatch last=%d have=%t published=%d", last, haveLast, len(pub.Messages))
	}
	rendered := registry.Render()
	for _, expected := range []string{
		"feed_sequence_gaps_total 1",
		"feed_sequence_gap_messages_total 2",
		"feed_missing_sequences_total 2",
		"feed_decode_failures_total 0",
		"feed_data_completeness 1",
		"feed_readiness 1",
	} {
		if !strings.Contains(rendered, expected) {
			t.Fatalf("gap metrics missing %q: %s", expected, rendered)
		}
	}
	if strings.Contains(rendered, "feed_last_gap_timestamp_seconds 0") {
		t.Fatalf("gap timestamp was not recorded: %s", rendered)
	}
}

func TestRelayPublishFailureDoesNotCommitSourceSequence(t *testing.T) {
	registry := metrics.NewRegistry()
	readiness := relayDependencyReadyState()
	state := sequence.New()
	pub := &orderedFailingPublisher{}
	issueLogger := newSampledIssueLogger(log.New(io.Discard, "", 0), time.Minute, nil)
	frame := relayFrame(250, 'a', 1)

	err := processRelayFrame(
		context.Background(),
		frame,
		false,
		state,
		pub,
		registry,
		readiness,
		issueLogger,
		time.Now(),
	)
	if !errors.Is(err, publisher.ErrPublishAckTimeout) {
		t.Fatalf("expected fail-closed publish timeout, got %v", err)
	}
	if last, haveLast := state.LastSequence(); haveLast || last != 0 || state.NextExpected() != 0 {
		t.Fatalf("failed publication committed source sequence: last=%d have=%t next=%d", last, haveLast, state.NextExpected())
	}
	if len(pub.messageIDs) != 1 || pub.messageIDs[0] != "250:"+frame.Transactions[0].Hash {
		t.Fatalf("unexpected attempted publication identity: %v", pub.messageIDs)
	}
	rendered := registry.Render()
	for _, expected := range []string{
		"feed_last_sequence 0",
		"feed_publish_success_total 0",
		"feed_publish_failures_total 1",
		"feed_readiness 0",
	} {
		if !strings.Contains(rendered, expected) {
			t.Fatalf("uncommitted source metrics missing %q: %s", expected, rendered)
		}
	}
}

func TestReconnectBaselineSurvivesControlBroadcastWithoutFeedFrames(t *testing.T) {
	reconnect := reconnectBaseline{}
	reconnect.ObserveMessage(true)
	reconnect.ObserveMessage(false)
	if !reconnect.ConsumeForFrame() {
		t.Fatal("confirmation-only broadcast consumed reconnect baseline")
	}
	if reconnect.ConsumeForFrame() {
		t.Fatal("reconnect baseline was applied to more than one feed message")
	}
}

func TestRelayZeroOutputAndMultiOutputEnvelopesTrackSequenceOnce(t *testing.T) {
	registry := metrics.NewRegistry()
	readiness := relayDependencyReadyState()
	state := sequence.New()
	pub := &publisher.MemoryPublisher{}
	issueLogger := newSampledIssueLogger(log.New(io.Discard, "", 0), time.Minute, nil)

	processRelayFrameForTest(t, relayFrame(500, 'a', 0), false, state, pub, registry, readiness, issueLogger)
	if len(pub.Messages) != 0 || state.NextExpected() != 501 {
		t.Fatalf("zero-output envelope did not advance exactly once published=%d next=%d", len(pub.Messages), state.NextExpected())
	}
	processRelayFrameForTest(t, relayFrame(501, 'b', 2), false, state, pub, registry, readiness, issueLogger)
	if len(pub.Messages) != 2 || state.NextExpected() != 502 {
		t.Fatalf("multi-output envelope changed sequence per transaction published=%d next=%d", len(pub.Messages), state.NextExpected())
	}
	rendered := registry.Render()
	if !strings.Contains(rendered, "feed_normalized_transactions_total 2") || !strings.Contains(rendered, "feed_sequence_gaps_total 0") {
		t.Fatalf("zero/multi-output metrics mismatch: %s", rendered)
	}
}

func TestRelayDuplicateAndRegressionAreDistinctAndRegressionIsTerminal(t *testing.T) {
	registry := metrics.NewRegistry()
	readiness := relayDependencyReadyState()
	state := sequence.New()
	pub := &publisher.MemoryPublisher{}
	issueLogger := newSampledIssueLogger(log.New(io.Discard, "", 0), time.Minute, nil)

	processRelayFrameForTest(t, relayFrame(700, 'a', 1), false, state, pub, registry, readiness, issueLogger)
	processRelayFrameForTest(t, relayFrame(700, 'a', 1), true, state, pub, registry, readiness, issueLogger)
	processRelayFrameForTest(t, relayFrame(699, 'b', 0), true, state, pub, registry, readiness, issueLogger)

	if len(pub.Messages) != 1 {
		t.Fatalf("duplicate or regression was published: %d", len(pub.Messages))
	}
	if ok, reason := readiness.Ready(); ok || reason != "Nitro feed sequence regression" {
		t.Fatalf("regression did not latch terminal readiness ok=%v reason=%q", ok, reason)
	}
	rendered := registry.Render()
	if !strings.Contains(rendered, "feed_sequence_duplicates_total 1") || !strings.Contains(rendered, "feed_sequence_regressions_total 1") || !strings.Contains(rendered, "feed_sequence_gaps_total 0") {
		t.Fatalf("sequence classes were conflated: %s", rendered)
	}
}

func TestRelayDecodeCorruptionIsNotASequenceMetricAndIsTerminal(t *testing.T) {
	registry := metrics.NewRegistry()
	readiness := publishReadyState()
	issueLogger := newSampledIssueLogger(log.New(io.Discard, "", 0), time.Minute, nil)
	_, _, err := nitro.DecodeBroadcast([]byte(`{"version":`))
	if err == nil {
		t.Fatal("expected a real Nitro broadcast decode failure")
	}
	recordRelayDecodeFailure(registry, readiness, issueLogger, err)
	if ok, reason := readiness.Ready(); ok || reason != "Nitro broadcast decoding integrity failure" {
		t.Fatalf("decode corruption did not fail readiness ok=%v reason=%q", ok, reason)
	}
	rendered := registry.Render()
	if !strings.Contains(rendered, "feed_decode_failures_total 1") || !strings.Contains(rendered, "feed_sequence_gaps_total 0") || !strings.Contains(rendered, "feed_sequence_regressions_total 0") {
		t.Fatalf("decode and sequence metrics were conflated: %s", rendered)
	}
}

func TestCanonicalNumericBaseFeePreservesReadinessSequenceAndPublication(t *testing.T) {
	registry := metrics.NewRegistry()
	readiness := relayDependencyReadyState()
	state := sequence.New()
	pub := &publisher.MemoryPublisher{}
	var output bytes.Buffer
	issueLogger := newSampledIssueLogger(log.New(&output, "", 0), time.Minute, nil)

	raw := numericBaseFeeFixture(t)
	for _, sequenceNumber := range []string{"460530858", "460530859"} {
		candidate := bytes.Replace(raw, []byte("460530858"), []byte(sequenceNumber), 1)
		frames, report, err := nitro.DecodeBroadcast(candidate)
		if err != nil {
			t.Fatalf("decode canonical numeric baseFeeL1: %v", err)
		}
		if len(frames) != 1 || len(report.Malformed) != 0 {
			t.Fatalf("unexpected numeric baseFeeL1 result: frames=%+v report=%+v", frames, report)
		}
		processRelayFrameForTest(t, frames[0], false, state, pub, registry, readiness, issueLogger)
	}

	last, haveLast := state.LastSequence()
	if !haveLast || last != 460530859 || len(pub.Messages) != 2 {
		t.Fatalf("numeric baseFeeL1 interrupted sequence or publication last=%d have=%t published=%d", last, haveLast, len(pub.Messages))
	}
	if ok, reason := readiness.Ready(); !ok || reason != "ready" {
		t.Fatalf("numeric baseFeeL1 cleared readiness ok=%v reason=%q", ok, reason)
	}
	rendered := registry.Render()
	for _, expected := range []string{
		"feed_decode_failures_total 0",
		"feed_sequence_gaps_total 0",
		"feed_jetstream_publish_success_total 2",
		"feed_readiness 1",
	} {
		if !strings.Contains(rendered, expected) {
			t.Fatalf("numeric baseFeeL1 metrics missing %q: %s", expected, rendered)
		}
	}
	if strings.Contains(output.String(), "event=feed_sequence_event") {
		t.Fatalf("contiguous numeric baseFeeL1 produced sequence logs: %s", output.String())
	}
}

func TestMalformedBaseFeeFailsClosedWithoutPublishingOrLoggingPayload(t *testing.T) {
	registry := metrics.NewRegistry()
	readiness := publishReadyState()
	readiness.MarkSuccessfulPublish()
	state := sequence.New()
	pub := &publisher.MemoryPublisher{}
	var output bytes.Buffer
	issueLogger := newSampledIssueLogger(log.New(&output, "", 0), time.Minute, nil)
	raw := bytes.Replace(numericBaseFeeFixture(t), []byte("9007199254740993"), []byte("1.5"), 1)

	frames, _, err := nitro.DecodeBroadcast(raw)
	if err != nil {
		recordRelayDecodeFailure(registry, readiness, issueLogger, err)
	} else {
		for _, frame := range frames {
			processRelayFrameForTest(t, frame, false, state, pub, registry, readiness, issueLogger)
		}
	}
	if err == nil {
		t.Fatal("accepted malformed baseFeeL1")
	}

	if len(pub.Messages) != 0 {
		t.Fatalf("malformed baseFeeL1 was published: %d", len(pub.Messages))
	}
	if ok, reason := readiness.Ready(); ok || reason != "Nitro broadcast decoding integrity failure" {
		t.Fatalf("malformed baseFeeL1 did not fail closed ok=%v reason=%q", ok, reason)
	}
	if !strings.Contains(registry.Render(), "feed_decode_failures_total 1") {
		t.Fatalf("malformed baseFeeL1 was not counted: %s", registry.Render())
	}
	for _, forbidden := range []string{"BAL4ZoK", "l2Msg", "raw_tx", "signatureV2"} {
		if strings.Contains(output.String(), forbidden) {
			t.Fatalf("malformed baseFeeL1 log exposed payload %q: %s", forbidden, output.String())
		}
	}
}

func TestUnsupportedRelayMessageIsObservableWithoutFailingReadiness(t *testing.T) {
	registry := metrics.NewRegistry()
	readiness := relayDependencyReadyState()
	state := sequence.New()
	pub := &publisher.MemoryPublisher{}
	issueLogger := newSampledIssueLogger(log.New(io.Discard, "", 0), time.Minute, nil)

	processRelayFrameForTest(t, relayFrame(800, 'a', 1), false, state, pub, registry, readiness, issueLogger)
	unsupported := relayFrame(801, 'b', 0)
	unsupported.Unsupported = []string{"unsupported L2 message kind 0x7f"}
	processRelayFrameForTest(t, unsupported, false, state, pub, registry, readiness, issueLogger)
	if ok, reason := readiness.Ready(); !ok || reason != "ready" {
		t.Fatalf("unsupported message failed readiness ok=%v reason=%q", ok, reason)
	}
	if !strings.Contains(registry.Render(), "feed_unsupported_messages_total 1") {
		t.Fatalf("unsupported message was not counted: %s", registry.Render())
	}
}

func TestReadinessBecomesTrueOnlyAfterSuccessfulPublish(t *testing.T) {
	registry := metrics.NewRegistry()
	readiness := publishReadyState()
	pub := &publisher.MemoryPublisher{}
	transactions := []normalizer.NormalizedTx{{TxHash: "0xabc"}}

	if ok, _ := readiness.Ready(); ok {
		t.Fatal("readiness was true before publication")
	}
	if err := publishAndUpdateReadiness(context.Background(), pub, registry, readiness, transactions, time.Now()); err != nil {
		t.Fatal(err)
	}
	if ok, reason := readiness.Ready(); !ok || reason != "ready" {
		t.Fatalf("successful publish did not enable readiness ok=%v reason=%q", ok, reason)
	}
	rendered := registry.Render()
	if !strings.Contains(rendered, "feed_publish_success_total 1") || !strings.Contains(rendered, "feed_jetstream_publish_success_total 1") || !strings.Contains(rendered, "feed_readiness 1") {
		t.Fatalf("successful publish metrics mismatch: %s", rendered)
	}
}

func TestPublishFailureRemainsFailClosed(t *testing.T) {
	registry := metrics.NewRegistry()
	readiness := publishReadyState()
	transactions := []normalizer.NormalizedTx{{TxHash: "0xabc"}}

	if err := publishAndUpdateReadiness(context.Background(), failingPublisher{}, registry, readiness, transactions, time.Now()); err == nil {
		t.Fatal("expected publish failure")
	}
	if ok, _ := readiness.Ready(); ok {
		t.Fatal("publish failure enabled readiness")
	}
	rendered := registry.Render()
	if !strings.Contains(rendered, "feed_publish_failures_total 1") || !strings.Contains(rendered, "feed_jetstream_publish_failures_total 1") || !strings.Contains(rendered, "feed_publish_success_total 0") || !strings.Contains(rendered, "feed_readiness 0") {
		t.Fatalf("failed publish metrics mismatch: %s", rendered)
	}
}

type failingPublisher struct{}

func (failingPublisher) Publish(context.Context, string, any) error {
	return errors.New("publish failed")
}
func (failingPublisher) Close() error { return nil }

type orderedFailingPublisher struct {
	messageIDs []string
}

func (p *orderedFailingPublisher) Publish(_ context.Context, _ string, value any) error {
	identified, ok := value.(interface{ DurableMessageID() string })
	if !ok {
		return errors.New("missing durable identity")
	}
	p.messageIDs = append(p.messageIDs, identified.DurableMessageID())
	return publisher.ErrPublishAckTimeout
}

func (*orderedFailingPublisher) Close() error { return nil }

func publishReadyState() *metrics.Readiness {
	readiness := &metrics.Readiness{}
	readiness.MarkSourceInitialized()
	readiness.MarkAdapterInitialized()
	readiness.MarkSourceConnected()
	readiness.MarkNATSReachable()
	readiness.MarkSequenceKnown()
	return readiness
}

func relayDependencyReadyState() *metrics.Readiness {
	readiness := &metrics.Readiness{}
	readiness.MarkSourceInitialized()
	readiness.MarkAdapterInitialized()
	readiness.MarkSourceConnected()
	readiness.MarkNATSReachable()
	return readiness
}

func processRelayFrameForTest(
	t *testing.T,
	frame nitro.Frame,
	afterReconnect bool,
	state *sequence.State,
	pub publisher.Publisher,
	registry *metrics.Registry,
	readiness *metrics.Readiness,
	issueLogger *sampledIssueLogger,
) {
	t.Helper()
	if err := processRelayFrame(
		context.Background(),
		frame,
		afterReconnect,
		state,
		pub,
		registry,
		readiness,
		issueLogger,
		time.Now(),
	); err != nil {
		t.Fatalf("process relay frame: %v", err)
	}
}

func relayFrame(sequenceNumber uint64, hashByte byte, transactionCount int) nitro.Frame {
	frame := nitroFrameWithChain(nitro.ArbitrumOneChainID)
	frame.Sequence = sequenceNumber
	frame.Transactions = nil
	for index := range transactionCount {
		tx := nitroFrameWithChain(nitro.ArbitrumOneChainID).Transactions[0]
		tx.Hash = "0x" + strings.Repeat(string(hashByte+byte(index)), 64)
		tx.Nonce = uint64(index)
		frame.Transactions = append(frame.Transactions, tx)
	}
	return frame
}

func numericBaseFeeFixture(t *testing.T) []byte {
	t.Helper()
	path := filepath.Join("..", "..", "internal", "nitro", "testdata", "numeric_base_fee_l1.json")
	raw, err := os.ReadFile(path)
	if err != nil {
		t.Fatalf("read numeric baseFeeL1 fixture: %v", err)
	}
	return raw
}

func nitroFrameWithChain(chainID uint64) nitro.Frame {
	return nitro.Frame{
		Sequence:        1,
		TimestampUnixMS: 1700000000000,
		Transactions: []normalizer.RelayTx{
			{
				Hash:                 "0xaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa",
				Type:                 "0x65",
				ChainID:              chainID,
				From:                 "0x1111111111111111111111111111111111111111",
				To:                   "0x2222222222222222222222222222222222222222",
				Nonce:                7,
				Value:                "0",
				Calldata:             "0x1234",
				GasLimit:             "21000",
				MaxFeePerGas:         "100",
				MaxPriorityFeePerGas: "0",
				RawTx:                "0x0102",
			},
		},
	}
}

func mapEnv(values map[string]string) func(string) string {
	return func(key string) string {
		return values[key]
	}
}
