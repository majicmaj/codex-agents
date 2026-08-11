package tui

import (
	"context"
	"encoding/json"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"sort"
	"strings"
	"time"
	"unicode/utf8"

	tea "charm.land/bubbletea/v2"
	"github.com/majd/codex-agents/internal/appserver"
)

const (
	reset   = "\x1b[0m"
	dim     = "\x1b[2m"
	bold    = "\x1b[1m"
	cyan    = "\x1b[36m"
	green   = "\x1b[32m"
	magenta = "\x1b[35m"
	red     = "\x1b[31m"

	closeConfirmationText   = "press ctrl + x again to close this session"
	closeConfirmationWindow = 3 * time.Second
)

type mode int

const (
	listMode mode = iota
	sessionMode
)

type chatMessage struct {
	ID             string
	Role           string
	Text           string
	Kind           string
	Status         string
	Detail         string
	Phase          string
	TurnID         string
	TurnDurationMS int64
	Changes        []fileChange
	Actions        []commandAction
	RiskLevel      string
	Authorization  string
}

type transcriptPoint struct {
	row int
	col int
}

type Model struct {
	client              *appserver.Client
	cwd                 string
	threads             []appserver.Thread
	selected            int
	mode                mode
	input               []rune
	cursor              int
	selectionAnchor     int
	hasSelection        bool
	width               int
	height              int
	sessionID           string
	messages            []chatMessage
	recaps              map[string]string
	unread              map[string]bool
	loading             bool
	status              string
	err                 error
	groupByProject      bool
	projectsRoot        string
	lastEmptyCtrlC      time.Time
	lastCtrlX           time.Time
	popupSelected       int
	history             []string
	historyIndex        int
	historyBuffers      map[int][]rune
	killBuffer          string
	showHelp            bool
	activeTurns         map[string]string
	ownedThreads        map[string]bool
	writerBusy          map[string]bool
	scrollOffset        int
	turnStarted         map[string]time.Time
	statusProbe         *sessionStatusProbe
	externalStamp       string
	externalReading     bool
	discovering         bool
	lastDiscovery       time.Time
	mouseSelecting      bool
	transcriptSelecting bool
	transcriptSelected  bool
	transcriptAnchor    transcriptPoint
	transcriptHead      transcriptPoint
	expandedTools       bool
	transcript          *transcriptLayout
	nativeSessions      bool
}

type eventMsg appserver.Event
type tickMsg time.Time
type closeConfirmationExpiredMsg struct{ armedAt time.Time }
type threadsMsg struct {
	threads []appserver.Thread
	err     error
}
type discoveredThreadsMsg struct {
	threads []appserver.Thread
	err     error
}
type resumedMsg struct {
	thread     appserver.Thread
	owned      bool
	writerBusy bool
	err        error
}
type startedMsg struct {
	thread appserver.Thread
	turnID string
	prompt string
	err    error
}
type sentMsg struct {
	threadID   string
	turnID     string
	text       string
	messageAt  int
	optimistic bool
	err        error
}
type interruptedMsg struct{ err error }
type renamedMsg struct {
	threadID string
	name     string
	err      error
}
type closedSessionMsg struct {
	threadID string
	status   string
	err      error
}
type sessionStatusSnapshot struct {
	Status appserver.Status
	Recap  string
}
type externalStatusesMsg map[string]sessionStatusSnapshot
type externalHistoryMsg struct {
	thread appserver.Thread
	err    error
}
type nativeSessionExitedMsg struct {
	threadID string
	err      error
}

func New(client *appserver.Client, cwd string, threads []appserver.Thread) Model {
	return Model{
		client: client, cwd: cwd, threads: threads, unread: make(map[string]bool),
		recaps:         make(map[string]string),
		groupByProject: true, projectsRoot: defaultProjectsRoot(), activeTurns: make(map[string]string), ownedThreads: make(map[string]bool), writerBusy: make(map[string]bool),
		turnStarted: make(map[string]time.Time), statusProbe: newSessionStatusProbe(),
		transcript: newTranscriptLayout(),
	}
}

// WithNativeSessions selects the production session renderer. The overview
// stays in this process, while sessions temporarily own the terminal through
// the installed Codex TUI connected to the same shared App Server daemon.
func (m Model) WithNativeSessions(enabled bool) Model {
	m.nativeSessions = enabled
	return m
}

func (m Model) Init() tea.Cmd {
	return tea.Batch(waitEvent(m.client.Events()), tick(), scanExternalStatuses(m.statusProbe, m.threads))
}

func waitEvent(events <-chan appserver.Event) tea.Cmd {
	return func() tea.Msg {
		event, ok := <-events
		if !ok {
			return eventMsg{Method: "connection/closed"}
		}
		return eventMsg(event)
	}
}

func tick() tea.Cmd {
	return tea.Tick(time.Second, func(t time.Time) tea.Msg { return tickMsg(t) })
}

