package main

import (
	"bytes"
	"context"
	"flag"
	"fmt"
	"io"
	"os"
	"strings"
	"sync"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/majd/codex-agents/internal/appserver"
	"github.com/majd/codex-agents/internal/tui"
	"github.com/majd/codex-agents/internal/updater"
)

var version = "dev"

const (
	alternateScrollOn  = "\x1b[?1007h"
	alternateScrollOff = "\x1b[?1007l"
)

// Bubble Tea enables xterm modifyOtherKeys alongside the Kitty keyboard
// protocol. Codex deliberately disables that legacy mode because iTerm2,
// Ghostty, and some tmux transports can otherwise collapse Shift+Enter back
// into plain Enter. Keep Bubble Tea's renderer, but make its terminal setup
// match Codex's mode negotiation.
type codexTerminalWriter struct {
	io.Writer
	terminal *os.File
	mu       sync.Mutex
	pushed   bool
	flags    string
}

func (w *codexTerminalWriter) Write(p []byte) (int, error) {
	w.mu.Lock()
	defer w.mu.Unlock()
	filtered := w.normalizeTerminalModes(p)
	_, err := w.Writer.Write(filtered)
	if err != nil {
		return 0, err
	}
	return len(p), nil
}

func (w *codexTerminalWriter) normalizeTerminalModes(input []byte) []byte {
	const (
		modifyOtherKeys = "\x1b[>4;2m"
		setKeyboard     = "\x1b[=5;1u"
		resetKeyboard   = "\x1b[=0;1u"
	)
	var output bytes.Buffer
	releasedMouseCapture := bytes.Contains(input, []byte("\x1b[?1002l")) ||
		bytes.Contains(input, []byte("\x1b[?1003l")) ||
		bytes.Contains(input, []byte("\x1b[?1006l"))
	for len(input) > 0 {
		index, token := len(input), ""
		for _, candidate := range []string{modifyOtherKeys, setKeyboard, resetKeyboard} {
			if found := bytes.Index(input, []byte(candidate)); found >= 0 && found < index {
				index, token = found, candidate
			}
		}
		if token == "" {
			output.Write(input)
			break
		}
		output.Write(input[:index])
		switch token {
		case modifyOtherKeys:
			output.WriteString("\x1b[>4;0m")
		case setKeyboard:
			if !w.pushed {
				output.WriteString("\x1b[>" + w.flags + "u")
				w.pushed = true
			}
		case resetKeyboard:
			if w.pushed {
				output.WriteString("\x1b[<u\x1b[=0u")
				w.pushed = false
			}
		}
		input = input[index+len(token):]
	}
	// Bubble Tea releases overview mouse capture when entering a session. A
	// number of terminals also stop applying alternate-scroll at that boundary,
	// even though DECSET 1007 remains logically enabled. Reassert it after the
	// reset sequences so wheel input remains inside our virtual transcript.
	if releasedMouseCapture {
		output.WriteString(alternateScrollOn)
	}
	return output.Bytes()
}

// Bubble Tea only treats custom output as a terminal when it exposes the full
// terminal file contract. Forwarding these methods preserves size detection,
// raw-mode setup, and visible rendering while Write filters one mode sequence.
func (w *codexTerminalWriter) Read(p []byte) (int, error) { return w.terminal.Read(p) }
func (w *codexTerminalWriter) Close() error               { return nil }
func (w *codexTerminalWriter) Fd() uintptr                { return w.terminal.Fd() }

func codexKeyboardFlags() string {
	program := strings.ToLower(os.Getenv("TERM_PROGRAM"))
	if strings.Contains(program, "iterm") || strings.Contains(program, "ghostty") {
		return "5"
	}
	return "7"
}

func main() {
	showVersion := flag.Bool("version", false, "print version and exit")
	doctor := flag.Bool("doctor", false, "verify Codex App Server connectivity and exit")
	updateNow := flag.Bool("update", false, "install the latest GitHub release and exit")
	noUpdate := flag.Bool("no-update", false, "skip the automatic daily update check")
	nativeSessions := flag.Bool("native-sessions", true, "open sessions in the native Codex TUI (default)")
	legacySessions := flag.Bool("legacy-sessions", false, "use the legacy built-in session renderer")
	flag.Parse()
	if *showVersion {
		fmt.Printf("codex-agents %s\n", version)
		return
	}
	if *updateNow {
		result, err := checkForUpdate(true)
		if err != nil {
			fmt.Fprintf(os.Stderr, "codex-agents: update: %v\n", err)
			os.Exit(1)
		}
		if result.Updated {
			fmt.Printf("updated codex-agents %s → %s\n", version, result.Version)
		} else {
			fmt.Printf("codex-agents %s is up to date\n", version)
		}
		return
	}
	if !*noUpdate && os.Getenv("CODEX_AGENTS_NO_UPDATE") != "1" {
		result, err := checkForUpdate(false)
		if err == nil && result.Updated {
			fmt.Fprintf(os.Stderr, "codex-agents updated to %s\n", result.Version)
			if err := replaceProcess(result.Executable); err != nil {
				fmt.Fprintf(os.Stderr, "codex-agents: restart after update: %v\n", err)
				os.Exit(1)
			}
			return
		}
	}

	ctx, cancel := context.WithCancel(context.Background())
	defer cancel()
	client, err := appserver.Start(ctx, version)
	if err != nil {
		fmt.Fprintf(os.Stderr, "codex-agents: %v\n", err)
		os.Exit(1)
	}
	defer client.Close()

	listCtx, listCancel := context.WithTimeout(ctx, 20*time.Second)
	threads, err := client.ListThreads(listCtx)
	listCancel()
	if err != nil {
		fmt.Fprintf(os.Stderr, "codex-agents: list sessions: %v\n", err)
		os.Exit(1)
	}
	if *doctor {
		fmt.Printf("ok: Codex App Server connected; %d sessions visible\n", len(threads))
		return
	}

	useNativeSessions := *nativeSessions && !*legacySessions && os.Getenv("CODEX_AGENTS_NATIVE_SESSIONS") != "0"
	model := tui.New(client, tui.CurrentDirectory(), threads).WithNativeSessions(useNativeSessions)
	program := tea.NewProgram(
		model,
		tea.WithOutput(&codexTerminalWriter{Writer: os.Stdout, terminal: os.Stdout, flags: codexKeyboardFlags()}),
	)
	// Match Codex's selection-safe wheel behavior: with mouse reporting off,
	// alternate-scroll asks the terminal to translate the wheel into arrows.
	_, _ = fmt.Fprint(os.Stdout, alternateScrollOn)
	_, runErr := program.Run()
	_, _ = fmt.Fprint(os.Stdout, alternateScrollOff)
	if runErr != nil {
		fmt.Fprintf(os.Stderr, "codex-agents: %v\n", runErr)
		os.Exit(1)
	}
}

func checkForUpdate(force bool) (updater.Result, error) {
	ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
	defer cancel()
	return updater.MaybeUpdate(ctx, updater.Options{CurrentVersion: version, Force: force})
}
