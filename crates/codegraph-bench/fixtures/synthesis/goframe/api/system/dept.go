package system

import "github.com/gogf/gf/v2/frame/g"

type DeptSearchReq struct {
	g.Meta   `path:"/dept/list" tags:"Dept" method:"get" summary:"list"`
	DeptName string
}
type DeptSearchRes struct {
	g.Meta `mime:"application/json"`
	List   []string
}

type DeptAddReq struct {
	g.Meta `path:"/dept/add" method:"post"`
	Name   string
}
type DeptAddRes struct{}

// A declared route whose handler does not exist in this codebase.
type OrphanReq struct {
	g.Meta `path:"/orphan" method:"get"`
}
type OrphanRes struct{}
