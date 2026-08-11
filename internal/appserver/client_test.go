package appserver

import (
	"errors"
	"testing"
)

func TestIsActiveWriterError(t *testing.T) {
	err := &rpcError{Code: -32600, Message: "thread thr_1 already has an active writer"}
	if !IsActiveWriterError(err) || !IsActiveWriterError(errors.Join(errors.New("resume failed"), ErrActiveWriter)) {
		t.Fatal("active writer conflict was not classified")
	}
	if IsActiveWriterError(&rpcError{Code: -32600, Message: "different invalid request"}) {
		t.Fatal("unrelated invalid request was classified as a writer conflict")
	}
}
