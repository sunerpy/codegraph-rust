package ec2

import (
	"context"

	"example.com/synth/refresh"
)

type Discovery struct {
	*refresh.Discovery
	region string
}

func New(region string) *Discovery {
	d := &Discovery{region: region}
	d.Discovery = refresh.NewDiscovery(refresh.Options{
		Mech:     "ec2",
		RefreshF: d.refresh,
	})
	return d
}

func (d *Discovery) refresh(ctx context.Context) ([]*refresh.Group, error) {
	return nil, nil
}

func listStatic(ctx context.Context) ([]*refresh.Group, error) { return nil, nil }

// The static provider's options, wired at package level.
var staticOptions = refresh.Options{Mech: "static", RefreshF: listStatic}
