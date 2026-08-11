package tui

import (
	"errors"
	"io"
	"os"
	"os/exec"
	"os/signal"
	"strconv"
	"strings"
	"sync/atomic"
	"syscall"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/term"
	"github.com/creack/pty"
	"github.com/majd/codex-agents/internal/appserver"
	"github.com/muesli/cancelreader"
)

const nativeEscapeTimeout = 20 * time.Millisecond

// nativeCodexCommand gives Codex a real pseudo-terminal and forwards its output
// unchanged. The tiny input bridge reserves plain Left as the parent navigation
// gesture; every other byte remains owned by native Codex.
type nativeCodexCommand struct {
	command       *exec.Cmd
	input         io.Reader
	backRequested atomic.Bool
}

func (c *nativeCodexCommand) SetStdin(input io.Reader) { c.input = input }
func (c *nativeCodexCommand) SetStdout(io.Writer)      {}
func (c *nativeCodexCommand) SetStderr(io.Writer)      {}

func (c *nativeCodexCommand) Run() error {
	if c.input == nil {
		return errors.New("native Codex input is unavailable")
	}

	// Bubble Tea restores the parent terminal before Exec. Put only the parent
	// input side back in raw mode so individual keys can be proxied to Codex's
	// independently configured PTY slave.
	var restoreInput func()
	if inputFile, ok := c.input.(interface{ Fd() uintptr }); ok && term.IsTerminal(inputFile.Fd()) {
		state, err := term.MakeRaw(inputFile.Fd())
		if err != nil {
			return err
		}
		restoreInput = func() { _ = term.Restore(inputFile.Fd(), state) }
		defer restoreInput()
	}

	input, err := cancelreader.NewReader(c.input)
	if err != nil {
		return err
	}
	defer input.Close()

	size, _ := pty.GetsizeFull(os.Stdout)
	terminal, err := pty.StartWithSize(c.command, size)
	if err != nil {
		return err
	}

	resize := make(chan os.Signal, 1)
	resizeDone := make(chan struct{})
	signal.Notify(resize, syscall.SIGWINCH)
	go func() {
		for {
			select {
			case <-resize:
				_ = pty.InheritSize(terminal, os.Stdout)
			case <-resizeDone:
				return
			}
		}
	}()

	outputDone := make(chan struct{})
	go func() {
		_, _ = io.Copy(os.Stdout, terminal)
		close(outputDone)
	}()

	inputDone := make(chan error, 1)
	go func() {
		inputDone <- bridgeNativeInput(input, terminal, func() {
			c.backRequested.Store(true)
			if c.command.Process != nil {
				_ = c.command.Process.Signal(syscall.SIGTERM)
			}
		})
	}()

	waitErr := c.command.Wait()
	input.Cancel()
	_ = terminal.Close()
	inputErr := <-inputDone
	<-outputDone
	signal.Stop(resize)
	close(resizeDone)

	if c.backRequested.Load() {
		return nil
	}
	if waitErr != nil {
		return waitErr
	}
	if inputErr != nil && !errors.Is(inputErr, cancelreader.ErrCanceled) && !errors.Is(inputErr, io.EOF) {
		return inputErr
	}
	return nil
}

type nativeInputChunk struct {
	data []byte
	err  error
}

type nativeEscapeParser struct {
	pending []byte
}

func (p *nativeEscapeParser) feed(value byte) (output []byte, back bool) {
	if len(p.pending) == 0 {
		if value == '\x1b' {
			p.pending = append(p.pending, value)
			return nil, false
		}
		return []byte{value}, false
	}

	if len(p.pending) == 1 {
		if value == '[' || value == 'O' {
			p.pending = append(p.pending, value)
			return nil, false
		}
		output = append(output, p.pending...)
		p.pending = p.pending[:0]
		if value == '\x1b' {
			p.pending = append(p.pending, value)
			return output, false
		}
		return append(output, value), false
	}

	p.pending = append(p.pending, value)
	if p.pending[1] == 'O' {
		if len(p.pending) < 3 {
			return nil, false
		}
		if len(p.pending) == 3 && value == 'D' {
			return p.flush(), true
		}
		return p.flush(), false
	}

	if value >= 0x40 && value <= 0x7e {
		parameters := string(p.pending[2 : len(p.pending)-1])
		if value == 'D' && isPlainLeftPress(parameters) {
			return p.flush(), true
		}
		return p.flush(), false
	}
	if len(p.pending) > 32 {
		return p.flush(), false
	}
	return nil, false
}

