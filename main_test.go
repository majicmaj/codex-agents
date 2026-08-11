package main

import (
	"bytes"
	"strings"
	"testing"
)

func TestCodexTerminalWriterDisablesModifyOtherKeys(t *testing.T) {
	var output bytes.Buffer
	input := "before\x1b[>4;2m\x1b[=5;1uafter"
	writer := &codexTerminalWriter{Writer: &output, flags: "7"}
	n, err := writer.Write([]byte(input))
	if err != nil || n != len(input) {
		t.Fatalf("write = %d, %v", n, err)
	}
	if strings.Contains(output.String(), "\x1b[>4;2m") || !strings.Contains(output.String(), "\x1b[>4;0m") || !strings.Contains(output.String(), "\x1b[>7u") {
		t.Fatalf("terminal mode was not normalized: %q", output.String())
	}
	output.Reset()
	_, _ = writer.Write([]byte("\x1b[=0;1u"))
	if output.String() != "\x1b[<u\x1b[=0u" {
		t.Fatalf("keyboard stack was not restored: %q", output.String())
	}
}

func TestCodexTerminalWriterPreservesKeyboardStackOrder(t *testing.T) {
	var output bytes.Buffer
	writer := &codexTerminalWriter{Writer: &output, flags: "7"}
	_, _ = writer.Write([]byte("\x1b[=0;1u\x1b[=5;1u"))
	if output.String() != "\x1b[>7u" || !writer.pushed {
		t.Fatalf("startup sequence = %q, pushed=%v", output.String(), writer.pushed)
	}
	output.Reset()
	_, _ = writer.Write([]byte("\x1b[=0;1u"))
	if output.String() != "\x1b[<u\x1b[=0u" || writer.pushed {
		t.Fatalf("shutdown sequence = %q, pushed=%v", output.String(), writer.pushed)
	}
}

func TestCodexTerminalWriterRearmsAlternateScrollAfterMouseRelease(t *testing.T) {
	var output bytes.Buffer
	writer := &codexTerminalWriter{Writer: &output, flags: "7"}
	input := "\x1b[?1002l\x1b[?1003l\x1b[?1006l"
	_, _ = writer.Write([]byte(input))
	if !strings.HasPrefix(output.String(), input) || !strings.HasSuffix(output.String(), alternateScrollOn) {
		t.Fatalf("mouse release did not rearm alternate scroll: %q", output.String())
	}
}
