package sequence

import "testing"

func TestStateFirstInOrderDuplicateGapAndOutOfOrder(t *testing.T) {
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
	if gap.Event != Gap || gap.GapFrom != 2 || gap.GapTo != 3 || gap.Publishable {
		t.Fatalf("unexpected gap result: %+v", gap)
	}
	if !state.HasUnresolvedGap() {
		t.Fatal("expected unresolved gap")
	}
	if got := state.Observe(2, false); got.Event != InOrder || !got.Publishable {
		t.Fatalf("expected gap start to be accepted in order: %+v", got)
	}
	if !state.HasUnresolvedGap() {
		t.Fatal("gap should remain unresolved until the full range is accepted")
	}
	if got := state.Observe(3, false); got.Event != InOrder || !got.Publishable {
		t.Fatalf("expected gap end to be accepted in order: %+v", got)
	}
	if state.HasUnresolvedGap() {
		t.Fatal("expected gap to be resolved")
	}
	duplicateOlder := state.Observe(0, false)
	if duplicateOlder.Event != Duplicate || duplicateOlder.Publishable {
		t.Fatalf("seen older sequence should be duplicate: %+v", duplicateOlder)
	}
	newState := New()
	if got := newState.Observe(10, false); got.Event != FirstMessage || !got.Publishable {
		t.Fatalf("unexpected first result: %+v", got)
	}
	outOfOrder := newState.Observe(8, false)
	if outOfOrder.Event != OutOfOrder || outOfOrder.Publishable {
		t.Fatalf("unexpected out-of-order result: %+v", outOfOrder)
	}
}

func TestReconnectContinuesWithoutReset(t *testing.T) {
	state := New()
	if got := state.Observe(41, false); got.Event != FirstMessage {
		t.Fatalf("unexpected first result: %+v", got)
	}
	continued := state.Observe(42, true)
	if continued.Event != Reconnect || !continued.Publishable {
		t.Fatalf("unexpected reconnect continuation: %+v", continued)
	}
	discontinuity := state.Observe(45, true)
	if discontinuity.Event != Gap || discontinuity.GapFrom != 43 || discontinuity.GapTo != 44 || discontinuity.Publishable {
		t.Fatalf("unexpected reconnect discontinuity: %+v", discontinuity)
	}
	reset := state.Observe(40, true)
	if reset.Event != FeedReset || reset.Publishable {
		t.Fatalf("unexpected reconnect reset result: %+v", reset)
	}
}