func (m Model) Update(message tea.Msg) (tea.Model, tea.Cmd) {
	switch msg := message.(type) {
	case tea.WindowSizeMsg:
		m.width, m.height = msg.Width, msg.Height
	case tickMsg:
		if !m.lastEmptyCtrlC.IsZero() && time.Since(m.lastEmptyCtrlC) > 2*time.Second {
			m.lastEmptyCtrlC = time.Time{}
			if m.status == "ctrl+c again to quit" {
				m.status = ""
			}
		}
		if !m.lastCtrlX.IsZero() && time.Since(m.lastCtrlX) > closeConfirmationWindow {
			m.lastCtrlX = time.Time{}
		}
		commands := []tea.Cmd{tick(), scanExternalStatuses(m.statusProbe, m.threads)}
		if !m.discovering && (m.lastDiscovery.IsZero() || time.Time(msg).Sub(m.lastDiscovery) >= 2*time.Second) {
			m.discovering = true
			m.lastDiscovery = time.Time(msg)
			commands = append(commands, discoverThreads(m.client))
		}
		return m, tea.Batch(commands...)
	case closeConfirmationExpiredMsg:
		if m.lastCtrlX.Equal(msg.armedAt) {
			m.lastCtrlX = time.Time{}
		}
	case externalStatusesMsg:
		if m.recaps == nil {
			m.recaps = make(map[string]string)
		}
		for id, snapshot := range msg {
			m.mergeExternalStatus(id, snapshot.Status)
			if snapshot.Recap != "" {
				m.recaps[id] = snapshot.Recap
			}
		}
		if m.mode == sessionMode && m.sessionID != "" && !m.ownedThreads[m.sessionID] && !m.externalReading {
			if stamp := m.statusProbe.rolloutStamp(m.sessionID); stamp != "" && stamp != m.externalStamp {
				m.externalStamp = stamp
				m.externalReading = true
				return m, refreshExternalHistory(m.client, m.sessionID)
			}
		}
	case externalHistoryMsg:
		m.externalReading = false
		if msg.err != nil {
			m.err = msg.err
			break
		}
		oldThread, _ := m.threadByID(msg.thread.ID)
		oldRows := m.ensureTranscriptLayout(oldThread).totalRows()
		oldStatus := appserver.Status{}
		if existing, ok := m.threadByID(msg.thread.ID); ok {
			oldStatus = existing.Status
		}
		messages := messagesFromTurns(msg.thread.Turns)
		messages = mergeRolloutReviews(messages, m.statusProbe.approvals(msg.thread.ID))
		m.replaceMessages(messages)
		newRows := m.ensureTranscriptLayout(msg.thread).totalRows()
		if m.scrollOffset > 0 {
			m.scrollOffset += max(0, newRows-oldRows)
		}
		// The rollout file is the authoritative live signal for sessions owned
		// by another Codex process. thread/read can lag behind and report either
		// idle or notLoaded immediately after task_started.
		if !m.ownedThreads[msg.thread.ID] && oldStatus.Type == "active" && msg.thread.Status.Type != "active" {
			msg.thread.Status = oldStatus
		}
		m.upsertThread(msg.thread)
	case eventMsg:
		m.applyEvent(appserver.Event(msg))
		return m, waitEvent(m.client.Events())
	case threadsMsg:
		m.loading = false
		if msg.err != nil {
			m.err = msg.err
		} else {
			m.mergeThreads(msg.threads)
			m.status = ""
		}
	case discoveredThreadsMsg:
		m.discovering = false
		if msg.err == nil {
			selectedID := m.selectedThreadID()
			m.mergeDiscoveredThreads(msg.threads)
			m.selectThread(selectedID)
		}
	case nativeSessionExitedMsg:
		m.loading = false
		if msg.err != nil {
			m.err = fmt.Errorf("native Codex session: %w", msg.err)
			m.status = ""
			break
		}
		m.status = "returned from native Codex"
		m.selectThread(msg.threadID)
		m.discovering = true
		return m, discoverThreads(m.client)
	case resumedMsg:
		m.loading = false
		if msg.err != nil {
			m.err = msg.err
			break
		}
		m.clearTranscriptSelection()
		m.sessionID = msg.thread.ID
		if m.writerBusy == nil {
			m.writerBusy = make(map[string]bool)
		}
		if msg.owned {
			m.ownedThreads[msg.thread.ID] = true
			delete(m.writerBusy, msg.thread.ID)
		} else {
			delete(m.ownedThreads, msg.thread.ID)
			m.writerBusy[msg.thread.ID] = msg.writerBusy
		}
		m.lastCtrlX = time.Time{}
		m.messages = messagesFromTurns(msg.thread.Turns)
		if !m.ownedThreads[msg.thread.ID] {
			m.messages = mergeRolloutReviews(m.messages, m.statusProbe.approvals(msg.thread.ID))
		}
		m.setPromptHistory(m.messages)
		m.invalidateTranscript(0)
		m.mode = sessionMode
		m.unread[msg.thread.ID] = false
		m.status = ""
		m.scrollOffset = 0
		m.externalStamp = m.statusProbe.rolloutStamp(msg.thread.ID)
		m.upsertThread(msg.thread)
		if turn := activeTurn(msg.thread.Turns); turn != nil {
			m.activeTurns[msg.thread.ID] = turn.ID
			if turn.StartedAt != nil {
				m.turnStarted[msg.thread.ID] = time.Unix(*turn.StartedAt, 0)
			} else {
				m.turnStarted[msg.thread.ID] = time.Now()
			}
		}
	case startedMsg:
		m.loading = false
		if msg.err != nil {
			m.err = msg.err
			break
		}
		m.clearTranscriptSelection()
		m.upsertThread(msg.thread)
		m.sessionID = msg.thread.ID
		m.lastCtrlX = time.Time{}
		m.activeTurns[msg.thread.ID] = msg.turnID
		m.turnStarted[msg.thread.ID] = time.Now()
		m.ownedThreads[msg.thread.ID] = true
		delete(m.writerBusy, msg.thread.ID)
		m.messages = []chatMessage{{Role: "user", Text: msg.prompt}}
		m.setPromptHistory(m.messages)
		m.invalidateTranscript(0)
		m.mode = sessionMode
		m.scrollOffset = 0
		m.status = ""
	case sentMsg:
		m.loading = false
		if m.writerBusy == nil {
			m.writerBusy = make(map[string]bool)
		}
		if msg.err != nil {
			if msg.optimistic {
				m.removeOptimisticMessage(msg.messageAt, msg.text)
				m.restoreDraft(msg.text)
			}
			if appserver.IsActiveWriterError(msg.err) {
				m.writerBusy[msg.threadID] = true
				delete(m.ownedThreads, msg.threadID)
				m.status = ""
				m.err = nil
			} else {
				m.err = msg.err
			}
		} else {
			if !msg.optimistic {
				m.recordHistory(msg.text)
				if string(m.input) == msg.text {
					m.clearInput()
				}
				if !m.hasUserMessage(msg.text) {
					messageIndex := len(m.messages)
					m.messages = append(m.messages, chatMessage{Role: "user", Text: msg.text})
					m.invalidateTranscript(messageIndex)
				}
			}
			m.activeTurns[msg.threadID] = msg.turnID
			m.turnStarted[msg.threadID] = time.Now()
			m.ownedThreads[msg.threadID] = true
			delete(m.writerBusy, msg.threadID)
			m.status = ""
		}
	case interruptedMsg:
		if msg.err != nil {
			m.err = msg.err
		} else {
			m.status = "interrupt requested"
		}
	case renamedMsg:
		m.loading = false
		if msg.err != nil {
			m.err = msg.err
		} else {
			m.setThreadName(msg.threadID, msg.name)
			m.status = "renamed session to " + msg.name
		}
	case closedSessionMsg:
		m.loading = false
		if msg.err != nil {
			m.err = msg.err
		} else {
			m.lastCtrlX = time.Time{}
			delete(m.ownedThreads, msg.threadID)
			delete(m.writerBusy, msg.threadID)
			delete(m.activeTurns, msg.threadID)
			delete(m.turnStarted, msg.threadID)
			m.updateStatus(msg.threadID, appserver.Status{Type: "notLoaded"})
			m.clearTranscriptSelection()
			m.mode, m.sessionID, m.messages = listMode, "", nil
			m.invalidateTranscript(0)
			m.scrollOffset = 0
			m.status = "session closed · open it later to resume"
		}
	case tea.KeyPressMsg:
		return m.handleKey(msg)
	case tea.PasteMsg:
		m.insertRunes([]rune(msg.Content))
		return m, nil
	case tea.MouseClickMsg:
		if msg.Button == tea.MouseLeft && !m.showHelp {
			if position, ok := m.composerPosition(msg.X, msg.Y, false); ok {
				m.clearTranscriptSelection()
				m.cursor = position
				m.selectionAnchor = position
				m.hasSelection = false
				m.mouseSelecting = true
			} else if point, ok := m.transcriptPointAt(msg.X, msg.Y, false); ok {
				m.hasSelection = false
				m.mouseSelecting = false
				m.transcriptAnchor = point
				m.transcriptHead = point
				m.transcriptSelected = false
				m.transcriptSelecting = true
			} else {
				m.hasSelection = false
				m.mouseSelecting = false
				m.clearTranscriptSelection()
			}
		}
	case tea.MouseMotionMsg:
		if m.mouseSelecting {
			if position, ok := m.composerPosition(msg.X, msg.Y, true); ok {
				m.cursor = position
				m.hasSelection = m.cursor != m.selectionAnchor
			}
		} else if m.transcriptSelecting {
			if point, ok := m.transcriptPointAt(msg.X, msg.Y, true); ok {
				m.transcriptHead = point
				m.transcriptSelected = point != m.transcriptAnchor
			}
		}
	case tea.MouseReleaseMsg:
		if m.mouseSelecting {
			m.mouseSelecting = false
			if position, ok := m.composerPosition(msg.X, msg.Y, true); ok {
				m.cursor = position
				m.hasSelection = m.cursor != m.selectionAnchor
			}
			if start, end, ok := m.selection(); ok {
				return m, copyText(string(m.input[start:end]))
			}
		} else if m.transcriptSelecting {
			m.transcriptSelecting = false
			if point, ok := m.transcriptPointAt(msg.X, msg.Y, true); ok {
				m.transcriptHead = point
				m.transcriptSelected = point != m.transcriptAnchor
			}
			if text := m.selectedTranscriptText(); text != "" {
				return m, copyText(text)
			}
		}
	case tea.MouseWheelMsg:
		if m.mode == listMode {
			switch msg.Button {
			case tea.MouseWheelUp:
				m.moveListSelection(-1)
			case tea.MouseWheelDown:
				m.moveListSelection(1)
			}
		} else {
			switch msg.Button {
			case tea.MouseWheelUp:
				m.scrollConversation(3)
			case tea.MouseWheelDown:
				m.scrollConversation(-3)
			}
		}
	}
	return m, nil
}

