package tui

import (
	"context"
	"encoding/json"
	"fmt"
	"strings"
	"testing"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
	"github.com/majd/codex-agents/internal/appserver"
)

func TestMessageFromItem(t *testing.T) {
	tests := []struct {
		name string
		raw  string
		role string
		text string
	}{
		{"assistant", `{"id":"a","type":"agentMessage","text":"done"}`, "assistant", "done"},
		{"user", `{"id":"u","type":"userMessage","content":[{"type":"text","text":"hello"}]}`, "user", "hello"},
	}
	for _, test := range tests {
		t.Run(test.name, func(t *testing.T) {
			message, ok := messageFromItem(json.RawMessage(test.raw))
			if !ok || message.Role != test.role || message.Text != test.text {
				t.Fatalf("got %#v, %v", message, ok)
			}
		})
	}
}

func TestWebSearchAndExplorationUseSemanticItems(t *testing.T) {
	web, ok := messageFromItem(json.RawMessage(`{"id":"w","type":"webSearch","query":"fallback","action":{"type":"findInPage","pattern":"fileChange"}}`))
	if !ok || web.Kind != "web" || web.Text != "fileChange" {
		t.Fatalf("unexpected web item: %#v", web)
	}
	command, ok := messageFromItem(json.RawMessage(`{"id":"c","type":"commandExecution","command":"sed stuff","status":"completed","commandActions":[{"type":"read","command":"sed stuff","name":"model.go","path":"/tmp/model.go"},{"type":"search","command":"rg needle","query":"needle","path":"internal"}]}`))
	if !ok || !explorable(command.Actions) {
		t.Fatalf("semantic command was not explorable: %#v", command)
	}
	view := ansi.Strip(strings.Join(renderExplored(command.Actions, 80, "/tmp"), "\n"))
	if !strings.Contains(view, "Explored") || !strings.Contains(view, "Read model.go") || !strings.Contains(view, "Search needle in internal") {
		t.Fatalf("unexpected exploration rendering: %q", view)
	}
}

func TestFileChangeRendersCodexStyleDiff(t *testing.T) {
	raw := json.RawMessage(`{
		"id":"patch","type":"fileChange","status":"completed",
		"changes":[{"path":"internal/tui/model.go","kind":{"type":"update"},"diff":"@@ -10,2 +10,2 @@\n-old := 1\n+updated := 2\n context\n"}]
	}`)
	message, ok := messageFromItem(raw)
	if !ok || len(message.Changes) != 1 {
		t.Fatalf("file change was not decoded: %#v", message)
	}
	rendered := renderActivity(message, 80, "/tmp", false)
	plain := ansi.Strip(strings.Join(rendered, "\n"))
	for _, expected := range []string{"Edited internal/tui/model.go (+1 -1)", "10 -old := 1", "10 +updated := 2", "11  context"} {
		if !strings.Contains(plain, expected) {
			t.Fatalf("diff is missing %q: %q", expected, plain)
		}
	}
	joined := strings.Join(rendered, "\n")
	if !strings.Contains(joined, diffAddBG) || !strings.Contains(joined, diffDeleteBG) || !strings.Contains(joined, diffNumber+"2") {
		t.Fatalf("diff colors or syntax highlighting are missing: %q", joined)
	}
}

func TestWebSearchLifecycleChangesVerb(t *testing.T) {
	params := func(method string) appserver.Event {
		return appserver.Event{Method: method, Params: json.RawMessage(`{"threadId":"thr","turnId":"turn","item":{"id":"web","type":"webSearch","action":{"type":"search","query":"codex app server"}}}`)}
	}
	m := Model{sessionID: "thr", activeTurns: make(map[string]string), ownedThreads: make(map[string]bool), unread: make(map[string]bool), turnStarted: make(map[string]time.Time)}
	m.applyEvent(params("item/started"))
	if len(m.messages) != 1 || m.messages[0].Status != "inProgress" {
		t.Fatalf("search did not start live: %#v", m.messages)
	}
	m.applyEvent(params("item/completed"))
	if m.messages[0].Status != "completed" {
		t.Fatalf("search did not complete: %#v", m.messages[0])
	}
	if got := ansi.Strip(strings.Join(renderActivity(m.messages[0], 80, "/tmp", false), "\n")); !strings.Contains(got, "Searched the web for codex app server") {
		t.Fatalf("wrong completed search rendering: %q", got)
	}
}

func TestNeedsInput(t *testing.T) {
	status := appserver.Status{Type: "active", ActiveFlags: []string{"waitingOnApproval"}}
	if !needsInput(status) {
		t.Fatal("waitingOnApproval should need input")
	}
	if needsInput(appserver.Status{Type: "active"}) {
		t.Fatal("plain active state should not need input")
	}
}

func TestWrapPreservesText(t *testing.T) {
	lines := wrap("one two three four", 9)
	if len(lines) < 2 {
		t.Fatalf("expected wrapping, got %#v", lines)
	}
	for _, line := range lines {
		if len([]rune(line)) > 9 {
			t.Fatalf("line is too wide: %q", line)
		}
	}
}

func TestProjectForNestedProjectsDirectory(t *testing.T) {
	got := projectFor("/Users/majd/Projects/Laurel/dashboard", "/Users/majd/Projects")
	if got.Label != "Laurel" || got.Root != "/Users/majd/Projects/Laurel" {
		t.Fatalf("got %#v", got)
	}
}

func TestProjectForOutsideProjectsDirectory(t *testing.T) {
	got := projectFor("/work/client-api", "/Users/majd/Projects")
	if got.Label != "client-api" || got.Root != "/work/client-api" {
		t.Fatalf("got %#v", got)
	}
}

func TestProjectGroupingKeepsProjectSessionsTogether(t *testing.T) {
	m := Model{
		threads: []appserver.Thread{
			{ID: "laurel-old", Cwd: "/Users/majd/Projects/Laurel/dashboard", UpdatedAt: 1},
			{ID: "other", Cwd: "/Users/majd/Projects/Other", UpdatedAt: 3},
			{ID: "laurel-new", Cwd: "/Users/majd/Projects/Laurel", UpdatedAt: 2},
		},
		unread:         make(map[string]bool),
		groupByProject: true,
		projectsRoot:   "/Users/majd/Projects",
	}
	got := m.orderedThreads()
	if got[0].ID != "laurel-new" || got[1].ID != "laurel-old" || got[2].ID != "other" {
		t.Fatalf("unexpected order: %s, %s, %s", got[0].ID, got[1].ID, got[2].ID)
	}
}

func TestOverviewRowUsesFixedTitleStatusAndRecapColumns(t *testing.T) {
	name := strings.Repeat("T", 45)
	m := Model{
		mode: listMode, width: 110, height: 12, groupByProject: true,
		projectsRoot: "/Users/majd/Projects",
		threads: []appserver.Thread{{
			ID: "thr", Name: &name, Cwd: "/Users/majd/Projects/Laurel",
			Status: appserver.Status{Type: "active"},
		}},
		recaps: map[string]string{"thr": "Running the latest verification suite"},
		unread: make(map[string]bool),
	}
	plain := ansi.Strip(m.listView())
	expected := strings.Repeat("T", 33) + "…  Working     · Running the latest verification suite"
	if !strings.Contains(plain, expected) {
		t.Fatalf("overview row does not use the requested columns: %q", plain)
	}
	if strings.Contains(plain, strings.Repeat("T", 35)) {
		t.Fatal("overview title exceeded 34 characters")
	}
}

