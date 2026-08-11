//go:build !windows

package main

import (
	"os"
	"syscall"
)

func replaceProcess(executable string) error {
	return syscall.Exec(executable, append([]string{executable}, os.Args[1:]...), os.Environ())
}