func (m Model) handleKey(key tea.KeyPressMsg) (tea.Model, tea.Cmd) {
	m.err = nil
	if m.showHelp {
		if key.String() == "?" || key.String() == "esc" || key.String() == "ctrl+c" {
			m.showHelp = false
		}
		return m, nil
	}

	if matches := m.matchingCommands(); len(matches) > 0 {
		switch key.String() {
		case "up", "ctrl+p":
			m.popupSelected = max(0, m.popupSelected-1)
			return m, nil
		case "down", "ctrl+n":
			m.popupSelected = min(len(matches)-1, m.popupSelected+1)
			return m, nil
		case "tab":
			m.completeSlashCommand()
			return m, nil
		case "esc":
			m.clearInput()
			m.popupSelected = 0
			return m, nil
		case "enter":
			name, args, exact := parseSlashCommand(string(m.input))
			if !exact || args == "" && string(m.input) != "/"+name && string(m.input) != "/"+name+" " {
				m.completeSlashCommand()
				return m, nil
			}
		}
	}
	if m.mode == listMode {
		switch key.String() {
		case "esc":
			return m, tea.Quit
		case "ctrl+c":
			if len(m.input) > 0 {
				m.clearInput()
				m.status = ""
				m.lastEmptyCtrlC = time.Time{}
				return m, nil
			}
			if !m.lastEmptyCtrlC.IsZero() && time.Since(m.lastEmptyCtrlC) <= 2*time.Second {
				return m, tea.Quit
			}
			m.lastEmptyCtrlC = time.Now()
			m.status = "ctrl+c again to quit"
			return m, nil
		case "ctrl+d":
			if len(m.input) == 0 {
				return m, tea.Quit
			}
			m.deleteForward()
		case "?":
			if len(m.input) == 0 {
				m.showHelp = true
				return m, nil
			}
			m.insertRunes([]rune(key.Text))
		case "up":
			if len(m.input) > 0 && m.recallHistory(-1) {
				break
			}
			m.moveListSelection(-1)
		case "down":
			if len(m.input) > 0 && m.recallHistory(1) {
				break
			}
			m.moveListSelection(1)
		case "right":
			if len(m.input) == 0 {
				return m.openSelected()
			}
			m.moveCursor(1, false)
		case "enter":
			if len(m.input) > 0 {
				if name, args, ok := parseSlashCommand(string(m.input)); ok {
					return m.runSlashCommand(name, args)
				}
				return m.startSession()
			}
			return m.openSelected()
		case "backspace", "shift+backspace", "ctrl+h":
			m.backspace()
		case "space":
			m.insertRunes([]rune{' '})
		case "alt+backspace", "ctrl+backspace", "ctrl+shift+backspace", "ctrl+w", "ctrl+alt+h":
			m.deleteWordBackward()
		case "alt+delete", "ctrl+delete", "ctrl+shift+delete", "alt+d":
			m.deleteWordForward()
		case "delete", "shift+delete":
			m.deleteForward()
		case "left", "ctrl+b":
			m.moveCursor(-1, false)
		case "ctrl+f":
			m.moveCursor(1, false)
		case "alt+b", "alt+left", "ctrl+left":
			m.moveWordBackward()
		case "alt+f", "alt+right", "ctrl+right":
			m.moveWordForward()
		case "shift+left":
			m.moveCursor(-1, true)
		case "shift+right":
			m.moveCursor(1, true)
		case "home", "ctrl+a":
			m.moveLineStart()
		case "end", "ctrl+e":
			m.moveLineEnd()
		case "ctrl+u":
			m.killToStart()
		case "ctrl+k":
			m.killToEnd()
		case "ctrl+y":
			m.insertRunes([]rune(m.killBuffer))
		case "ctrl+j", "ctrl+m", "shift+enter", "alt+enter":
			m.insertRunes([]rune{'\n'})
		case "ctrl+r":
			m.loading, m.status = true, "refreshing"
			return m, loadThreads(m.client)
		case "g":
			selectedID := m.selectedThreadID()
			m.groupByProject = !m.groupByProject
			m.selectThread(selectedID)
		default:
			if key.Text != "" {
				m.insertRunes([]rune(key.Text))
			}
		}
		return m, nil
	}

	switch key.String() {
	case "ctrl+x":
		if m.sessionID == "" || m.loading {
			return m, nil
		}
		if !m.closeConfirmationArmed(time.Now()) {
			m.lastCtrlX = time.Now()
			m.status = ""
			return m, expireCloseConfirmation(m.lastCtrlX)
		}
		m.lastCtrlX = time.Time{}
		m.loading = true
		m.status = "closing session"
		return m, closeSession(m.client, m.sessionID)
	case "esc":
		if m.activeTurns[m.sessionID] != "" {
			return m.interruptActiveTurn()
		}
		if len(m.input) == 0 {
			m.lastCtrlX = time.Time{}
			m.clearTranscriptSelection()
			m.mode = listMode
			m.sessionID = ""
			m.messages = nil
			m.invalidateTranscript(0)
			return m, nil
		}
	case "left":
		if len(m.input) == 0 {
			m.lastCtrlX = time.Time{}
			m.clearTranscriptSelection()
			m.mode = listMode
			m.sessionID = ""
			m.messages = nil
			m.invalidateTranscript(0)
			return m, nil
		}
		m.moveCursor(-1, false)
	case "ctrl+c":
		if len(m.input) > 0 {
			m.clearInput()
			return m, nil
		}
		if m.sessionID != "" {
			return m.interruptActiveTurn()
		}
	case "ctrl+d":
		if len(m.input) == 0 {
			return m, tea.Quit
		}
		m.deleteForward()
	case "ctrl+t":
		m.expandedTools = !m.expandedTools
	case "?":
		if len(m.input) == 0 {
			m.showHelp = true
			return m, nil
		}
		m.insertRunes([]rune(key.Text))
	case "enter":
		if len(m.input) > 0 && !m.loading {
			text := string(m.input)
			if name, args, ok := parseSlashCommand(text); ok {
				return m.runSlashCommand(name, args)
			}
			m.scrollOffset = 0
			m.loading = true
			resume := !m.ownedThreads[m.sessionID]
			messageIndex := -1
			if !resume {
				m.recordHistory(text)
				m.clearInput()
				messageIndex = len(m.messages)
				m.messages = append(m.messages, chatMessage{Role: "user", Text: text})
				m.invalidateTranscript(messageIndex)
			}
			return m, sendTurn(m.client, m.sessionID, m.activeTurns[m.sessionID], text, resume, messageIndex)
		}
	case "backspace", "shift+backspace", "ctrl+h":
		m.backspace()
	case "space":
		m.insertRunes([]rune{' '})
	case "alt+backspace", "ctrl+backspace", "ctrl+shift+backspace", "ctrl+w", "ctrl+alt+h":
		m.deleteWordBackward()
	case "alt+delete", "ctrl+delete", "ctrl+shift+delete", "alt+d":
		m.deleteWordForward()
	case "delete", "shift+delete":
		m.deleteForward()
	case "right", "ctrl+f":
		m.moveCursor(1, false)
	case "ctrl+b":
		m.moveCursor(-1, false)
	case "alt+b", "alt+left", "ctrl+left":
		m.moveWordBackward()
	case "alt+f", "alt+right", "ctrl+right":
		m.moveWordForward()
	case "up", "ctrl+p":
		if m.moveCursorVertical(-1, max(1, m.width-6)) {
			break
		}
		if m.recallHistory(-1) {
			break
		}
		if len(m.history) == 0 {
			m.scrollConversation(1)
		}
	case "down", "ctrl+n":
		if m.moveCursorVertical(1, max(1, m.width-6)) {
			break
		}
		if m.recallHistory(1) {
			break
		}
		if len(m.history) == 0 {
			m.scrollConversation(-1)
		}
	case "pgup":
		m.scrollConversation(max(3, m.height/2))
	case "pgdown":
		m.scrollConversation(-max(3, m.height/2))
	case "home":
		if len(m.input) == 0 {
			m.scrollConversation(1 << 30)
		} else {
			m.moveLineStart()
		}
	case "end":
		if len(m.input) == 0 {
			m.scrollOffset = 0
		} else {
			m.moveLineEnd()
		}
	case "shift+left":
		m.moveCursor(-1, true)
	case "shift+right":
		m.moveCursor(1, true)
	case "ctrl+a":
		m.moveLineStart()
	case "ctrl+e":
		m.moveLineEnd()
	case "ctrl+u":
		m.killToStart()
	case "ctrl+k":
		m.killToEnd()
	case "ctrl+y":
		m.insertRunes([]rune(m.killBuffer))
	case "ctrl+j", "ctrl+m", "shift+enter", "alt+enter":
		m.insertRunes([]rune{'\n'})
	default:
		if key.Text != "" {
			m.insertRunes([]rune(key.Text))
		}
	}
	return m, nil
}

func (m Model) openSelected() (tea.Model, tea.Cmd) {
	threads := m.orderedThreads()
	if len(threads) == 0 || m.loading {
		return m, nil
	}
	thread := threads[m.selected]
	if m.nativeSessions {
		m.loading, m.status = true, "opening native Codex"
		return m, runNativeSession(thread)
	}
	m.loading, m.status = true, "loading session"
	id := thread.ID
	return m, resumeThread(m.client, id, m.ownedThreads[id])
}

func (m Model) startSession() (tea.Model, tea.Cmd) {
	if m.loading {
		return m, nil
	}
	prompt := strings.TrimSpace(string(m.input))
	if prompt == "" {
		return m, nil
	}
	m.recordHistory(string(m.input))
	m.clearInput()
	m.loading, m.status = true, "starting session"
	if m.nativeSessions {
		return m, runNativeNewSession(m.cwd, prompt)
	}
	return m, startThread(m.client, m.cwd, prompt)
}

func (m Model) runSlashCommand(name, args string) (tea.Model, tea.Cmd) {
	m.recordHistory(strings.TrimSpace(string(m.input)))
	m.clearInput()
	switch name {
	case "new":
		if args == "" {
			m.lastCtrlX = time.Time{}
			m.clearTranscriptSelection()
			m.mode, m.sessionID, m.messages = listMode, "", nil
			m.invalidateTranscript(0)
			m.status = "type a prompt to start a new session"
			return m, nil
		}
		m.input, m.cursor = []rune(args), len([]rune(args))
		return m.startSession()
	case "resume":
		m.lastCtrlX = time.Time{}
		m.clearTranscriptSelection()
		m.mode, m.sessionID, m.messages = listMode, "", nil
		m.invalidateTranscript(0)
		m.status = "select a session to resume"
		return m, nil
	case "rename":
		if m.sessionID == "" {
			m.status = "open a session before renaming it"
			return m, nil
		}
		name := strings.TrimSpace(args)
		if name == "" {
			m.status = "usage: /rename <session name>"
			return m, nil
		}
		m.loading, m.status = true, "renaming session"
		return m, renameThread(m.client, m.sessionID, name)
	case "status":
		if thread, ok := m.threadByID(m.sessionID); ok {
			_, _, state := stateFor(thread, m.unread[thread.ID])
			m.status = fmt.Sprintf("%s · %s · %s", state, filepath.Base(thread.Cwd), thread.ID)
		} else {
			m.status = fmt.Sprintf("%d sessions · grouping by %s", len(m.threads), map[bool]string{true: "project", false: "status"}[m.groupByProject])
		}
		return m, nil
	case "stop":
		if m.sessionID == "" {
			m.status = "open a running session to interrupt it"
			return m, nil
		}
		return m.interruptActiveTurn()
	case "clear":
		m.messages = nil
		m.invalidateTranscript(0)
		m.status = "conversation view cleared; session history is unchanged"
		return m, nil
	case "help":
		m.showHelp = true
		return m, nil
	case "quit", "exit":
		return m, tea.Quit
	default:
		m.status = fmt.Sprintf("/%s is a native Codex command and is not exposed by this App Server view", name)
		return m, nil
	}
}

