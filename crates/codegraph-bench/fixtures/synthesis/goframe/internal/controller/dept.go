package controller

import (
	"context"

	"example.com/synthesis/goframe/api/system"
)

type sysDeptController struct{}

// NB: method name (List) differs from the request type (DeptSearchReq) — the join
// must be by signature, not name.
func (c *sysDeptController) List(ctx context.Context, req *system.DeptSearchReq) (res *system.DeptSearchRes, err error) {
	return helper(ctx)
}

func (c *sysDeptController) Add(ctx context.Context, req *system.DeptAddReq) (res *system.DeptAddRes, err error) {
	return
}

// Returns the response type but takes no request type — must NOT be linked.
func helper(ctx context.Context) (res *system.DeptSearchRes, err error) {
	return
}
