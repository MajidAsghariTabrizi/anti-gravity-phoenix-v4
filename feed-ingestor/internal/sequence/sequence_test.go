package sequence

import "testing"

func TestStateStartupBaselineContiguousDuplicateGapAndRegression(t *testing.T) {
	state := New()

	first := state.Observe(0, false)
	if first.Event != FirstMessage || !first.Publishable {
		t.Fatalf("unexpected first result: %+v", first)
	}
	inOrder := state.Observe(1, false)
	if inOrder.Event != InOrder || !inOrder.Publishable {
		t.Fatalf("unexpected in-order result: %+v", inOrder)
	}
	duplicate := state.Observe(1, false)
	if duplicate.Event != Duplicate || duplicate.Publishable {
		t.Fatalf("unexpected duplicate result: %+v", duplicate)
	}
	gap := state.Observe(4, false)
	if gap.Event != Gap || gap.GapFrom != 2 || gap.GapTo != 3 || gap.Missing != 2 || !gap.Publishable {
		t.Fatalf("unexpected gap result: %+v", gap)
	}
	if !state.HasUnresolvedGap() {
		t.Fatal("expected unresolved gap")
	}
	if last, ok := state.LastSequence(); !ok || last != 4 || state.NextExpected() != 5 {
		t.Fatalf("gap did not advance the baseline: last=%d ok=%t next=%d", last, ok, state.NextExpected())
	}
	if got := state.Observe(5, false); got.Event != InOrder || !got.Publishable {
		t.Fatalf("continued traffic after the gap was not accepted: %+v", got)
	}
	if state.HasUnresolvedGap() {
		t.Fatal("contiguous traffic should re-establish continuity")
	}
	regression := state.Observe(3, false)
	if regression.Event != Regression || regression.Publishable {
		t.Fatalf("older sequence should be a regression: %+v", regression)
	}
}

func TestReconnectUsesExistingExpectedBaseline(t *testing.T) {
	state := New()
	if got := state.Observe(41, false); got.Event != FirstMessage {
		t.Fatalf("unexpected first result: %+v", got)
	}
	continued := state.Observe(42, true)
	if continued.Event != Reconnect || !continued.Publishable {
		t.Fatalf("unexpected reconnect continuation: %+v", continued)
	}
	discontinuity := state.Observe(45, true)
	if discontinuity.Event != Gap || discontinuity.GapFrom != 43 || discontinuity.GapTo != 44 || discontinuity.Missing != 2 || !discontinuity.Reconnected || !discontinuity.Publishable {
		t.Fatalf("unexpected reconnect discontinuity: %+v", discontinuity)
	}
	if got := state.Observe(46, false); got.Event != InOrder || !got.Publishable {
		t.Fatalf("reconnect gap did not establish a new baseline: %+v", got)
	}
	regression := state.Observe(40, true)
	if regression.Event != Regression || !regression.Reconnected || regression.Publishable {
		t.Fatalf("unexpected reconnect regression result: %+v", regression)
	}
}

func TestReconnectBeforeFirstAcceptedMessageEstablishesExplicitStartupBaseline(t *testing.T) {
	state := New()
	result := state.Observe(900, true)
	if result.Event != FirstMessage || !result.Reconnected || !result.Publishable {
		t.Fatalf("unexpected reconnect startup baseline: %+v", result)
	}
	if state.NextExpected() != 901 {
		t.Fatalf("unexpected next sequence after reconnect baseline: %d", state.NextExpected())
	}
}