func loadThreads(client *appserver.Client) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
		defer cancel()
		threads, err := client.ListThreads(ctx)
		return threadsMsg{threads, err}
	}
}

func discoverThreads(client *appserver.Client) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 15*time.Second)
		defer cancel()
		threads, err := client.ListThreads(ctx)
		return discoveredThreadsMsg{threads: threads, err: err}
	}
}

type threadOpener interface {
	ResumeThread(context.Context, string) (appserver.Thread, error)
	ReadThreadHistory(context.Context, string) (appserver.Thread, error)
}

func resumeThread(client threadOpener, id string, alreadyOwned bool) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		owned, writerBusy := alreadyOwned, false
		if !alreadyOwned {
			if _, err := client.ResumeThread(ctx, id); err != nil {
				if !appserver.IsActiveWriterError(err) {
					return resumedMsg{err: err}
				}
				writerBusy = true
			} else {
				owned = true
			}
		}
		thread, err := client.ReadThreadHistory(ctx, id)
		return resumedMsg{thread: thread, owned: owned, writerBusy: writerBusy, err: err}
	}
}

func refreshExternalHistory(client *appserver.Client, id string) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		thread, err := client.ReadThreadHistory(ctx, id)
		return externalHistoryMsg{thread: thread, err: err}
	}
}

func startThread(client *appserver.Client, cwd, prompt string) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		thread, err := client.StartThread(ctx, cwd)
		if err == nil {
			turn, turnErr := client.StartTurn(ctx, thread.ID, prompt)
			err = turnErr
			return startedMsg{thread: thread, turnID: turn.ID, prompt: prompt, err: err}
		}
		return startedMsg{thread: thread, prompt: prompt, err: err}
	}
}

type turnStarter interface {
	ResumeThread(context.Context, string) (appserver.Thread, error)
	StartTurn(context.Context, string, string) (appserver.Turn, error)
	SteerTurn(context.Context, string, string, string) (appserver.Turn, error)
}

func sendTurn(client turnStarter, id, activeTurnID, text string, resume bool, messageAt int) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 30*time.Second)
		defer cancel()
		if resume {
			if _, err := client.ResumeThread(ctx, id); err != nil {
				return sentMsg{threadID: id, text: text, messageAt: messageAt, optimistic: messageAt >= 0, err: err}
			}
		}
		var turn appserver.Turn
		var err error
		if activeTurnID != "" {
			turn, err = client.SteerTurn(ctx, id, activeTurnID, text)
		} else {
			turn, err = client.StartTurn(ctx, id, text)
		}
		return sentMsg{threadID: id, turnID: turn.ID, text: text, messageAt: messageAt, optimistic: messageAt >= 0, err: err}
	}
}

func interruptTurn(client *appserver.Client, threadID, turnID string) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		return interruptedMsg{client.InterruptTurn(ctx, threadID, turnID)}
	}
}

func renameThread(client *appserver.Client, threadID, name string) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		return renamedMsg{threadID: threadID, name: name, err: client.SetThreadName(ctx, threadID, name)}
	}
}

func closeSession(client *appserver.Client, threadID string) tea.Cmd {
	return func() tea.Msg {
		ctx, cancel := context.WithTimeout(context.Background(), 10*time.Second)
		defer cancel()
		status, err := client.UnsubscribeThread(ctx, threadID)
		return closedSessionMsg{threadID: threadID, status: status, err: err}
	}
}

func expireCloseConfirmation(armedAt time.Time) tea.Cmd {
	return tea.Tick(closeConfirmationWindow, func(time.Time) tea.Msg {
		return closeConfirmationExpiredMsg{armedAt: armedAt}
	})
}

func (m Model) closeConfirmationArmed(now time.Time) bool {
	if m.lastCtrlX.IsZero() {
		return false
	}
	elapsed := now.Sub(m.lastCtrlX)
	return elapsed >= 0 && elapsed <= closeConfirmationWindow
}

func (m Model) interruptActiveTurn() (tea.Model, tea.Cmd) {
	turnID := m.activeTurns[m.sessionID]
	if turnID == "" {
		m.status = "no active turn to interrupt"
		return m, nil
	}
	return m, interruptTurn(m.client, m.sessionID, turnID)
}

func activeTurnID(turns []appserver.Turn) string {
	if turn := activeTurn(turns); turn != nil {
		return turn.ID
	}
	return ""
}

func activeTurn(turns []appserver.Turn) *appserver.Turn {
	for i := len(turns) - 1; i >= 0; i-- {
		if turns[i].Status == "inProgress" || turns[i].Status == "in_progress" || turns[i].Status == "active" {
			return &turns[i]
		}
	}
	return nil
}

func (m *Model) applyEvent(event appserver.Event) {
	if m.activeTurns == nil {
		m.activeTurns = make(map[string]string)
	}
	if m.ownedThreads == nil {
		m.ownedThreads = make(map[string]bool)
	}
	if m.unread == nil {
		m.unread = make(map[string]bool)
	}
	if m.turnStarted == nil {
		m.turnStarted = make(map[string]time.Time)
	}
	switch event.Method {
	case "thread/started":
		var params struct {
			Thread appserver.Thread `json:"thread"`
		}
		if json.Unmarshal(event.Params, &params) == nil && params.Thread.ID != "" {
			m.upsertThread(params.Thread)
		}
	case "thread/name/updated":
		var params struct {
			ThreadID   string  `json:"threadId"`
			ThreadName *string `json:"threadName"`
		}
		if json.Unmarshal(event.Params, &params) == nil && params.ThreadName != nil {
			m.setThreadName(params.ThreadID, *params.ThreadName)
		}
	case "thread/status/changed":
		var params struct {
			ThreadID string           `json:"threadId"`
			Status   appserver.Status `json:"status"`
		}
		if json.Unmarshal(event.Params, &params) == nil {
			m.updateStatus(params.ThreadID, params.Status)
		}
	case "turn/started":
		var params struct {
			ThreadID string         `json:"threadId"`
			Turn     appserver.Turn `json:"turn"`
		}
		if json.Unmarshal(event.Params, &params) == nil {
			m.updateStatus(params.ThreadID, appserver.Status{Type: "active"})
			m.activeTurns[params.ThreadID] = params.Turn.ID
			if params.Turn.StartedAt != nil {
				m.turnStarted[params.ThreadID] = time.Unix(*params.Turn.StartedAt, 0)
			} else {
				m.turnStarted[params.ThreadID] = time.Now()
			}
			m.ownedThreads[params.ThreadID] = true
		}
	case "turn/completed":
		var params struct {
			ThreadID string         `json:"threadId"`
			Turn     appserver.Turn `json:"turn"`
		}
		if json.Unmarshal(event.Params, &params) == nil {
			delete(m.activeTurns, params.ThreadID)
			delete(m.turnStarted, params.ThreadID)
			m.updateStatus(params.ThreadID, appserver.Status{Type: "idle"})
			if m.sessionID != params.ThreadID {
				m.unread[params.ThreadID] = true
			}
			if m.sessionID == params.ThreadID {
				m.mergeItems(params.Turn.Items, params.Turn.ID, durationMillis(params.Turn))
			}
		}
	case "item/started":
		var params struct {
			ThreadID string          `json:"threadId"`
			TurnID   string          `json:"turnId"`
			Item     json.RawMessage `json:"item"`
		}
		if json.Unmarshal(event.Params, &params) == nil && params.ThreadID == m.sessionID {
			m.mergeItemsWithStatus([]json.RawMessage{params.Item}, params.TurnID, 0, "inProgress")
		}
	case "item/agentMessage/delta":
		var params struct{ ThreadID, TurnID, ItemID, Delta string }
		if json.Unmarshal(event.Params, &params) == nil && params.ThreadID == m.sessionID {
			m.appendDelta(params.ItemID, params.TurnID, params.Delta)
		}
	case "item/commandExecution/outputDelta":
		var params struct{ ThreadID, ItemID, Delta string }
		if json.Unmarshal(event.Params, &params) == nil && params.ThreadID == m.sessionID {
			m.appendActivityOutput(params.ItemID, params.Delta)
		}
	case "item/autoApprovalReview/started", "item/autoApprovalReview/completed":
		m.mergeApprovalReview(event)
	case "item/completed":
		var params struct {
			ThreadID string          `json:"threadId"`
			TurnID   string          `json:"turnId"`
			Item     json.RawMessage `json:"item"`
		}
		if json.Unmarshal(event.Params, &params) == nil && params.ThreadID == m.sessionID {
			m.mergeItemsWithStatus([]json.RawMessage{params.Item}, params.TurnID, 0, "completed")
		}
	case "item/commandExecution/requestApproval", "item/fileChange/requestApproval", "item/tool/requestUserInput", "item/permissions/requestApproval":
		var params struct {
			ThreadID string `json:"threadId"`
		}
		if json.Unmarshal(event.Params, &params) == nil {
			m.updateStatus(params.ThreadID, appserver.Status{Type: "active", ActiveFlags: []string{"waitingOnApproval"}})
			if params.ThreadID == m.sessionID {
				m.status = "approval or input needed — open this thread in native Codex to answer"
			}
		}
	case "connection/closed":
		m.err = m.client.Err()
	}
}

