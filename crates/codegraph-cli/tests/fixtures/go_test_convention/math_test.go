package mathutil

import "testing"

func TestAdd(t *testing.T) {
	if Add(1, 2) != 3 {
		t.Fatal("Add is wrong")
	}
}

func TestDouble(t *testing.T) {
	if Double(2) != 4 {
		t.Fatal("Double is wrong")
	}
}