func TestOverviewStatusVocabulary(t *testing.T) {
	tests := []struct {
		status appserver.Status
		unread bool
		want   string
	}{
		{appserver.Status{Type: "active"}, false, "Working"},
		{appserver.Status{Type: "active", ActiveFlags: []string{"waitingOnApproval"}}, false, "Needs Input"},
		{appserver.Status{Type: "idle"}, true, "Done"},
		{appserver.Status{Type: "idle"}, false, "Idle"},
		{appserver.Status{Type: "systemError"}, false, "Failed"},
	}
	for _, test := range tests {
		_, _, got := stateFor(appserver.Thread{Status: test.status}, test.unread)
		if got != test.want {
			t.Fatalf("status %#v unread=%v = %q, want %q", test.status, test.unread, got, test.want)
		}
	}
}

func TestBackgroundDiscoveryAddsThreadsWithoutMovingSelection(t *testing.T) {
	m := Model{
		threads: []appserver.Thread{
			{ID: "selected", Cwd: "/Users/majd/Projects/Laurel", UpdatedAt: 20},
			{ID: "older", Cwd: "/Users/majd/Projects/Laurel", UpdatedAt: 10},
		},
		selected: 0, groupByProject: true, projectsRoot: "/Users/majd/Projects",
	}
	selectedID := m.selectedThreadID()
	m.mergeDiscoveredThreads([]appserver.Thread{
		{ID: "new", Cwd: "/Users/majd/Projects/Laurel", UpdatedAt: 30},
		{ID: "selected", Cwd: "/Users/majd/Projects/Laurel", UpdatedAt: 20},
	})
	m.selectThread(selectedID)
	if len(m.threads) != 3 || m.selectedThreadID() != "selected" {
		t.Fatalf("discovery changed selection or missed thread: selected=%q threads=%d", m.selectedThreadID(), len(m.threads))
	}
}

func TestBackgroundDiscoveryPreservesLiveStatus(t *testing.T) {
	m := Model{threads: []appserver.Thread{{ID: "thr", Status: appserver.Status{Type: "active"}}}}
	m.mergeDiscoveredThreads([]appserver.Thread{{ID: "thr", Status: appserver.Status{Type: "notLoaded"}}})
	thread, _ := m.threadByID("thr")
	if thread.Status.Type != "active" {
		t.Fatalf("background list replaced live status with %q", thread.Status.Type)
	}
}

func TestSpaceIsInsertedInInput(t *testing.T) {
	m := Model{}
	updated, _ := m.handleKey(tea.KeyPressMsg{Code: 'h', Text: "hello"})
	m = updated.(Model)
	updated, _ = m.handleKey(tea.KeyPressMsg{Code: tea.KeySpace, Text: " "})
	m = updated.(Model)
	updated, _ = m.handleKey(tea.KeyPressMsg{Code: 'w', Text: "world"})
	got := updated.(Model)
	if string(got.input) != "hello world" {
		t.Fatalf("got %q", string(got.input))
	}
}

func TestDeleteWordBackward(t *testing.T) {
	m := Model{input: []rune("hello wide world"), cursor: len([]rune("hello wide world"))}
	m.deleteWordBackward()
	if string(m.input) != "hello wide " || m.cursor != len([]rune("hello wide ")) {
		t.Fatalf("got %q at %d", string(m.input), m.cursor)
	}
}

func TestBackspaceDeletesSelection(t *testing.T) {
	m := Model{input: []rune("hello world"), cursor: 11, selectionAnchor: 6, hasSelection: true}
	m.backspace()
	if string(m.input) != "hello " || m.cursor != 6 || m.hasSelection {
		t.Fatalf("got %q at %d (selected=%v)", string(m.input), m.cursor, m.hasSelection)
	}
}

