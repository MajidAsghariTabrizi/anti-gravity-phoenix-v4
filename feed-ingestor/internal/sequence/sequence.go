package sequence

type Event string

const (
	FirstMessage Event = "FIRST_MESSAGE"
	InOrder      Event = "IN_ORDER"
	Duplicate    Event = "DUPLICATE"
	Gap          Event = "GAP"
	OutOfOrder   Event = "OUT_OF_ORDER"
	Reconnect    Event = "RECONNECT"
	FeedReset    Event = "FEED_RESET"
)

type Result struct {
	Event       Event
	Sequence    uint64
	GapFrom     uint64
	GapTo       uint64
	Publishable bool
}

type State struct {
	lastSequence uint64
	haveLast     bool
	gapEnd       uint64
	haveGap      bool
	seen         map[uint64]struct{}
}

func New() *State {
	return &State{seen: make(map[uint64]struct{})}
}

func (s *State) Observe(sequence uint64, afterReconnect bool) Result {
	if _, ok := s.seen[sequence]; ok {
		return Result{Event: Duplicate, Sequence: sequence}
	}
	if !s.haveLast {
		s.accept(sequence)
		return Result{Event: FirstMessage, Sequence: sequence, Publishable: true}
	}

	expected := s.lastSequence + 1
	if sequence == expected {
		s.accept(sequence)
		if afterReconnect {
			return Result{Event: Reconnect, Sequence: sequence, Publishable: true}
		}
		return Result{Event: InOrder, Sequence: sequence, Publishable: true}
	}
	if sequence > expected {
		if !s.haveGap || sequence-1 > s.gapEnd {
			s.gapEnd = sequence - 1
			s.haveGap = true
		}
		return Result{
			Event:    Gap,
			Sequence: sequence,
			GapFrom:  expected,
			GapTo:    sequence - 1,
		}
	}
	if afterReconnect {
		return Result{Event: FeedReset, Sequence: sequence}
	}
	return Result{Event: OutOfOrder, Sequence: sequence}
}

func (s *State) NextExpected() uint64 {
	if !s.haveLast {
		return 0
	}
	return s.lastSequence + 1
}

func (s *State) LastSequence() (uint64, bool) {
	return s.lastSequence, s.haveLast
}

func (s *State) HasUnresolvedGap() bool {
	return s.haveGap
}

func (s *State) accept(sequence uint64) {
	s.seen[sequence] = struct{}{}
	s.lastSequence = sequence
	s.haveLast = true
	if s.haveGap && s.lastSequence >= s.gapEnd {
		s.haveGap = false
		s.gapEnd = 0
	}
}
