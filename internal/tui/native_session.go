package tui

import (
	"io"
	"os"
	"os/exec"

	tea "charm.land/bubbletea/v2"
	"github.com/majd/codex-agents/internal/appserver"
)

// nativeCodexCommand bypasses the overview's terminal-mode-filtering writer.
// Codex performs a real TTY check on stdout and should negotiate its own modes
// while it owns the terminal. Bubble Tea still supplies the live stdin stream.
type nativeCodexCommand struct {
	command *exec.Cmd
}

func (c *nativeCodexCommand) Run() error               { return c.command.Run() }
func (c *nativeCodexCommand) SetStdin(input io.Reader) { c.command.Stdin = input }
func (c *nativeCodexCommand) SetStdout(io.Writer)      { c.command.Stdout = os.Stdout }
func (c *nativeCodexCommand) SetStderr(io.Writer)      { c.command.Stderr = os.Stderr }

// nativeSessionCommand deliberately invokes the installed Codex binary rather
// than duplicating its renderer. unix:// selects the durable local App Server
// daemon already used by codex-agents, avoiding a competing rollout writer.
func nativeSessionCommand(thread appserver.Thread) *exec.Cmd {
	command := exec.Command("codex", "resume", "--remote", "unix://", thread.ID)
	if thread.Cwd != "" {
		command.Dir = thread.Cwd
	}
	return command
}

func nativeNewSessionCommand(cwd, prompt string) *exec.Cmd {
	arguments := []string{"--remote", "unix://"}
	if cwd != "" {
		arguments = append(arguments, "-C", cwd)
	}
	if prompt != "" {
		arguments = append(arguments, prompt)
	}
	command := exec.Command("codex", arguments...)
	if cwd != "" {
		command.Dir = cwd
	}
	return command
}

func runNativeCommand(command *exec.Cmd, threadID string) tea.Cmd {
	return tea.Exec(&nativeCodexCommand{command: command}, func(err error) tea.Msg {
		return nativeSessionExitedMsg{threadID: threadID, err: err}
	})
}

func runNativeSession(thread appserver.Thread) tea.Cmd {
	return runNativeCommand(nativeSessionCommand(thread), thread.ID)
}

func runNativeNewSession(cwd, prompt string) tea.Cmd {
	return runNativeCommand(nativeNewSessionCommand(cwd, prompt), "")
}