func TestCtrlCClearsThenDoubleEmptyQuits(t *testing.T) {
	m := Model{input: []rune("draft"), cursor: 5}
	updated, cmd := m.handleKey(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	m = updated.(Model)
	if len(m.input) != 0 || cmd != nil {
		t.Fatal("ctrl+c with a draft should only clear it")
	}
	updated, cmd = m.handleKey(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	m = updated.(Model)
	if cmd != nil || m.status != "ctrl+c again to quit" {
		t.Fatal("first empty ctrl+c should arm quit")
	}
	_, cmd = m.handleKey(tea.KeyPressMsg{Code: 'c', Mod: tea.ModCtrl})
	if cmd == nil {
		t.Fatal("second empty ctrl+c should quit")
	}
	if _, ok := cmd().(tea.QuitMsg); !ok {
		t.Fatalf("expected QuitMsg, got %T", cmd())
	}
}

func TestDoubleCtrlXClosesCurrentSession(t *testing.T) {
	name := "Original session name"
	m := Model{
		mode: sessionMode, sessionID: "thr", width: 100, height: 12,
		threads:    []appserver.Thread{{ID: "thr", Name: &name, Cwd: "/tmp"}},
		transcript: newTranscriptLayout(),
	}
	updated, cmd := m.handleKey(tea.KeyPressMsg{Code: 'x', Mod: tea.ModCtrl})
	m = updated.(Model)
	view := m.sessionView()
	if cmd == nil || m.status != "" || !strings.Contains(view, red+bold+closeConfirmationText) || strings.Contains(view, name) {
		t.Fatalf("first ctrl+x did not replace the title: status=%q cmd=%v view=%q", m.status, cmd != nil, view)
	}
	updated, cmd = m.handleKey(tea.KeyPressMsg{Code: 'x', Mod: tea.ModCtrl})
	m = updated.(Model)
	if cmd == nil || !m.loading || m.status != "closing session" {
		t.Fatalf("second ctrl+x did not start close: status=%q loading=%v cmd=%v", m.status, m.loading, cmd != nil)
	}
}

func TestCtrlXConfirmationExpiresAfterThreeSeconds(t *testing.T) {
	name := "Original session name"
	armedAt := time.Now().Add(-closeConfirmationWindow - time.Millisecond)
	m := Model{
		mode: sessionMode, sessionID: "thr", width: 100, height: 12, lastCtrlX: armedAt,
		threads:    []appserver.Thread{{ID: "thr", Name: &name, Cwd: "/tmp"}},
		transcript: newTranscriptLayout(),
	}
	updated, cmd := m.handleKey(tea.KeyPressMsg{Code: 'x', Mod: tea.ModCtrl})
	m = updated.(Model)
	if cmd == nil || m.loading || !m.closeConfirmationArmed(time.Now()) {
		t.Fatal("expired ctrl+x closed instead of re-arming")
	}
	currentArmedAt := m.lastCtrlX
	updated, _ = m.Update(closeConfirmationExpiredMsg{armedAt: currentArmedAt})
	m = updated.(Model)
	if !m.lastCtrlX.IsZero() || !strings.Contains(m.sessionView(), name) || strings.Contains(m.sessionView(), closeConfirmationText) {
		t.Fatal("ctrl+x confirmation did not restore the original title")
	}
}

func TestInputIsPinnedToBottom(t *testing.T) {
	m := Model{width: 80, height: 20, unread: make(map[string]bool), groupByProject: true}
	view := m.listView()
	if got := strings.Count(view, "\n"); got != m.height-1 {
		t.Fatalf("view has %d line breaks, want %d", got, m.height-1)
	}
	background, _ := composerBackgrounds()
	if !strings.Contains(view, background) {
		t.Fatal("input background is missing")
	}
}

func TestComposerWrapsLongInputWithoutSelectablePadding(t *testing.T) {
	m := Model{width: 20, input: []rune(strings.Repeat("x", 30)), cursor: 30}
	lines, rows := m.composer("prompt")
	if rows < 5 {
		t.Fatalf("expected padding plus wrapped rows, got %d: %#v", rows, lines)
	}
	background, _ := composerBackgrounds()
	if lines[0] != background+eraseToEnd+reset || lines[len(lines)-1] != lines[0] {
		t.Fatal("composer is missing full-width vertical padding")
	}
	for _, line := range lines {
		if strings.ContainsRune(line, '\u00a0') {
			t.Fatalf("composer row contains copy-hostile non-breaking padding: %q", line)
		}
	}
	if strings.Contains(lines[0], strings.Repeat(" ", 8)) {
		t.Fatalf("composer background contains selectable padding: %q", lines[0])
	}
}

func TestArrowKeysStayInsideNonEmptyComposer(t *testing.T) {
	m := Model{mode: sessionMode, width: 30, input: []rune("first\nsecond"), cursor: len([]rune("first\nsecond")), scrollOffset: 7}
	updated, _ := m.handleKey(tea.KeyPressMsg{Code: tea.KeyUp})
	m = updated.(Model)
	if m.cursor != len([]rune("first")) || m.scrollOffset != 7 {
		t.Fatalf("up changed the wrong state: cursor=%d scroll=%d", m.cursor, m.scrollOffset)
	}
	updated, _ = m.handleKey(tea.KeyPressMsg{Code: tea.KeyDown})
	m = updated.(Model)
	if m.cursor != len([]rune("first\nsecon")) || m.scrollOffset != 7 {
		t.Fatalf("down changed the wrong state: cursor=%d scroll=%d", m.cursor, m.scrollOffset)
	}
}

func TestSessionArrowHistoryPreservesDraftAndEditedRecall(t *testing.T) {
	m := Model{
		mode: sessionMode, width: 30,
		history: []string{"first prompt", "second prompt"}, historyIndex: 2,
		input: []rune("unsent draft"), cursor: len([]rune("unsent draft")),
	}
	updated, _ := m.handleKey(tea.KeyPressMsg{Code: tea.KeyUp})
	m = updated.(Model)
	if got := string(m.input); got != "second prompt" {
		t.Fatalf("up did not recall newest prompt: %q", got)
	}
	m.insertRunes([]rune(" edited"))
	updated, _ = m.handleKey(tea.KeyPressMsg{Code: tea.KeyUp})
	m = updated.(Model)
	if got := string(m.input); got != "first prompt" {
		t.Fatalf("second up did not recall older prompt: %q", got)
	}
	updated, _ = m.handleKey(tea.KeyPressMsg{Code: tea.KeyDown})
	m = updated.(Model)
	if got := string(m.input); got != "second prompt edited" {
		t.Fatalf("down lost edits to recalled prompt: %q", got)
	}
	updated, _ = m.handleKey(tea.KeyPressMsg{Code: tea.KeyDown})
	m = updated.(Model)
	if got := string(m.input); got != "unsent draft" {
		t.Fatalf("newest history slot lost unsent draft: %q", got)
	}
}

func TestSessionArrowHistoryReturnsToEmptyNewestSlot(t *testing.T) {
	m := Model{mode: sessionMode, width: 30, history: []string{"past prompt"}, historyIndex: 1}
	updated, _ := m.handleKey(tea.KeyPressMsg{Code: tea.KeyUp})
	m = updated.(Model)
	if got := string(m.input); got != "past prompt" {
		t.Fatalf("empty composer did not recall history: %q", got)
	}
	updated, _ = m.handleKey(tea.KeyPressMsg{Code: tea.KeyDown})
	m = updated.(Model)
	if len(m.input) != 0 || m.cursor != 0 {
		t.Fatalf("down did not return to empty newest slot: %q at %d", string(m.input), m.cursor)
	}
}

func TestOpeningExistingSessionLoadsPastPromptsIntoHistory(t *testing.T) {
	m := Model{
		ownedThreads: make(map[string]bool), writerBusy: make(map[string]bool),
		unread: make(map[string]bool), activeTurns: make(map[string]string),
		turnStarted: make(map[string]time.Time), statusProbe: newSessionStatusProbe(),
		transcript: newTranscriptLayout(),
	}
	thread := appserver.Thread{ID: "thr", Turns: []appserver.Turn{
		{ID: "one", Items: []json.RawMessage{json.RawMessage(`{"id":"u1","type":"userMessage","content":[{"type":"text","text":"older"}]}`)}},
		{ID: "two", Items: []json.RawMessage{json.RawMessage(`{"id":"u2","type":"userMessage","content":[{"type":"text","text":"newer"}]}`)}},
	}}
	updated, _ := m.Update(resumedMsg{thread: thread, owned: true})
	m = updated.(Model)
	updated, _ = m.handleKey(tea.KeyPressMsg{Code: tea.KeyUp})
	m = updated.(Model)
	if got := string(m.input); got != "newer" {
		t.Fatalf("opened session did not expose its prompt history: %q (history=%#v)", got, m.history)
	}
}

func TestSessionViewCapturesWheelInsideIsolatedScreen(t *testing.T) {
	m := Model{mode: sessionMode, width: 80, height: 20}
	view := m.View()
	if !view.AltScreen {
		t.Fatal("session view did not isolate terminal scrollback")
	}
	if got := view.MouseMode; got != tea.MouseModeCellMotion {
		t.Fatalf("mouse mode = %v, want cell motion", got)
	}
}

func TestOverviewToSessionPreservesBoundedStyledLayout(t *testing.T) {
	m := Model{
		mode: sessionMode, sessionID: "thr", width: 80, height: 16,
		threads: []appserver.Thread{{ID: "thr", Cwd: "/tmp"}},
		messages: []chatMessage{
			{Role: "user", Text: "keep this prompt pinned"},
			{Role: "assistant", Text: strings.Repeat("scrollable answer ", 80)},
		},
		transcript: newTranscriptLayout(),
	}
	m.scrollConversation(8)
	view := m.View()
	background, _ := composerBackgrounds()
	if !view.AltScreen || view.MouseMode != tea.MouseModeCellMotion {
		t.Fatalf("session transition lost bounded mouse capture: alt=%v mouse=%v", view.AltScreen, view.MouseMode)
	}
	if !strings.Contains(view.Content, background+"  keep this prompt pinned") {
		t.Fatal("session transition lost the sticky prompt background")
	}
	if strings.Count(view.Content, background+eraseToEnd) < 2 {
		t.Fatal("session transition lost the full-width composer background")
	}
	if got := strings.Count(view.Content, "\n"); got != m.height-1 {
		t.Fatalf("session transition escaped its viewport: lines=%d want=%d", got, m.height-1)
	}
}

func TestSessionMouseWheelCannotEscapeConversationBounds(t *testing.T) {
	m := Model{
		mode: sessionMode, sessionID: "thr", width: 48, height: 10,
		threads:    []appserver.Thread{{ID: "thr", Cwd: "/tmp"}},
		messages:   []chatMessage{{Role: "assistant", Text: strings.Repeat("scrollable line\n", 40)}},
		transcript: newTranscriptLayout(),
	}
	for range 100 {
		updated, _ := m.Update(tea.MouseWheelMsg{Button: tea.MouseWheelUp})
		m = updated.(Model)
	}
	_, bodyRows := m.transcriptGeometry()
	wantMax := max(0, m.ensureTranscriptLayout(m.threads[0]).totalRows()-bodyRows)
	if m.scrollOffset != wantMax {
		t.Fatalf("wheel exceeded upper conversation bound: offset=%d max=%d", m.scrollOffset, wantMax)
	}
	for range 100 {
		updated, _ := m.Update(tea.MouseWheelMsg{Button: tea.MouseWheelDown})
		m = updated.(Model)
	}
	if m.scrollOffset != 0 {
		t.Fatalf("wheel exceeded lower conversation bound: offset=%d", m.scrollOffset)
	}
}

func TestOverviewCapturesAndBoundsMouseWheel(t *testing.T) {
	m := Model{
		mode: listMode, width: 80, height: 8,
		threads: []appserver.Thread{{ID: "a"}, {ID: "b"}, {ID: "c"}},
	}
	if got := m.View().MouseMode; got != tea.MouseModeCellMotion {
		t.Fatalf("overview mouse mode = %v, want cell motion", got)
	}
	for range 10 {
		updated, _ := m.Update(tea.MouseWheelMsg{Button: tea.MouseWheelUp})
		m = updated.(Model)
	}
	if m.selected != 0 {
		t.Fatalf("wheel escaped above list: selected=%d", m.selected)
	}
	for range 10 {
		updated, _ := m.Update(tea.MouseWheelMsg{Button: tea.MouseWheelDown})
		m = updated.(Model)
	}
	if m.selected != 2 {
		t.Fatalf("wheel escaped below list: selected=%d", m.selected)
	}
	if got := strings.Count(m.listView(), "\n"); got != m.height-1 {
		t.Fatalf("bounded overview has %d line breaks, want %d", got, m.height-1)
	}
}

func TestMouseSelectionIsLimitedToComposerInput(t *testing.T) {
	m := Model{mode: sessionMode, width: 30, height: 12, input: []rune("hello world"), cursor: 11}
	// Five-row framed composer starts at row 6; its text row is row 8.
	updated, _ := m.Update(tea.MouseClickMsg{X: 4, Y: 8, Button: tea.MouseLeft})
	m = updated.(Model)
	updated, _ = m.Update(tea.MouseMotionMsg{X: 9, Y: 8, Button: tea.MouseLeft})
	m = updated.(Model)
	updated, cmd := m.Update(tea.MouseReleaseMsg{X: 9, Y: 8, Button: tea.MouseLeft})
	m = updated.(Model)
	start, end, ok := m.selection()
	if !ok || string(m.input[start:end]) != "hello" || cmd == nil {
		t.Fatalf("composer drag selected the wrong value: %d:%d %v", start, end, ok)
	}
	updated, _ = m.Update(tea.MouseClickMsg{X: 4, Y: 2, Button: tea.MouseLeft})
	m = updated.(Model)
	if _, _, ok := m.selection(); ok || m.mouseSelecting || m.transcriptSelecting {
		t.Fatal("transcript click created an application selection")
	}
}

func TestConversationDragSelectsHighlightsAndCopiesWhileMouseStaysCaptured(t *testing.T) {
	m := Model{
		mode: sessionMode, sessionID: "thr", width: 80, height: 20,
		threads:    []appserver.Thread{{ID: "thr", Cwd: "/tmp"}},
		messages:   []chatMessage{{Role: "assistant", Text: "select this line"}},
		transcript: newTranscriptLayout(),
	}
	updated, _ := m.Update(tea.MouseClickMsg{X: 2, Y: transcriptBodyTop, Button: tea.MouseLeft})
	m = updated.(Model)
	updated, _ = m.Update(tea.MouseMotionMsg{X: 8, Y: transcriptBodyTop, Button: tea.MouseLeft})
	m = updated.(Model)
	if !m.transcriptSelecting || !m.transcriptSelected {
		t.Fatal("conversation drag did not establish a live selection")
	}
	_, selectionBG := composerBackgrounds()
	if view := m.sessionView(); !strings.Contains(view, selectionBG+"select") {
		t.Fatalf("conversation selection was not visibly highlighted: %q", view)
	}
	updated, cmd := m.Update(tea.MouseReleaseMsg{X: 8, Y: transcriptBodyTop, Button: tea.MouseLeft})
	m = updated.(Model)
	selectedText := m.selectedTranscriptText()
	if cmd == nil || m.transcriptSelecting || selectedText != "select" {
		t.Fatalf("conversation selection did not copy cleanly: text=%q selecting=%v cmd=%v", selectedText, m.transcriptSelecting, cmd != nil)
	}
	if got := m.View().MouseMode; got != tea.MouseModeCellMotion {
		t.Fatalf("selection disabled bounded wheel capture: %v", got)
	}
}

func TestConversationSelectionPreservesCommandSyntaxStyles(t *testing.T) {
	m := Model{
		mode: sessionMode, sessionID: "thr", width: 120, height: 20,
		threads: []appserver.Thread{{ID: "thr", Cwd: "/tmp"}},
		messages: []chatMessage{{
			Role: "activity", Kind: "command", Status: "completed",
			Text: "go test -race ./internal/... && echo 'done'",
		}},
		transcript: newTranscriptLayout(),
	}
	layout := m.ensureTranscriptLayout(m.threads[0])
	m.transcriptSelected = true
	m.transcriptAnchor = transcriptPoint{row: 0, col: 0}
	m.transcriptHead = transcriptPoint{row: 0, col: ansi.StringWidth(layout.rows(0, 1)[0])}
	rendered := m.highlightTranscriptSelection(layout.rows(0, 1), 0)[0]
	_, selectionBG := composerBackgrounds()
	for _, style := range []string{selectionBG, shellCommand, shellFlag, shellString, shellOperator} {
		if !strings.Contains(rendered, style) {
			t.Fatalf("selected command lost style %q: %q", style, rendered)
		}
	}
}

func TestConversationSelectionUsesVirtualRowsAfterScrolling(t *testing.T) {
	m := Model{
		mode: sessionMode, sessionID: "thr", width: 40, height: 12,
		threads:    []appserver.Thread{{ID: "thr", Cwd: "/tmp"}},
		messages:   []chatMessage{{Role: "assistant", Text: strings.Repeat("virtual row\n", 60)}},
		transcript: newTranscriptLayout(),
	}
	m.scrollConversation(18)
	layout := m.ensureTranscriptLayout(m.threads[0])
	start, _, _, _ := m.transcriptViewport(layout)
	updated, _ := m.Update(tea.MouseClickMsg{X: 2, Y: transcriptBodyTop, Button: tea.MouseLeft})
	m = updated.(Model)
	updated, _ = m.Update(tea.MouseMotionMsg{X: 9, Y: transcriptBodyTop, Button: tea.MouseLeft})
	m = updated.(Model)
	if m.transcriptAnchor.row != start || m.transcriptHead.row != start || m.selectedTranscriptText() != "virtual" {
		t.Fatalf("scrolled selection lost its virtual row: start=%d anchor=%#v head=%#v text=%q", start, m.transcriptAnchor, m.transcriptHead, m.selectedTranscriptText())
	}
	before := m.scrollOffset
	updated, _ = m.Update(tea.MouseWheelMsg{Button: tea.MouseWheelUp})
	m = updated.(Model)
	if m.scrollOffset <= before {
		t.Fatalf("selection broke bounded wheel scrolling: before=%d after=%d", before, m.scrollOffset)
	}
}

func TestMarkdownUsesCodexLikeStyles(t *testing.T) {
	got := strings.Join(renderMarkdown("Implemented **now** with `threadId` and [docs](https://example.com).", 120, "/tmp"), "\n")
	for _, literal := range []string{"**now**", "`threadId`", "[docs](https://example.com)"} {
		if strings.Contains(got, literal) {
			t.Fatalf("markdown punctuation was rendered literally: %q", got)
		}
	}
	if !strings.Contains(got, bold+"now"+reset) || !strings.Contains(got, cyan+"threadId"+reset) || !strings.Contains(got, "\x1b]8;;https://example.com") {
		t.Fatalf("expected Codex-like Markdown styles, got %q", got)
	}
}

func TestCodeTabsRenderAsStableSpaces(t *testing.T) {
	markdown := "```go\nfunc main() {\n\tif ready {\n\t\trun()\n\t}\n}\n```"
	rendered := ansi.Strip(strings.Join(renderMarkdown(markdown, 80, "/tmp"), "\n"))
	if strings.ContainsRune(rendered, '\t') {
		t.Fatalf("raw tab escaped into rendered code: %q", rendered)
	}
	if !strings.Contains(rendered, "      if ready {") || !strings.Contains(rendered, "          run()") {
		t.Fatalf("code indentation was not rendered at four spaces per tab: %q", rendered)
	}
	for _, row := range strings.Split(rendered, "\n") {
		if ansi.StringWidth(row) > 80 {
			t.Fatalf("tab-expanded code escaped its width: %q", row)
		}
	}
}

func TestCommandOutputAndDiffTabsRenderAsSpaces(t *testing.T) {
	activity := renderActivity(chatMessage{Role: "activity", Kind: "command", Text: "printf", Detail: "one\ttwo"}, 80, "/tmp", true)
	if got := ansi.Strip(strings.Join(activity, "\n")); strings.ContainsRune(got, '\t') || !strings.Contains(got, "one    two") {
		t.Fatalf("command-output tab was not normalized: %q", got)
	}
	diff := renderDiffBody([]renderedDiffLine{{number: 1, kind: '+', text: "\treturn true"}}, "main.go", 80)
	if got := ansi.Strip(strings.Join(diff, "\n")); strings.ContainsRune(got, '\t') || !strings.Contains(got, "    return true") {
		t.Fatalf("diff tab was not normalized: %q", got)
	}
}

func TestComposerTabRendersAsOneCellButPreservesInput(t *testing.T) {
	m := Model{mode: sessionMode, width: 40, input: []rune("a\tb"), cursor: 3}
	lines, _ := m.composer("message Codex…")
	rendered := ansi.Strip(strings.Join(lines, "\n"))
	if strings.ContainsRune(rendered, '\t') || !strings.Contains(rendered, "a b") {
		t.Fatalf("composer tab did not render as a stable cell: %q", rendered)
	}
	if string(m.input) != "a\tb" {
		t.Fatalf("composer rendering mutated input: %q", string(m.input))
	}
}

func TestAssistantTranscriptStartsWithBullet(t *testing.T) {
	m := Model{width: 80, messages: []chatMessage{{Role: "assistant", Text: "Done."}}}
	lines, _ := m.renderTranscript(appserver.Thread{Cwd: "/tmp"})
	if len(lines) != 1 || !strings.HasPrefix(lines[0], dim+"•"+reset+" ") {
		t.Fatalf("assistant transcript is missing its Codex bullet: %#v", lines)
	}
}

type recordingTurnStarter struct{ calls []string }

func (f *recordingTurnStarter) ResumeThread(_ context.Context, id string) (appserver.Thread, error) {
	f.calls = append(f.calls, "resume:"+id)
	return appserver.Thread{ID: id}, nil
}

func (f *recordingTurnStarter) StartTurn(_ context.Context, id, text string) (appserver.Turn, error) {
	f.calls = append(f.calls, "start:"+id+":"+text)
	return appserver.Turn{ID: "turn_1"}, nil
}

func (f *recordingTurnStarter) SteerTurn(_ context.Context, id, turnID, text string) (appserver.Turn, error) {
	f.calls = append(f.calls, "steer:"+id+":"+turnID+":"+text)
	return appserver.Turn{ID: turnID}, nil
}

func TestHistoricalThreadIsResumedBeforeFirstSend(t *testing.T) {
	fake := &recordingTurnStarter{}
	message := sendTurn(fake, "thread_1", "", "hello", true, -1)().(sentMsg)
	if got := strings.Join(fake.calls, ","); got != "resume:thread_1,start:thread_1:hello" {
		t.Fatalf("wrong App Server call order: %s", got)
	}
	if message.err != nil || message.turnID != "turn_1" {
		t.Fatalf("unexpected send result: %#v", message)
	}
}

func TestInputDuringActiveTurnSteersInsteadOfStartingCompetingTurn(t *testing.T) {
	fake := &recordingTurnStarter{}
	message := sendTurn(fake, "thread_1", "turn_active", "more context", false, 3)().(sentMsg)
	if got := strings.Join(fake.calls, ","); got != "steer:thread_1:turn_active:more context" {
		t.Fatalf("active turn input used wrong App Server call: %s", got)
	}
	if message.err != nil || message.turnID != "turn_active" {
		t.Fatalf("unexpected steer result: %#v", message)
	}
}

type recordingThreadOpener struct {
	calls     []string
	resumeErr error
	thread    appserver.Thread
}

func (f *recordingThreadOpener) ResumeThread(_ context.Context, id string) (appserver.Thread, error) {
	f.calls = append(f.calls, "resume:"+id)
	return f.thread, f.resumeErr
}

func (f *recordingThreadOpener) ReadThreadHistory(_ context.Context, id string) (appserver.Thread, error) {
	f.calls = append(f.calls, "read:"+id)
	return f.thread, nil
}

func TestOpeningThreadClaimsWriterBeforeShowingSession(t *testing.T) {
	fake := &recordingThreadOpener{thread: appserver.Thread{ID: "thread_1"}}
	message := resumeThread(fake, "thread_1", false)().(resumedMsg)
	if got := strings.Join(fake.calls, ","); got != "resume:thread_1,read:thread_1" {
		t.Fatalf("wrong open order: %s", got)
	}
	if message.err != nil || !message.owned || message.writerBusy {
		t.Fatalf("thread was not claimed on open: %#v", message)
	}
}

func TestOpeningExternallyOwnedThreadFallsBackToLiveReadOnlyHistory(t *testing.T) {
	fake := &recordingThreadOpener{
		thread: appserver.Thread{ID: "thread_1"}, resumeErr: appserver.ErrActiveWriter,
	}
	message := resumeThread(fake, "thread_1", false)().(resumedMsg)
	if got := strings.Join(fake.calls, ","); got != "resume:thread_1,read:thread_1" {
		t.Fatalf("writer conflict did not fall back to history: %s", got)
	}
	if message.err != nil || message.owned || !message.writerBusy || message.thread.ID != "thread_1" {
		t.Fatalf("external writer state was not retained: %#v", message)
	}
}

func TestWriterConflictRestoresOptimisticDraftWithoutGhostMessage(t *testing.T) {
	m := Model{
		mode: sessionMode, sessionID: "thread_1", width: 100, height: 14,
		messages:   []chatMessage{{Role: "user", Text: "retry me"}},
		writerBusy: make(map[string]bool), ownedThreads: map[string]bool{"thread_1": true},
		transcript: newTranscriptLayout(),
	}
	updated, _ := m.Update(sentMsg{
		threadID: "thread_1", text: "retry me", messageAt: 0, optimistic: true,
		err: appserver.ErrActiveWriter,
	})
	m = updated.(Model)
	if len(m.messages) != 0 || string(m.input) != "retry me" {
		t.Fatalf("failed send left a ghost or lost draft: messages=%#v input=%q", m.messages, string(m.input))
	}
	if !m.writerBusy["thread_1"] || m.err != nil || !strings.Contains(ansi.Strip(m.ownershipNotice()), "--remote unix://") {
		t.Fatalf("writer conflict was not explained cleanly: busy=%v err=%v notice=%q", m.writerBusy["thread_1"], m.err, m.ownershipNotice())
	}
}

func TestSuccessfulOwnershipRetryDoesNotDuplicateEarlyUserEvent(t *testing.T) {
	m := Model{
		mode: sessionMode, sessionID: "thread_1", input: []rune("retry me"), cursor: 8,
		messages:   []chatMessage{{ID: "user-event", Role: "user", Text: "retry me"}},
		writerBusy: map[string]bool{"thread_1": true}, ownedThreads: make(map[string]bool),
		activeTurns: make(map[string]string), turnStarted: make(map[string]time.Time),
	}
	updated, _ := m.Update(sentMsg{threadID: "thread_1", turnID: "turn_1", text: "retry me", messageAt: -1})
	m = updated.(Model)
	if len(m.messages) != 1 || len(m.input) != 0 || !m.ownedThreads["thread_1"] || m.writerBusy["thread_1"] {
		t.Fatalf("ownership retry duplicated or retained stale state: messages=%#v input=%q owned=%v busy=%v", m.messages, string(m.input), m.ownedThreads["thread_1"], m.writerBusy["thread_1"])
	}
}

func TestSlashCompletionAndNativeCommandGuard(t *testing.T) {
	m := Model{input: []rune("/sta"), cursor: 4}
	if !m.completeSlashCommand() || string(m.input) != "/status " {
		t.Fatalf("completion produced %q", string(m.input))
	}
	m.input, m.cursor = []rune("/model"), 6
	updated, _ := m.runSlashCommand("model", "")
	got := updated.(Model)
	if !strings.Contains(got.status, "native Codex") {
		t.Fatalf("native command was not guarded: %q", got.status)
	}
}

func TestRenameCommandAndEvent(t *testing.T) {
	m := Model{
		mode: sessionMode, sessionID: "thr",
		threads: []appserver.Thread{{ID: "thr", Preview: "old"}},
	}
	updated, cmd := m.runSlashCommand("rename", "Focused work")
	m = updated.(Model)
	if cmd == nil || !m.loading || m.status != "renaming session" {
		t.Fatalf("rename did not start: status=%q loading=%v cmd=%v", m.status, m.loading, cmd != nil)
	}
	m.applyEvent(appserver.Event{Method: "thread/name/updated", Params: json.RawMessage(`{"threadId":"thr","threadName":"Focused work"}`)})
	thread, _ := m.threadByID("thr")
	if thread.Name == nil || *thread.Name != "Focused work" || threadTitle(thread) != "Focused work" {
		t.Fatalf("rename event did not update title: %#v", thread.Name)
	}
}

func TestCodexEditorBindings(t *testing.T) {
	m := Model{input: []rune("hello world"), cursor: 11}
	m.moveWordBackward()
	if m.cursor != 6 {
		t.Fatalf("word-left cursor = %d", m.cursor)
	}
	m.killToEnd()
	if string(m.input) != "hello " || m.killBuffer != "world" {
		t.Fatalf("kill produced %q, buffer %q", string(m.input), m.killBuffer)
	}
	m.insertRunes([]rune(m.killBuffer))
	if string(m.input) != "hello world" {
		t.Fatalf("yank produced %q", string(m.input))
	}
}

func TestLineEditingBindingsStayOnCurrentLine(t *testing.T) {
	m := Model{input: []rune("first\nsecond line\nthird"), cursor: len([]rune("first\nsecond"))}
	m.moveLineStart()
	if m.cursor != len([]rune("first\n")) {
		t.Fatalf("line start cursor = %d", m.cursor)
	}
	m.moveLineEnd()
	if m.cursor != len([]rune("first\nsecond line")) {
		t.Fatalf("line end cursor = %d", m.cursor)
	}
	m.killToStart()
	if string(m.input) != "first\n\nthird" || m.killBuffer != "second line" {
		t.Fatalf("line kill produced %q, buffer %q", string(m.input), m.killBuffer)
	}
}

func TestVerticalCursorMovementInWrappedComposer(t *testing.T) {
	m := Model{input: []rune("abcdefghij"), cursor: 9}
	if !m.moveCursorVertical(-1, 5) || m.cursor != 4 {
		t.Fatalf("cursor did not move vertically: %d", m.cursor)
	}
}

func TestShiftEnterInsertsNewlineWithoutSubmitting(t *testing.T) {
	m := Model{mode: sessionMode, input: []rune("first"), cursor: 5}
	updated, cmd := m.handleKey(tea.KeyPressMsg{Code: tea.KeyEnter, Mod: tea.ModShift})
	got := updated.(Model)
	if cmd != nil || string(got.input) != "first\n" || len(got.messages) != 0 {
		t.Fatalf("shift+enter submitted instead of inserting newline: input=%q messages=%d cmd=%v", string(got.input), len(got.messages), cmd != nil)
	}
}

func TestTurnStartedTracksActiveTurnForInterrupt(t *testing.T) {
	m := Model{activeTurns: make(map[string]string)}
	m.applyEvent(appserver.Event{Method: "turn/started", Params: json.RawMessage(`{"threadId":"thr_1","turn":{"id":"turn_9","status":"inProgress"}}`)})
	if got := m.activeTurns["thr_1"]; got != "turn_9" {
		t.Fatalf("active turn = %q", got)
	}
	m.applyEvent(appserver.Event{Method: "turn/completed", Params: json.RawMessage(`{"threadId":"thr_1","turn":{"id":"turn_9","status":"interrupted","items":[]}}`)})
	if _, ok := m.activeTurns["thr_1"]; ok {
		t.Fatal("completed turn remained active")
	}
}

func TestActivitySeparatorAndWorkingStatus(t *testing.T) {
	m := Model{
		width: 72, sessionID: "thr", activeTurns: map[string]string{"thr": "turn"},
		turnStarted: map[string]time.Time{"thr": time.Now().Add(-3 * time.Second)},
		messages: []chatMessage{
			{Role: "user", Text: "do it"},
			{ID: "cmd", Role: "activity", Kind: "command", Text: "go test ./...", Status: "completed", Detail: "ok", TurnID: "turn"},
			{ID: "final", Role: "assistant", Kind: "message", Text: "Done.", Phase: "final_answer", TurnID: "turn", TurnDurationMS: 317000},
		},
	}
	lines, _ := m.renderTranscript(appserver.Thread{Cwd: "/tmp"})
	got := ansi.Strip(strings.Join(lines, "\n"))
	if !strings.Contains(got, "Ran") || !strings.Contains(got, "go test ./...") || !strings.Contains(got, "Worked for 5m 17s") {
		t.Fatalf("activity transcript is incomplete: %q", got)
	}
	if strings.Index(got, "Worked for 5m 17s") > strings.Index(got, "Ran") {
		t.Fatalf("worked separator followed activity instead of starting its block: %q", got)
	}
	if status := m.workingStatus(); !strings.Contains(status, "Working") || !strings.Contains(status, "esc to interrupt") {
		t.Fatalf("working status is incomplete: %q", status)
	}
}

func TestApprovalReviewRendersAssessmentAndGrant(t *testing.T) {
	m := Model{sessionID: "thr", messages: nil}
	m.applyEvent(appserver.Event{Method: "item/autoApprovalReview/completed", Params: json.RawMessage(`{
		"threadId":"thr","turnId":"turn","reviewId":"review-1",
		"review":{"status":"approved","riskLevel":"medium","userAuthorization":"high","rationale":"This local fix restores lifecycle detection."},
		"action":{"type":"command","command":"go test ./..."}
	}`)})
	if len(m.messages) != 1 {
		t.Fatalf("review event was not retained: %#v", m.messages)
	}
	got := strings.Join(strings.Fields(ansi.Strip(strings.Join(renderActivity(m.messages[0], 100, "/tmp", false), "\n"))), " ")
	for _, want := range []string{
		"⚠ Automatic approval review approved (risk: medium, authorization: high): This local fix restores lifecycle detection.",
		"✓ Auto-reviewer approved codex to run go test ./... this time",
	} {
		if !strings.Contains(got, want) {
			t.Fatalf("approval lifecycle is missing %q: %q", want, got)
		}
	}
}

func TestRolloutReviewIsInsertedBeforeMatchingCommand(t *testing.T) {
	messages := []chatMessage{{ID: "cmd", Role: "activity", Kind: "command", TurnID: "turn", Text: "go test ./..."}}
	got := mergeRolloutReviews(messages, []rolloutReview{{ID: "review-call", TurnID: "turn"}})
	if len(got) != 2 || got[0].Kind != "review" || got[0].Text != messages[0].Text || got[1].Kind != "command" {
		t.Fatalf("rollout approval was not paired with its command: %#v", got)
	}
}

func TestCommandHighlightingPreservesTextAndStripsShellWrapper(t *testing.T) {
	command := `/bin/zsh -lc "rg -n 'hello/world' ./... && go test ./..."`
	rendered := ansi.Strip(highlightShell(command, "/tmp"))
	if rendered != "rg -n 'hello/world' ./... && go test ./..." {
		t.Fatalf("command text was corrupted: %q", rendered)
	}
	if strings.Contains(rendered, ".///") || strings.Contains(rendered, "////") {
		t.Fatalf("slash corruption returned: %q", rendered)
	}
}

func TestQuotedChainedCommandUsesBashSyntaxRoles(t *testing.T) {
	command := `"gofmt -w internal/appserver/client.go && go test ./... -run '^$' --bench BenchmarkVirtualizedTranscriptScroll && ./codex-agents --version"`
	rendered := highlightShell(command, "/tmp")
	plain := ansi.Strip(rendered)
	if plain != strings.Trim(command, `"`) {
		t.Fatalf("transport quotes were not removed cleanly: %q", plain)
	}
	for name, sequence := range map[string]string{
		"command":  shellCommand + "gofmt",
		"flag":     shellFlag + "-w",
		"operator": shellOperator + "&&",
		"string":   shellString + `'^$'`,
	} {
		if !strings.Contains(rendered, sequence) {
			t.Fatalf("%s syntax role is missing from %q", name, rendered)
		}
	}
	if strings.HasPrefix(rendered, shellString) {
		t.Fatal("the entire script was incorrectly highlighted as one string")
	}
}

func TestPlainHighlightingNeverRewritesSlashesRecursively(t *testing.T) {
	text := "run /bin/zsh -lc ./... and https://example.com/a/b"
	if got := ansi.Strip(highlightText(text, "/tmp")); got != text {
		t.Fatalf("highlighting changed copyable text: got %q want %q", got, text)
	}
}

func TestScrollStopsAtTop(t *testing.T) {
	m := Model{
		mode: sessionMode, width: 40, height: 10,
		messages: []chatMessage{{Role: "assistant", Text: strings.Repeat("line ", 100)}},
	}
	m.scrollConversation(1 << 30)
	top := m.scrollOffset
	m.scrollConversation(100)
	if m.scrollOffset != top {
		t.Fatalf("scroll moved beyond top: before=%d after=%d", top, m.scrollOffset)
	}
}

func TestConversationScrollAndStickyPrompt(t *testing.T) {
	m := Model{
		mode: sessionMode, sessionID: "thr", width: 48, height: 12,
		threads: []appserver.Thread{{ID: "thr", Cwd: "/tmp"}},
		messages: []chatMessage{
			{Role: "user", Text: "first question"},
			{Role: "assistant", Text: strings.Repeat("first answer ", 20)},
			{Role: "user", Text: "second question"},
			{Role: "assistant", Text: strings.Repeat("second answer ", 20)},
		},
	}
	m.scrollConversation(5)
	if m.scrollOffset == 0 {
		t.Fatal("conversation did not scroll up")
	}
	view := m.sessionView()
	if strings.Contains(view, "› You") || strings.Contains(view, "• Codex") {
		t.Fatal("turn labels should not be rendered")
	}
	background, _ := composerBackgrounds()
	if !strings.Contains(view, background+"  second question") && !strings.Contains(view, background+"  first question") {
		t.Fatal("sticky user prompt is missing")
	}
	if got := strings.Count(view, "\n"); got != m.height-1 {
		t.Fatalf("session view has %d line breaks, want %d", got, m.height-1)
	}
}

func TestStickyPromptTracksScrolledTurn(t *testing.T) {
	anchors := []transcriptAnchor{{line: 0, text: "first"}, {line: 12, text: "second"}}
	first := stickyPrompt(anchors, 10, 40)
	second := stickyPrompt(anchors, 14, 40)
	if !strings.Contains(first, "first") || strings.Contains(first, "second") {
		t.Fatalf("wrong first sticky prompt: %q", first)
	}
	if !strings.Contains(second, "second") {
		t.Fatalf("sticky prompt did not advance: %q", second)
	}
}

func TestSessionComposerAndStickyPromptHaveRulesAboveAndBelow(t *testing.T) {
	m := Model{mode: sessionMode, width: 40, input: []rune("hello"), cursor: 5}
	composer, _ := m.composer("message Codex…")
	rule := sessionRule(40)
	if composer[0] != rule || composer[len(composer)-1] != rule {
		t.Fatalf("composer frame is incomplete: %#v", composer)
	}
	sticky := strings.Split(stickyPrompt([]transcriptAnchor{{line: 0, text: "last prompt"}}, 0, 40), "\n")
	if len(sticky) != 3 || sticky[0] != rule || sticky[2] != rule || !strings.Contains(sticky[1], "last prompt") {
		t.Fatalf("sticky prompt frame is incomplete: %#v", sticky)
	}
}

func TestStickyPromptIsAlwaysPinned(t *testing.T) {
	anchors := []transcriptAnchor{{line: 0, text: "visible prompt"}}
	if got := stickyPrompt(anchors, 0, 40); !strings.Contains(got, "visible prompt") {
		t.Fatalf("visible prompt was not pinned: %q", got)
	}
	if !promptStartsViewport(anchors, 0) {
		t.Fatal("viewport did not suppress the duplicated prompt row")
	}
}

func TestExternalActiveThreadShowsWorkingStatus(t *testing.T) {
	m := Model{
		sessionID: "external", activeTurns: make(map[string]string),
		turnStarted: make(map[string]time.Time),
		threads:     []appserver.Thread{{ID: "external", Status: appserver.Status{Type: "idle"}}},
	}
	m.mergeExternalStatus("external", appserver.Status{Type: "active"})
	if status := m.workingStatus(); !strings.Contains(status, "Working") {
		t.Fatalf("external active thread has no working indicator: %q", status)
	}
	m.mergeExternalStatus("external", appserver.Status{Type: "idle"})
	if status := m.workingStatus(); status != "" {
		t.Fatalf("completed external thread still looks active: %q", status)
	}
}

func TestExternalHistoryCannotOverwriteLiveRolloutStatus(t *testing.T) {
	m := Model{
		mode: sessionMode, sessionID: "external", width: 80, height: 24,
		activeTurns: make(map[string]string), ownedThreads: make(map[string]bool),
		turnStarted: map[string]time.Time{"external": time.Now()},
		threads:     []appserver.Thread{{ID: "external", Cwd: "/tmp", Status: appserver.Status{Type: "active"}}},
		transcript:  newTranscriptLayout(),
	}
	updated, _ := m.Update(externalHistoryMsg{thread: appserver.Thread{
		ID: "external", Cwd: "/tmp", Status: appserver.Status{Type: "idle"},
	}})
	got := updated.(Model)
	thread, _ := got.threadByID("external")
	if thread.Status.Type != "active" || got.workingStatus() == "" {
		t.Fatalf("stale history status hid external activity: %#v", thread.Status)
	}
}

func TestViewClipsRowsToTerminalWidth(t *testing.T) {
	got := clipViewWidth("short\n"+strings.Repeat("x", 30), 10)
	for _, row := range strings.Split(got, "\n") {
		if ansi.StringWidth(row) > 10 {
			t.Fatalf("row escaped terminal width: %q", row)
		}
	}
}

func TestTranscriptLayoutRebuildsOnlyDirtyTail(t *testing.T) {
	m := Model{
		sessionID: "thread", width: 80, transcript: newTranscriptLayout(),
		messages: []chatMessage{
			{Role: "user", Text: "question"},
			{ID: "answer", Role: "assistant", Text: "first"},
		},
	}
	thread := appserver.Thread{ID: "thread", Cwd: "/tmp"}
	first := m.ensureTranscriptLayout(thread)
	prefix := &first.entries[0].lines[0]
	m.messages[1].Text += " second"
	m.invalidateTranscript(1)
	second := m.ensureTranscriptLayout(thread)
	if prefix != &second.entries[0].lines[0] {
		t.Fatal("unchanged transcript prefix was rendered again")
	}
	if got := strings.Join(second.rows(0, second.totalRows()), "\n"); !strings.Contains(got, "first second") {
		t.Fatalf("dirty tail was not refreshed: %q", got)
	}
}

func BenchmarkVirtualizedTranscriptScroll(b *testing.B) {
	messages := make([]chatMessage, 0, 20_000)
	for i := 0; i < 10_000; i++ {
		messages = append(messages,
			chatMessage{Role: "user", Text: fmt.Sprintf("question %d", i)},
			chatMessage{Role: "assistant", Text: "A compact answer that occupies one transcript row."},
		)
	}
	m := Model{
		mode: sessionMode, sessionID: "large", width: 120, height: 40,
		threads:  []appserver.Thread{{ID: "large", Cwd: "/tmp"}},
		messages: messages, transcript: newTranscriptLayout(),
	}
	layout := m.ensureTranscriptLayout(m.threads[0])
	maxOffset := max(1, layout.totalRows()-30)
	b.ResetTimer()
	for i := 0; i < b.N; i++ {
		m.scrollOffset = i % maxOffset
		_ = m.sessionView()
	}
}

func TestAppServerUserEchoAcknowledgesOptimisticPrompt(t *testing.T) {
	m := Model{messages: []chatMessage{{Role: "user", Text: "hello"}}}
	m.mergeMessage(chatMessage{ID: "user-1", Role: "user", Kind: "message", Text: "hello"})
	if len(m.messages) != 1 || m.messages[0].ID != "user-1" {
		t.Fatalf("user echo was duplicated: %#v", m.messages)
	}
}
