package sequence

type Event string

const (
	FirstMessage Event = "FIRST_MESSAGE"
	InOrder      Event = "IN_ORDER"
	Duplicate    Event = "DUPLICATE"
	Gap          Event = "GAP"
	Regression   Event = "REGRESSION"
	Reconnect    Event = "RECONNECT"
)

type Result struct {
	Event       Event
	Sequence    uint64
	GapFrom     uint64
	GapTo       uint64
	Missing     uint64
	Reconnected bool
	Publishable bool
}

type State struct {
	lastSequence  uint64
	haveLast      bool
	unresolvedGap bool
}

func New() *State {
	return &State{}
}

func (s *State) Observe(sequence uint64, afterReconnect bool) Result {
	if !s.haveLast {
		s.accept(sequence, false)
		return Result{
			Event:       FirstMessage,
			Sequence:    sequence,
			Reconnected: afterReconnect,
			Publishable: true,
		}
	}
	if sequence == s.lastSequence {
		return Result{Event: Duplicate, Sequence: sequence, Reconnected: afterReconnect}
	}
	if sequence < s.lastSequence {
		return Result{Event: Regression, Sequence: sequence, Reconnected: afterReconnect}
	}

	expected := s.lastSequence + 1
	if sequence == expected {
		s.accept(sequence, false)
		if afterReconnect {
			return Result{Event: Reconnect, Sequence: sequence, Reconnected: true, Publishable: true}
		}
		return Result{Event: InOrder, Sequence: sequence, Publishable: true}
	}

	s.accept(sequence, true)
	return Result{
		Event:       Gap,
		Sequence:    sequence,
		GapFrom:     expected,
		GapTo:       sequence - 1,
		Missing:     sequence - expected,
		Reconnected: afterReconnect,
		Publishable: true,
	}
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
	return s.unresolvedGap
}

func (s *State) accept(sequence uint64, unresolvedGap bool) {
	s.lastSequence = sequence
	s.haveLast = true
	s.unresolvedGap = unresolvedGap
}