func (m *Model) mergeApprovalReview(event appserver.Event) {
	var params struct {
		ThreadID string `json:"threadId"`
		TurnID   string `json:"turnId"`
		ReviewID string `json:"reviewId"`
		Review   struct {
			Status            string `json:"status"`
			RiskLevel         string `json:"riskLevel"`
			UserAuthorization string `json:"userAuthorization"`
			Rationale         string `json:"rationale"`
		} `json:"review"`
		Action struct {
			Type     string   `json:"type"`
			Command  string   `json:"command"`
			Program  string   `json:"program"`
			Argv     []string `json:"argv"`
			Target   string   `json:"target"`
			ToolName string   `json:"toolName"`
		} `json:"action"`
	}
	if json.Unmarshal(event.Params, &params) != nil || params.ThreadID != m.sessionID {
		return
	}
	text := params.Action.Command
	if text == "" && params.Action.Program != "" {
		text = strings.Join(params.Action.Argv, " ")
	}
	if text == "" {
		text = params.Action.Target
	}
	if text == "" {
		text = params.Action.ToolName
	}
	if text == "" {
		text = splitCamelCase(params.Action.Type)
	}
	m.mergeMessage(chatMessage{
		ID: params.ReviewID, Role: "activity", Kind: "review", Text: text,
		Status: params.Review.Status, Detail: params.Review.Rationale, TurnID: params.TurnID,
		RiskLevel: params.Review.RiskLevel, Authorization: params.Review.UserAuthorization,
	})
}

func (m *Model) appendDelta(id, turnID, delta string) {
	for i := range m.messages {
		if m.messages[i].ID == id {
			before := len(wrap(m.messages[i].Text, max(20, m.width-4)))
			m.messages[i].Text += delta
			m.rememberRecap(m.messages[i])
			m.invalidateTranscript(i)
			if m.scrollOffset > 0 {
				after := len(wrap(m.messages[i].Text, max(20, m.width-4)))
				m.scrollOffset += max(0, after-before)
			}
			return
		}
	}
	messageIndex := len(m.messages)
	m.messages = append(m.messages, chatMessage{ID: id, Role: "assistant", Kind: "message", TurnID: turnID, Text: delta})
	m.rememberRecap(m.messages[len(m.messages)-1])
	m.invalidateTranscript(messageIndex)
	if m.scrollOffset > 0 {
		m.scrollOffset += len(wrap(delta, max(20, m.width-4))) + 1
	}
}

func (m *Model) appendActivityOutput(id, delta string) {
	for i := range m.messages {
		if m.messages[i].ID == id {
			m.messages[i].Detail += delta
			m.invalidateTranscript(i)
			return
		}
	}
}

func (m *Model) mergeItems(items []json.RawMessage, turnID string, durationMS int64) {
	m.mergeItemsWithStatus(items, turnID, durationMS, "")
}

func (m *Model) mergeItemsWithStatus(items []json.RawMessage, turnID string, durationMS int64, status string) {
	// Completion duration changes the separator rendered before this turn's
	// first output, which may precede every item in the completion payload.
	if durationMS > 0 {
		for index, existing := range m.messages {
			if existing.TurnID == turnID && existing.Role != "user" {
				m.invalidateTranscript(index)
				break
			}
		}
	}
	for _, item := range items {
		message, ok := messageFromItem(item)
		if !ok {
			continue
		}
		message.TurnID = turnID
		message.TurnDurationMS = durationMS
		if status != "" && (message.Kind == "web" || message.Status == "") {
			message.Status = status
		}
		m.mergeMessage(message)
	}
}

func (m *Model) mergeMessage(message chatMessage) {
	m.rememberRecap(message)
	if message.Role == "user" && message.ID != "" {
		for i := len(m.messages) - 1; i >= 0; i-- {
			if m.messages[i].Role == "user" && m.messages[i].ID == "" && m.messages[i].Text == message.Text {
				m.messages[i] = message
				m.invalidateTranscript(i)
				return
			}
		}
	}
	for i := range m.messages {
		if message.ID != "" && m.messages[i].ID == message.ID {
			m.messages[i] = message
			m.invalidateTranscript(i)
			return
		}
	}
	messageIndex := len(m.messages)
	m.messages = append(m.messages, message)
	m.invalidateTranscript(messageIndex)
}

func (m *Model) removeOptimisticMessage(index int, text string) {
	if index < 0 || index >= len(m.messages) {
		return
	}
	message := m.messages[index]
	if message.Role != "user" || message.ID != "" || message.Text != text {
		return
	}
	m.messages = append(m.messages[:index], m.messages[index+1:]...)
	m.invalidateTranscript(index)
}

func (m Model) hasUserMessage(text string) bool {
	for i := len(m.messages) - 1; i >= 0; i-- {
		if m.messages[i].Role == "user" {
			return m.messages[i].Text == text
		}
	}
	return false
}

func (m *Model) restoreDraft(text string) {
	if text == "" {
		return
	}
	if len(m.input) == 0 {
		m.input = []rune(text)
	} else {
		m.input = append([]rune(text+"\n"), m.input...)
	}
	m.cursor = len(m.input)
	m.hasSelection = false
}

func (m *Model) replaceMessages(messages []chatMessage) {
	common := min(len(m.messages), len(messages))
	dirty := common
	for i := 0; i < common; i++ {
		left, right := m.messages[i], messages[i]
		if left.ID != right.ID || left.Role != right.Role || left.Text != right.Text || left.Kind != right.Kind ||
			left.Status != right.Status || left.Detail != right.Detail || left.Phase != right.Phase ||
			left.RiskLevel != right.RiskLevel || left.Authorization != right.Authorization ||
			left.TurnID != right.TurnID || left.TurnDurationMS != right.TurnDurationMS ||
			!slices.Equal(left.Changes, right.Changes) || !slices.Equal(left.Actions, right.Actions) {
			dirty = i
			break
		}
	}
	m.messages = messages
	for i := len(messages) - 1; i >= 0; i-- {
		if recap := recapForMessage(messages[i]); recap != "" {
			if m.recaps == nil {
				m.recaps = make(map[string]string)
			}
			m.recaps[m.sessionID] = recap
			break
		}
	}
	cachedEntries := 0
	if m.transcript != nil {
		cachedEntries = len(m.transcript.entries)
	}
	if dirty < len(messages) || dirty < cachedEntries {
		m.invalidateTranscript(dirty)
	}
}

func messagesFromTurns(turns []appserver.Turn) []chatMessage {
	var messages []chatMessage
	seen := make(map[string]bool)
	for _, turn := range turns {
		for _, item := range turn.Items {
			message, ok := messageFromItem(item)
			if ok && !seen[message.ID] {
				message.TurnID = turn.ID
				message.TurnDurationMS = durationMillis(turn)
				messages = append(messages, message)
				seen[message.ID] = true
			}
		}
	}
	return messages
}

func mergeRolloutReviews(messages []chatMessage, reviews []rolloutReview) []chatMessage {
	if len(reviews) == 0 {
		return messages
	}
	byTurn := make(map[string][]rolloutReview)
	for _, review := range reviews {
		byTurn[review.TurnID] = append(byTurn[review.TurnID], review)
	}
	merged := make([]chatMessage, 0, len(messages)+len(reviews))
	for _, message := range messages {
		queue := byTurn[message.TurnID]
		if message.Kind == "command" && len(queue) > 0 {
			alreadyPresent := len(merged) > 0 && merged[len(merged)-1].Kind == "review" && merged[len(merged)-1].TurnID == message.TurnID
			if !alreadyPresent {
				merged = append(merged, chatMessage{
					ID: queue[0].ID, Role: "activity", Kind: "review", Status: "approved",
					Text: message.Text, TurnID: message.TurnID,
				})
			}
			byTurn[message.TurnID] = queue[1:]
		}
		merged = append(merged, message)
	}
	return merged
}

