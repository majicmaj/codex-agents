//go:build windows

package main

import "errors"

func replaceProcess(string) error {
	return errors.New("automatic restart is not supported on Windows; launch codex-agents again")
}