// Codex enables Kitty keyboard enhancement flags 1|2|4. Terminals that report
// event types may include an explicit press/repeat suffix on an otherwise plain
// Left key. Release events must stay forwarded and must not navigate away.
func isPlainLeftPress(parameters string) bool {
	if parameters == "" || parameters == "1" {
		return true
	}
	key, modifiers, found := strings.Cut(parameters, ";")
	if !found || key != "1" {
		return false
	}
	modifierBits, event, ok := parseKittyModifierEvent(modifiers)
	if !ok || event == 3 {
		return false
	}
	// Caps Lock and Num Lock do not make Left a modified navigation key.
	const nonLockModifiers = 1 | 2 | 4 | 8 | 16 | 32
	return modifierBits&nonLockModifiers == 0
}

func parseKittyModifierEvent(value string) (modifierBits int, event int, ok bool) {
	modifierValue, eventValue, hasEvent := strings.Cut(value, ":")
	encoded, err := strconv.Atoi(modifierValue)
	if err != nil || encoded < 1 {
		return 0, 0, false
	}
	event = 1
	if hasEvent {
		event, err = strconv.Atoi(eventValue)
		if err != nil || event < 1 || event > 3 {
			return 0, 0, false
		}
	}
	return encoded - 1, event, true
}

func isEnhancedCtrlC(sequence string) bool {
	if !strings.HasPrefix(sequence, "\x1b[") || !strings.HasSuffix(sequence, "u") {
		return false
	}
	keyCodes, modifiers, found := strings.Cut(sequence[2:len(sequence)-1], ";")
	key, _, _ := strings.Cut(keyCodes, ":")
	if !found || key != "99" {
		return false
	}
	modifierBits, event, ok := parseKittyModifierEvent(modifiers)
	if !ok || event == 3 {
		return false
	}
	const nonLockModifiers = 1 | 2 | 4 | 8 | 16 | 32
	return modifierBits&nonLockModifiers == 4
}

// nativeDraftState intentionally prefers false negatives over false positives:
// Left may return to Codex when state is uncertain, but it must never close a
// session whose composer could contain text.
type nativeDraftState struct {
	units  int
	cursor int
	exact  bool
	paste  bool
}

func newNativeDraftState() nativeDraftState {
	return nativeDraftState{exact: true}
}

func (s nativeDraftState) definitelyEmpty() bool {
	return s.exact && s.units == 0
}