func messageFromItem(raw json.RawMessage) (chatMessage, bool) {
	var item struct {
		ID               string                        `json:"id"`
		Type             string                        `json:"type"`
		Text             string                        `json:"text"`
		Phase            string                        `json:"phase"`
		Command          string                        `json:"command"`
		CommandActions   []commandAction               `json:"commandActions"`
		Status           string                        `json:"status"`
		AggregatedOutput string                        `json:"aggregatedOutput"`
		Server           string                        `json:"server"`
		Tool             string                        `json:"tool"`
		Changes          []json.RawMessage             `json:"changes"`
		Summary          []string                      `json:"summary"`
		Content          []struct{ Type, Text string } `json:"content"`
		Query            string                        `json:"query"`
		Action           struct {
			Type    string   `json:"type"`
			Query   string   `json:"query"`
			Queries []string `json:"queries"`
			URL     string   `json:"url"`
			Pattern string   `json:"pattern"`
		} `json:"action"`
	}
	if json.Unmarshal(raw, &item) != nil {
		return chatMessage{}, false
	}
	switch item.Type {
	case "agentMessage":
		return chatMessage{ID: item.ID, Role: "assistant", Kind: "message", Text: item.Text, Phase: item.Phase}, true
	case "userMessage":
		var text []string
		for _, input := range item.Content {
			if input.Type == "text" {
				text = append(text, input.Text)
			}
		}
		return chatMessage{ID: item.ID, Role: "user", Kind: "message", Text: strings.Join(text, "\n")}, true
	case "commandExecution":
		return chatMessage{ID: item.ID, Role: "activity", Kind: "command", Text: item.Command, Status: item.Status, Detail: item.AggregatedOutput, Actions: item.CommandActions}, true
	case "webSearch":
		text := item.Query
		switch item.Action.Type {
		case "search":
			if item.Action.Query != "" {
				text = item.Action.Query
			}
			if text == "" && len(item.Action.Queries) > 0 {
				text = strings.Join(item.Action.Queries, ", ")
			}
		case "openPage":
			text = item.Action.URL
		case "findInPage":
			text = item.Action.Pattern
		}
		return chatMessage{ID: item.ID, Role: "activity", Kind: "web", Text: text, Status: "completed"}, true
	case "mcpToolCall":
		return chatMessage{ID: item.ID, Role: "activity", Kind: "mcp", Text: item.Server + "." + item.Tool, Status: item.Status}, true
	case "dynamicToolCall":
		return chatMessage{ID: item.ID, Role: "activity", Kind: "tool", Text: item.Tool, Status: item.Status}, true
	case "fileChange":
		changes := parseFileChanges(item.Changes)
		label := fmt.Sprintf("%d files", len(changes))
		if len(changes) == 1 {
			label = changes[0].Path
		}
		return chatMessage{ID: item.ID, Role: "activity", Kind: "file", Text: label, Status: item.Status, Changes: changes}, true
	case "reasoning":
		if len(item.Summary) == 0 {
			return chatMessage{}, false
		}
		return chatMessage{ID: item.ID, Role: "assistant", Kind: "message", Text: strings.Join(item.Summary, "\n")}, true
	default:
		label := splitCamelCase(item.Type)
		if label == "" {
			return chatMessage{}, false
		}
		return chatMessage{ID: item.ID, Role: "activity", Kind: "tool", Text: label, Status: item.Status}, true
	}
}

func durationMillis(turn appserver.Turn) int64 {
	if turn.DurationMS != nil {
		return *turn.DurationMS
	}
	if turn.StartedAt != nil && turn.CompletedAt != nil {
		return (*turn.CompletedAt - *turn.StartedAt) * 1000
	}
	return 0
}

func splitCamelCase(value string) string {
	var b strings.Builder
	for i, r := range value {
		if i > 0 && r >= 'A' && r <= 'Z' {
			b.WriteByte(' ')
		}
		b.WriteRune(r)
	}
	return b.String()
}

func (m *Model) mergeThreads(threads []appserver.Thread) {
	for _, thread := range threads {
		m.upsertThread(thread)
	}
	if m.selected >= len(m.threads) && m.selected > 0 {
		m.selected--
	}
}

func (m *Model) mergeDiscoveredThreads(threads []appserver.Thread) {
	for _, thread := range threads {
		if existing, ok := m.threadByID(thread.ID); ok {
			// Runtime events and rollout probes are fresher than a background list
			// response, especially for threads owned by another Codex process.
			if existing.Status.Type == "active" && thread.Status.Type != "active" {
				thread.Status = existing.Status
			}
		}
		m.upsertThread(thread)
	}
}

func (m *Model) setThreadName(id, name string) {
	for i := range m.threads {
		if m.threads[i].ID == id {
			value := name
			m.threads[i].Name = &value
			return
		}
	}
}

func (m *Model) upsertThread(thread appserver.Thread) {
	for i := range m.threads {
		if m.threads[i].ID == thread.ID {
			m.threads[i] = thread
			return
		}
	}
	m.threads = append(m.threads, thread)
}

func (m *Model) updateStatus(id string, status appserver.Status) {
	for i := range m.threads {
		if m.threads[i].ID == id {
			m.threads[i].Status = status
			m.threads[i].UpdatedAt = time.Now().Unix()
			return
		}
	}
}

func (m Model) orderedThreads() []appserver.Thread {
	threads := append([]appserver.Thread(nil), m.threads...)
	sort.SliceStable(threads, func(i, j int) bool {
		if m.groupByProject {
			pi := projectFor(threads[i].Cwd, m.projectsRoot)
			pj := projectFor(threads[j].Cwd, m.projectsRoot)
			if pi.Label != pj.Label {
				return pi.Label < pj.Label
			}
		}
		ri, rj := m.rank(threads[i]), m.rank(threads[j])
		if ri != rj {
			return ri < rj
		}
		return threads[i].UpdatedAt > threads[j].UpdatedAt
	})
	return threads
}

func (m *Model) moveListSelection(delta int) {
	last := len(m.orderedThreads()) - 1
	if last < 0 {
		m.selected = 0
		return
	}
	m.selected = max(0, min(last, m.selected+delta))
}

func (m Model) rank(thread appserver.Thread) int {
	if needsInput(thread.Status) {
		return 0
	}
	if thread.Status.Type == "active" {
		return 1
	}
	if m.unread[thread.ID] {
		return 2
	}
	if thread.Status.Type == "systemError" {
		return 4
	}
	return 3
}

func needsInput(status appserver.Status) bool {
	for _, flag := range status.ActiveFlags {
		if flag == "waitingOnApproval" || flag == "waitingOnUserInput" {
			return true
		}
	}
	return false
}

func (m Model) View() tea.View {
	// Never let a styled/path-heavy row wrap in the terminal itself. Native
	// wrapping would create physical rows outside our viewport and expose the
	// shell's scrollback above the alternate-screen session.
	content := clipViewWidth(m.viewContent(), m.width)
	view := tea.NewView(content)
	view.AltScreen = true
	if m.mode == listMode {
		// Capture overview wheel events so terminals cannot reveal scrollback
		// above the alternate screen. The handler clamps them to list selection.
		view.MouseMode = tea.MouseModeCellMotion
	} else {
		// Capture session wheel events explicitly. Some terminals ignore DECSET
		// 1007 after a mouse-mode transition and scroll their primary history even
		// while an alternate screen is active. Cell-motion mode makes the bounded
		// transcript handler authoritative; Shift+drag remains the standard native
		// terminal-selection escape hatch for transcript text.
		view.MouseMode = tea.MouseModeCellMotion
	}
	view.KeyboardEnhancements.ReportAlternateKeys = true
	return view
}

func (m Model) viewContent() string {
	if m.width == 0 {
		return "Starting Codex agents…"
	}
	if m.showHelp {
		return m.helpView()
	}
	if m.mode == sessionMode {
		return m.sessionView()
	}
	return m.listView()
}

func (m Model) listView() string {
	var b strings.Builder
	fmt.Fprintf(&b, "%s%sCodex agents%s%s%*s%d sessions%s\n\n", bold, magenta, reset, dim, max(1, m.width-29), "", len(m.threads), reset)
	threads := m.orderedThreads()
	var rows []string
	selectedLine := 0
	lastRank := -1
	lastProject := ""
	for i, thread := range threads {
		rank := m.rank(thread)
		project := projectFor(thread.Cwd, m.projectsRoot)
		newGroup := (!m.groupByProject && rank != lastRank) || (m.groupByProject && project.Label != lastProject)
		if newGroup {
			if lastRank >= 0 || lastProject != "" {
				rows = append(rows, "")
			}
			groupName := sectionName(rank)
			if m.groupByProject {
				groupName = project.Label
			}
			rows = append(rows, bold+groupName+reset)
			lastRank = rank
			lastProject = project.Label
		}
		selected := "  "
		if i == m.selected {
			selected = cyan + "› " + reset
		}
		icon, color, state := stateFor(thread, m.unread[thread.ID])
		titleWidth := min(34, max(12, m.width-30))
		title := truncate(threadTitle(thread), titleWidth)
		const statusWidth = 11
		fixedWidth := 2 + 2 + titleWidth + 2 + statusWidth + 3
		recapWidth := max(0, m.width-fixedWidth)
		recap := truncate(m.threadRecap(thread), recapWidth)
		row := fmt.Sprintf("%s%s%s%s %-*s  %s%-*s%s · %s%s%s", selected, color, icon, reset, titleWidth, title, color, statusWidth, state, reset, dim, recap, reset)
		if i == m.selected {
			selectedLine = len(rows)
		}
		rows = append(rows, row)
	}
	if len(threads) == 0 {
		rows = append(rows, dim+"  No sessions yet."+reset)
	}
	composer, composerRows := m.composer("describe a new task…")
	popup, popupRows := m.commandPopup()
	reservedRows := composerRows + popupRows + 1
	availableRows := max(1, m.height-2-reservedRows)
	start := 0
	if len(rows) > availableRows {
		start = max(0, min(len(rows)-availableRows, selectedLine-availableRows/2))
		rows = rows[start : start+availableRows]
	}
	for _, row := range rows {
		b.WriteString(row)
		b.WriteByte('\n')
	}
	padToBottom(&b, m.height, reservedRows)
	writeLines(&b, popup)
	writeLines(&b, composer)
	group := "projects"
	if !m.groupByProject {
		group = "status"
	}
	m.writeFooter(&b, fmt.Sprintf("↑↓ select   →/enter open   g grouping:%s   esc quit", group))
	return b.String()
}

