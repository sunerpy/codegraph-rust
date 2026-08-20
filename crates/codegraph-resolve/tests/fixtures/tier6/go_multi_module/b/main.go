package main

import "ex/a"

// Caller reaches across the module boundary.
func Caller() int {
	return a.Helper()
}