func (s *nativeDraftState) observe(data []byte) {
	if len(data) == 0 {
		return
	}
	if data[0] == '\x1b' {
		sequence := string(data)
		if isEnhancedCtrlC(sequence) {
			s.units, s.cursor, s.exact = 0, 0, true
			return
		}
		switch sequence {
		case "\x1b[200~":
			s.paste = true
		case "\x1b[201~":
			s.paste = false
		case "\x1b[13u", "\x1b[13;1u":
			s.units, s.cursor, s.exact = 0, 0, true
		case "\x1b[13;2u":
			if s.exact {
				s.units++
				s.cursor++
			}
		case "\x1b[A", "\x1b[B", "\x1b[H", "\x1b[F", "\x1b[1~", "\x1b[4~":
			switch sequence {
			case "\x1b[H", "\x1b[1~":
				s.cursor = 0
			case "\x1b[F", "\x1b[4~":
				s.cursor = s.units
			default:
				s.exact = false
			}
		case "\x1b[D", "\x1b[1D", "\x1b[1;1D", "\x1bOD":
			if s.exact && s.cursor > 0 {
				s.cursor--
			}
		case "\x1b[C", "\x1b[1C", "\x1b[1;1C", "\x1bOC":
			if s.exact && s.cursor < s.units {
				s.cursor++
			}
		case "\x1b[3~":
			if s.exact && s.cursor < s.units {
				s.units--
			}
		}
		return
	}

	for _, value := range data {
		switch value {
		case 0x03: // Ctrl+C clears a non-empty Codex composer.
			s.units, s.cursor, s.exact = 0, 0, true
		case '\r', '\n':
			if s.paste {
				if s.exact {
					s.units++
					s.cursor++
				}
			} else {
				s.units, s.cursor, s.exact = 0, 0, true
			}
		case 0x08, 0x7f:
			if s.exact && s.cursor > 0 {
				s.units--
				s.cursor--
			}
		case 0x01: // Ctrl+A
			s.cursor = 0
		case 0x02: // Ctrl+B
			if s.exact && s.cursor > 0 {
				s.cursor--
			}
		case 0x05: // Ctrl+E
			s.cursor = s.units
		case 0x06: // Ctrl+F
			if s.exact && s.cursor < s.units {
				s.cursor++
			}
		case 0x0b: // Ctrl+K
			if s.exact {
				s.units = s.cursor
			}
		case 0x15: // Ctrl+U
			if s.exact {
				s.units -= s.cursor
				s.cursor = 0
			}
		case 0x17: // Ctrl+W depends on word boundaries we cannot observe safely.
			if s.units > 0 {
				s.exact = false
			}
		default:
			if value >= 0x20 && (value < 0x80 || value >= 0xc0) && s.exact {
				s.units++
				s.cursor++
			}
		}
	}
}

func (p *nativeEscapeParser) flush() []byte {
	output := append([]byte(nil), p.pending...)
	p.pending = p.pending[:0]
	return output
}

func bridgeNativeInput(input io.Reader, output io.Writer, onBack func()) error {
	chunks := make(chan nativeInputChunk, 1)
	stop := make(chan struct{})
	defer close(stop)
	go func() {
		buffer := make([]byte, 1024)
		for {
			count, err := input.Read(buffer)
			chunk := nativeInputChunk{data: append([]byte(nil), buffer[:count]...), err: err}
			select {
			case chunks <- chunk:
			case <-stop:
				return
			}
			if err != nil {
				return
			}
		}
	}()

	parser := nativeEscapeParser{}
	draft := newNativeDraftState()
	var timer *time.Timer
	var timeout <-chan time.Time
	resetTimer := func() {
		if timer == nil {
			timer = time.NewTimer(nativeEscapeTimeout)
		} else {
			if !timer.Stop() {
				select {
				case <-timer.C:
				default:
				}
			}
			timer.Reset(nativeEscapeTimeout)
		}
		timeout = timer.C
	}
	stopTimer := func() {
		if timer != nil {
			timer.Stop()
		}
		timeout = nil
	}
	defer stopTimer()

	write := func(data []byte) error {
		if len(data) == 0 {
			return nil
		}
		_, err := output.Write(data)
		return err
	}

	for {
		select {
		case chunk := <-chunks:
			for _, value := range chunk.data {
				data, back := parser.feed(value)
				if back {
					if draft.definitelyEmpty() {
						onBack()
						return nil
					}
				}
				draft.observe(data)
				if err := write(data); err != nil {
					return err
				}
				if len(parser.pending) > 0 {
					resetTimer()
				} else {
					stopTimer()
				}
			}
			if chunk.err != nil {
				if err := write(parser.flush()); err != nil {
					return err
				}
				return chunk.err
			}
		case <-timeout:
			if err := write(parser.flush()); err != nil {
				return err
			}
			timeout = nil
		}
	}
}

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