func (m *Model) sessionView() string {
	thread, _ := m.threadByID(m.sessionID)
	layout := m.ensureTranscriptLayout(thread)
	anchors := layout.anchors
	composer, composerRows := m.composer("message Codex…")
	popup, popupRows := m.commandPopup()
	working := m.workingStatus()
	ownership := m.ownershipNotice()
	start, end, stickyStart, maxOffset := m.transcriptViewport(layout)
	offset := min(m.scrollOffset, maxOffset)
	visibleLines := layout.rows(start, end)
	visibleLines = m.highlightTranscriptSelection(visibleLines, start)
	workingRows := 0
	if working != "" {
		workingRows = 1
	}
	ownershipRows := 0
	if ownership != "" {
		ownershipRows = 1
	}
	reservedRows := composerRows + popupRows + workingRows + ownershipRows + 1
	var b strings.Builder
	header := threadTitle(thread)
	headerColor := ""
	if m.closeConfirmationArmed(time.Now()) {
		header = closeConfirmationText
		headerColor = red
	}
	styledHeader := headerColor + bold + truncate(header, max(12, m.width-32)) + reset
	fmt.Fprintf(&b, "%s%s←%s  %s  %s%s%s\n", cyan, bold, reset, styledHeader, dim, filepath.Base(thread.Cwd), reset)
	b.WriteString(stickyPrompt(anchors, stickyStart, m.width))
	b.WriteByte('\n')
	writeLines(&b, visibleLines)
	padToBottom(&b, m.height, reservedRows)
	writeLines(&b, popup)
	if ownership != "" {
		b.WriteString(ownership)
		b.WriteByte('\n')
	}
	if working != "" {
		b.WriteString(working)
		b.WriteByte('\n')
	}
	writeLines(&b, composer)
	m.writeFooter(&b, joinFooter("↑↓/wheel scroll", "drag select/copy", "pgup/pgdn", "← back", "enter send", "ctrl+c interrupt", scrollStatus(offset, maxOffset)))
	return b.String()
}

func (m Model) ownershipNotice() string {
	if !m.writerBusy[m.sessionID] {
		return ""
	}
	return red + "• Standalone Codex owns this thread • reopen it with: codex resume --remote unix:// " + m.sessionID + reset
}

func (m Model) helpView() string {
	if m.nativeSessions {
		lines := []string{
			bold + magenta + "Codex agents shortcuts" + reset,
			"",
			bold + "Overview" + reset,
			"  ↑/↓ select · →/Enter open · g change grouping · Ctrl+R refresh",
			"  Type a prompt and press Enter to start it in native Codex",
			"  ←/→ or Ctrl+B/F move · Alt+B/F move by word · Home/End or Ctrl+A/E",
			"  Shift+←/→ select · Alt+Backspace/Delete word · Ctrl+C clears input",
			"  Ctrl+X twice within 3s closes the selected session subscription",
			"  / commands · ? shortcuts · Ctrl+D quit · double Ctrl+C quits overview",
			"",
			bold + "Native session" + reset,
			"  The installed Codex TUI owns all rendering, commands, and keybindings.",
			"  ← returns here immediately; /quit also exits and refreshes the overview.",
			"  A session already open in standalone Codex must be closed there first.",
			"",
			dim + "Press ? or Esc to close" + reset,
		}
		if len(lines) > m.height {
			lines = lines[:m.height]
		}
		return strings.Join(lines, "\n")
	}
	lines := []string{
		bold + magenta + "Codex agents shortcuts" + reset,
		"",
		bold + "Composer" + reset,
		"  Enter submit/open · Shift/Alt+Enter newline · Tab complete",
		"  ←/→ or Ctrl+B/F move · Alt+B/F move by word · Home/End or Ctrl+A/E",
		"  Backspace/Delete · Alt+Backspace/Delete word · Ctrl+U/K kill · Ctrl+Y yank",
		"  ↑/↓ or Ctrl+P/N history/popup · Shift+←/→ select · Ctrl+C clear/interrupt",
		"",
		bold + "Agent view" + reset,
		"  ↑/↓ select · →/Enter open · ←/Esc back · g change grouping · Ctrl+R refresh",
		"  Ctrl+X twice within 3s closes the open session; opening it later resumes it",
		"  Shared writer: launch native sessions with `codex --remote unix://`",
		"  Existing standalone session: reopen with `codex resume --remote unix:// <id>`",
		"  Ctrl+T expand/collapse tool output · drag selects and copies transcript text",
		"  / commands · ? shortcuts · Ctrl+D quit · double Ctrl+C quits overview",
		"",
		bold + "Command compatibility" + reset,
		"  Here: /new /resume /rename /status /stop /clear /help /quit /exit",
		"  Native-only entries stay discoverable and are labelled in the palette.",
		"",
		dim + "Press ? or Esc to close" + reset,
	}
	if len(lines) > m.height {
		lines = lines[:m.height]
	}
	return strings.Join(lines, "\n")
}

func (m Model) writeFooter(b *strings.Builder, help string) {
	if m.err != nil {
		fmt.Fprintf(b, "%s%s%s", red, truncate(m.err.Error(), max(20, m.width)), reset)
		return
	}
	if m.status != "" {
		fmt.Fprintf(b, "%s%s%s", cyan, truncate(m.status, max(20, m.width)), reset)
		return
	}
	if m.loading {
		fmt.Fprintf(b, "%sworking…%s", dim, reset)
		return
	}
	fmt.Fprintf(b, "%s%s%s", dim, help, reset)
}

func (m Model) threadByID(id string) (appserver.Thread, bool) {
	for _, thread := range m.threads {
		if thread.ID == id {
			return thread, true
		}
	}
	return appserver.Thread{}, false
}

func stateFor(thread appserver.Thread, unread bool) (string, string, string) {
	for _, flag := range thread.Status.ActiveFlags {
		switch flag {
		case "waitingOnApproval":
			return "!", cyan, "Needs Input"
		case "waitingOnUserInput":
			return "!", cyan, "Needs Input"
		}
	}
	switch thread.Status.Type {
	case "active":
		return "●", cyan, "Working"
	case "systemError":
		return "×", red, "Failed"
	case "notLoaded":
		return "○", dim, "Idle"
	}
	if unread {
		return "✓", green, "Done"
	}
	return "○", dim, "Idle"
}

func (m Model) threadRecap(thread appserver.Thread) string {
	for _, flag := range thread.Status.ActiveFlags {
		switch flag {
		case "waitingOnApproval":
			return "Waiting for approval"
		case "waitingOnUserInput":
			if recap := clean(m.recaps[thread.ID]); strings.HasPrefix(recap, "Waiting for input:") {
				return recap
			}
			return "Waiting for your response"
		}
	}
	if recap := clean(m.recaps[thread.ID]); recap != "" {
		return recap
	}
	if preview := clean(thread.Preview); preview != "" {
		return "Last prompt: " + preview
	}
	switch thread.Status.Type {
	case "active":
		return "Working on the current turn"
	case "systemError":
		return "The session stopped with an error"
	default:
		return "No recent activity"
	}
}

func (m *Model) rememberRecap(message chatMessage) {
	if m.sessionID == "" {
		return
	}
	recap := recapForMessage(message)
	if recap == "" {
		return
	}
	if m.recaps == nil {
		m.recaps = make(map[string]string)
	}
	m.recaps[m.sessionID] = recap
}

func recapForMessage(message chatMessage) string {
	text := clean(message.Text)
	if text == "" {
		return ""
	}
	if message.Role == "user" {
		return "Asked: " + text
	}
	if message.Role == "assistant" {
		return text
	}
	switch message.Kind {
	case "command":
		return "Running: " + text
	case "web":
		return "Searching: " + text
	case "file":
		return "Editing: " + text
	case "mcp", "tool":
		return "Using: " + text
	case "review":
		return "Reviewing: " + text
	default:
		return text
	}
}

