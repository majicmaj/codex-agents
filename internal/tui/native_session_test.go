package tui

import (
	"bytes"
	"errors"
	"io"
	"reflect"
	"testing"

	"github.com/majd/codex-agents/internal/appserver"
)

func TestNativeSessionCommandUsesSharedDaemonAndSessionCwd(t *testing.T) {
	thread := appserver.Thread{ID: "019ff0fc-test", Cwd: "/tmp/project"}
	command := nativeSessionCommand(thread)

	wantArgs := []string{"codex", "resume", "--remote", "unix://", thread.ID}
	if !reflect.DeepEqual(command.Args, wantArgs) {
		t.Fatalf("args = %#v, want %#v", command.Args, wantArgs)
	}
	if command.Dir != thread.Cwd {
		t.Fatalf("dir = %q, want %q", command.Dir, thread.Cwd)
	}
}

func TestNativeNewSessionCommandSendsOverviewPrompt(t *testing.T) {
	command := nativeNewSessionCommand("/tmp/project", "explain this repository")

	wantArgs := []string{"codex", "--remote", "unix://", "-C", "/tmp/project", "explain this repository"}
	if !reflect.DeepEqual(command.Args, wantArgs) {
		t.Fatalf("args = %#v, want %#v", command.Args, wantArgs)
	}
	if command.Dir != "/tmp/project" {
		t.Fatalf("dir = %q, want %q", command.Dir, "/tmp/project")
	}
}

func TestNativeCodexCommandKeepsTerminalOwnershipInsidePTY(t *testing.T) {
	wrapped := &nativeCodexCommand{command: nativeSessionCommand(appserver.Thread{ID: "test"})}
	input := &bytes.Buffer{}
	wrapped.SetStdin(input)
	wrapped.SetStdout(&bytes.Buffer{})
	wrapped.SetStderr(&bytes.Buffer{})
	if wrapped.input != input {
		t.Fatal("native Codex did not retain the parent input stream")
	}
	if wrapped.command.Stdin != nil || wrapped.command.Stdout != nil || wrapped.command.Stderr != nil {
		t.Fatal("native Codex was attached before its PTY was created")
	}
}

func TestBridgeNativeInputReservesOnlyPlainLeft(t *testing.T) {
	tests := []struct {
		name       string
		input      string
		wantOutput string
		wantBack   bool
	}{
		{name: "csi left", input: "\x1b[D", wantBack: true},
		{name: "kitty left", input: "\x1b[1;1D", wantBack: true},
		{name: "kitty explicit left press", input: "\x1b[1;1:1D", wantBack: true},
		{name: "kitty repeated left", input: "\x1b[1;1:2D", wantBack: true},
		{name: "kitty left release", input: "\x1b[1;1:3D", wantOutput: "\x1b[1;1:3D"},
		{name: "kitty left with caps lock", input: "\x1b[1;65:1D", wantBack: true},
		{name: "application left", input: "\x1bOD", wantBack: true},
		{name: "left with input", input: "x\x1b[D", wantOutput: "x\x1b[D"},
		{name: "left after cursor edit and deleting input", input: "xy\x1b[D\x7f\x1b[3~\x1b[D", wantOutput: "xy\x1b[D\x7f\x1b[3~", wantBack: true},
		{name: "left after deleting input", input: "x\x7f\x1b[D", wantOutput: "x\x7f", wantBack: true},
		{name: "left after ctrl u", input: "draft\x15\x1b[D", wantOutput: "draft\x15", wantBack: true},
		{name: "left after home and delete", input: "x\x1b[H\x1b[3~\x1b[D", wantOutput: "x\x1b[H\x1b[3~", wantBack: true},
		{name: "left after clearing input", input: "x\x03\x1b[D", wantOutput: "x\x03", wantBack: true},
		{name: "left after enhanced ctrl c", input: "x\x1b[99;5u\x1b[1;1:1D", wantOutput: "x\x1b[99;5u", wantBack: true},
		{name: "left after submitting input", input: "x\r\x1b[D", wantOutput: "x\r", wantBack: true},
		{name: "left after uncertain history", input: "\x1b[A\x1b[D", wantOutput: "\x1b[A\x1b[D"},
		{name: "shift left", input: "\x1b[1;2D", wantOutput: "\x1b[1;2D"},
		{name: "ctrl left", input: "\x1b[1;5D", wantOutput: "\x1b[1;5D"},
		{name: "right", input: "\x1b[C", wantOutput: "\x1b[C"},
		{name: "escape", input: "\x1b", wantOutput: "\x1b"},
		{name: "text", input: "hello", wantOutput: "hello"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			var output bytes.Buffer
			back := false
			err := bridgeNativeInput(bytes.NewBufferString(test.input), &output, func() { back = true })
			if err != nil && !errors.Is(err, io.EOF) {
				t.Fatalf("bridge error: %v", err)
			}
			if output.String() != test.wantOutput {
				t.Fatalf("output = %q, want %q", output.String(), test.wantOutput)
			}
			if back != test.wantBack {
				t.Fatalf("back = %v, want %v", back, test.wantBack)
			}
		})
	}
}

func TestNativeSessionLeavesOverviewAsParent(t *testing.T) {
	thread := appserver.Thread{ID: "019ff0fc-test", Cwd: "/tmp/project"}
	model := New(nil, "/tmp", []appserver.Thread{thread}).WithNativeSessions(true)

	updated, command := model.openSelected()
	got := updated.(Model)
	if got.mode != listMode {
		t.Fatalf("mode = %v, want listMode", got.mode)
	}
	if !got.loading || got.status != "opening native Codex" {
		t.Fatalf("loading/status = %v/%q", got.loading, got.status)
	}
	if command == nil {
		t.Fatal("opening a native session returned no command")
	}
}

func TestNativeStartSessionUsesCodexInsteadOfAppServerWriter(t *testing.T) {
	model := New(nil, "/tmp/project", nil).WithNativeSessions(true)
	model.input = []rune("explain this repository")
	model.cursor = len(model.input)

	updated, command := model.startSession()
	got := updated.(Model)
	if got.mode != listMode {
		t.Fatalf("mode = %v, want listMode", got.mode)
	}
	if !got.loading || got.status != "starting session" {
		t.Fatalf("loading/status = %v/%q", got.loading, got.status)
	}
	if len(got.input) != 0 {
		t.Fatalf("input was not cleared: %q", string(got.input))
	}
	if command == nil {
		t.Fatal("starting a native session returned no command")
	}
}
