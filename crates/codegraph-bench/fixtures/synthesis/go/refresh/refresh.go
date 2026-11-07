package refresh

import "context"

type Group struct{ Source string }

// Options are the options for a refreshing discovery.
type Options struct {
	Mech     string
	RefreshF func(ctx context.Context) ([]*Group, error)
}

// Discovery runs a refresh function on an interval.
type Discovery struct {
	refreshf func(ctx context.Context) ([]*Group, error)
}

func NewDiscovery(opts Options) *Discovery {
	return &Discovery{refreshf: opts.RefreshF}
}

func (d *Discovery) Refresh(ctx context.Context) ([]*Group, error) {
	return d.refreshf(ctx)
}
