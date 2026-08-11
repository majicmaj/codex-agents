package tui

import (
	"bytes"
	"os"
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

func TestNativeCodexCommandKeepsRealTerminalOutput(t *testing.T) {
	wrapped := &nativeCodexCommand{command: nativeSessionCommand(appserver.Thread{ID: "test"})}
	wrapped.SetStdout(&bytes.Buffer{})
	wrapped.SetStderr(&bytes.Buffer{})
	if wrapped.command.Stdout != os.Stdout {
		t.Fatal("native Codex stdout was not attached directly to the terminal")
	}
	if wrapped.command.Stderr != os.Stderr {
		t.Fatal("native Codex stderr was not attached directly to the terminal")
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
