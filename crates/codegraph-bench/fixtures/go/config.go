package payroll

import "errors"

// Errors a payroll run returns.
var (
	// ErrNoRun is returned when no run is scheduled.
	ErrNoRun   = errors.New("no run")
	ErrTooLate = newRunError("too late")
)

// Phases of a payroll run.
const (
	PhaseDraft = iota
	PhaseFinal
)

// Schedule runs a hook at the end of a run.
type Schedule struct {
	Hook func() error
}

func newRunError(msg string) error { return errors.New(msg) }

func closeBooks() error { return nil }

var nightly = Schedule{Hook: closeBooks}

var defaultEmitter = &Emitter{Table: "payroll"}

var runAll = func() error {
	_ = RunNightly("all")
	return closeBooks()
}

func phaseOf(final bool) int {
	if final {
		return PhaseFinal
	}
	return PhaseDraft
}