func sectionName(rank int) string {
	switch rank {
	case 0:
		return "Needs input"
	case 1:
		return "Working"
	case 2:
		return "Done"
	case 4:
		return "Failed"
	default:
		return "Idle"
	}
}

func threadTitle(thread appserver.Thread) string {
	if thread.Name != nil && strings.TrimSpace(*thread.Name) != "" {
		return clean(*thread.Name)
	}
	if strings.TrimSpace(thread.Preview) != "" {
		return clean(thread.Preview)
	}
	if thread.Cwd != "" {
		return filepath.Base(thread.Cwd)
	}
	if len(thread.ID) > 8 {
		return thread.ID[:8]
	}
	return thread.ID
}

func clean(s string) string { return strings.Join(strings.Fields(s), " ") }

func relativeAge(timestamp int64) string {
	if timestamp <= 0 {
		return "—"
	}
	d := time.Since(time.Unix(timestamp, 0))
	if d < 0 {
		d = 0
	}
	switch {
	case d < time.Minute:
		return fmt.Sprintf("%ds", int(d.Seconds()))
	case d < time.Hour:
		return fmt.Sprintf("%dm", int(d.Minutes()))
	case d < 24*time.Hour:
		return fmt.Sprintf("%dh", int(d.Hours()))
	default:
		return fmt.Sprintf("%dd", int(d.Hours()/24))
	}
}

func truncate(s string, width int) string {
	if width <= 1 {
		return ""
	}
	if utf8.RuneCountInString(s) <= width {
		return s
	}
	r := []rune(s)
	return string(r[:width-1]) + "…"
}

func wrap(text string, width int) []string {
	if width < 1 {
		return nil
	}
	text = expandTranscriptTabs(text)
	var result []string
	for _, paragraph := range strings.Split(text, "\n") {
		if paragraph == "" {
			result = append(result, "")
			continue
		}
		for len([]rune(paragraph)) > width {
			r := []rune(paragraph)
			cut := width
			for cut > width/2 && r[cut] != ' ' {
				cut--
			}
			if cut <= width/2 {
				cut = width
			}
			result = append(result, strings.TrimSpace(string(r[:cut])))
			paragraph = strings.TrimSpace(string(r[cut:]))
		}
		result = append(result, paragraph)
	}
	return result
}

func CurrentDirectory() string {
	cwd, err := os.Getwd()
	if err != nil {
		return "."
	}
	return cwd
}

type project struct {
	Label string
	Root  string
}

func defaultProjectsRoot() string {
	if configured := strings.TrimSpace(os.Getenv("CODEX_AGENTS_PROJECTS_DIR")); configured != "" {
		return filepath.Clean(configured)
	}
	home, err := os.UserHomeDir()
	if err != nil {
		return ""
	}
	return filepath.Join(home, "Projects")
}

func projectFor(cwd, projectsRoot string) project {
	cwd = filepath.Clean(cwd)
	projectsRoot = filepath.Clean(projectsRoot)
	if cwd != "." && projectsRoot != "." && projectsRoot != "" {
		relative, err := filepath.Rel(projectsRoot, cwd)
		inside := err == nil && relative != "." && relative != ".." && !strings.HasPrefix(relative, ".."+string(filepath.Separator))
		if inside {
			name := strings.Split(relative, string(filepath.Separator))[0]
			return project{Label: name, Root: filepath.Join(projectsRoot, name)}
		}
	}
	label := filepath.Base(cwd)
	if label == "." || label == string(filepath.Separator) || label == "" {
		label = "Other"
	}
	return project{Label: label, Root: cwd}
}

func (m Model) selectedThreadID() string {
	threads := m.orderedThreads()
	if m.selected < 0 || m.selected >= len(threads) {
		return ""
	}
	return threads[m.selected].ID
}

func (m *Model) selectThread(id string) {
	if id == "" {
		return
	}
	for i, thread := range m.orderedThreads() {
		if thread.ID == id {
			m.selected = i
			return
		}
	}
}

func (m *Model) clearInput() {
	m.input = nil
	m.cursor = 0
	m.hasSelection = false
	m.popupSelected = 0
	m.historyIndex = len(m.history)
	m.historyBuffers = nil
}

func (m *Model) selection() (int, int, bool) {
	if !m.hasSelection || m.selectionAnchor == m.cursor {
		return 0, 0, false
	}
	start, end := m.selectionAnchor, m.cursor
	if start > end {
		start, end = end, start
	}
	return start, end, true
}

func (m *Model) deleteSelection() bool {
	start, end, ok := m.selection()
	if !ok {
		return false
	}
	m.input = append(m.input[:start], m.input[end:]...)
	m.cursor = start
	m.hasSelection = false
	return true
}

func (m *Model) insertRunes(runes []rune) {
	m.deleteSelection()
	inserted := append([]rune(nil), runes...)
	m.input = append(m.input, make([]rune, len(inserted))...)
	copy(m.input[m.cursor+len(inserted):], m.input[m.cursor:len(m.input)-len(inserted)])
	copy(m.input[m.cursor:], inserted)
	m.cursor += len(inserted)
	m.hasSelection = false
	m.lastEmptyCtrlC = time.Time{}
}

func (m *Model) backspace() {
	if m.deleteSelection() || m.cursor == 0 {
		return
	}
	m.input = append(m.input[:m.cursor-1], m.input[m.cursor:]...)
	m.cursor--
}

func (m *Model) deleteWordBackward() {
	if m.deleteSelection() || m.cursor == 0 {
		return
	}
	start := m.cursor
	for start > 0 && isWordSpace(m.input[start-1]) {
		start--
	}
	for start > 0 && !isWordSpace(m.input[start-1]) {
		start--
	}
	m.input = append(m.input[:start], m.input[m.cursor:]...)
	m.cursor = start
	m.hasSelection = false
}

func isWordSpace(r rune) bool {
	return r == ' ' || r == '\t' || r == '\n'
}

func (m *Model) moveCursor(delta int, selecting bool) {
	m.moveCursorTo(max(0, min(len(m.input), m.cursor+delta)), selecting)
}

func (m *Model) moveCursorTo(position int, selecting bool) {
	position = max(0, min(len(m.input), position))
	if selecting {
		if !m.hasSelection {
			m.selectionAnchor = m.cursor
			m.hasSelection = true
		}
	} else {
		m.hasSelection = false
	}
	m.cursor = position
}

func padToBottom(b *strings.Builder, height, reservedRows int) {
	target := max(0, height-reservedRows)
	lines := strings.Count(b.String(), "\n")
	for lines < target {
		b.WriteByte('\n')
		lines++
	}
}

func writeLines(b *strings.Builder, lines []string) {
	for _, line := range lines {
		b.WriteString(line)
		b.WriteByte('\n')
	}
}

func (m Model) inputLine(placeholder string) string {
	width := max(1, m.width)
	inputBG, selectionBG := composerBackgrounds()
	var b strings.Builder
	b.WriteString(inputBG)
	b.WriteString(bold + "›" + reset + inputBG + " ")

	start, end, selected := m.selection()
	if len(m.input) == 0 {
		b.WriteString("\x1b[7m \x1b[27m")
		b.WriteString(dim + placeholder + reset + inputBG)
	} else {
		for i, r := range m.input {
			if selected && i >= start && i < end {
				b.WriteString(selectionBG)
			} else {
				b.WriteString(inputBG)
			}
			if i == m.cursor {
				b.WriteString("\x1b[7m")
				b.WriteRune(r)
				b.WriteString("\x1b[27m")
			} else {
				b.WriteRune(r)
			}
		}
		if m.cursor == len(m.input) {
			b.WriteString(inputBG + "\x1b[7m \x1b[27m")
		}
	}

	used := 3 + len(m.input)
	if len(m.input) == 0 {
		used += utf8.RuneCountInString(placeholder)
	}
	if used < width {
		b.WriteString(inputBG + strings.Repeat(" ", width-used))
	}
	b.WriteString(reset)
	return b.String()
}

func composerBackgrounds() (string, string) {
	if terminalLooksLight() {
		return "\x1b[48;5;254m", "\x1b[48;5;250m"
	}
	return "\x1b[48;5;236m", "\x1b[48;5;240m"
}

func terminalLooksLight() bool {
	parts := strings.Split(os.Getenv("COLORFGBG"), ";")
	if len(parts) == 0 {
		return false
	}
	background := 0
	if _, err := fmt.Sscanf(parts[len(parts)-1], "%d", &background); err != nil {
		return false
	}
	return background >= 7
}

func userMessageLine(text string, width int) string {
	background, _ := composerBackgrounds()
	return background + text + eraseToEnd + reset
}

// Ordinary trailing spaces paint the row without forcing terminal selections
// to retain copy-hostile non-breaking-space padding.
func backgroundFill(width int) string {
	if width <= 0 {
		return ""
	}
	return strings.Repeat(" ", width)
}

// EL paints the active background color to the terminal edge without placing
// literal padding characters into the selection buffer.
const eraseToEnd = "\x1b[K"
