package tui

import (
	"fmt"
	"sort"
	"strings"
	"time"

	"github.com/charmbracelet/x/ansi"
	"github.com/majd/codex-agents/internal/appserver"
)

type transcriptAnchor struct {
	line int
	text string
}

type transcriptEntry struct {
	start       int
	lines       []string
	anchor      string
	workedAfter bool
}

// transcriptLayout is a virtualized row index. Messages retain their rendered
// rows independently, so scrolling only slices the handful of rows on screen
// and a streaming delta only rebuilds the changed tail entry.
type transcriptLayout struct {
	sessionID string
	cwd       string
	width     int
	expanded  bool
	dirtyFrom int
	entries   []transcriptEntry
	anchors   []transcriptAnchor
}

func newTranscriptLayout() *transcriptLayout {
	return &transcriptLayout{dirtyFrom: 0}
}

func (m *Model) invalidateTranscript(from int) {
	if m.transcript == nil {
		m.transcript = newTranscriptLayout()
	}
	if m.transcript.dirtyFrom < 0 || from < m.transcript.dirtyFrom {
		m.transcript.dirtyFrom = max(0, from)
	}
}

func (m *Model) ensureTranscriptLayout(thread appserver.Thread) *transcriptLayout {
	if m.transcript == nil {
		m.transcript = newTranscriptLayout()
	}
	layout := m.transcript
	width := max(20, m.width-4)
	if layout.sessionID != m.sessionID || layout.cwd != thread.Cwd || layout.width != width || layout.expanded != m.expandedTools {
		layout.sessionID, layout.cwd, layout.width, layout.expanded = m.sessionID, thread.Cwd, width, m.expandedTools
		layout.entries, layout.anchors, layout.dirtyFrom = nil, nil, 0
	}
	if layout.dirtyFrom < 0 && len(layout.entries) == len(m.messages) {
		return layout
	}
	from := layout.dirtyFrom
	if from < 0 {
		from = min(len(layout.entries), len(m.messages))
	}
	from = min(from, min(len(layout.entries), len(m.messages)))
	start, worked := 0, false
	if from > 0 {
		previous := layout.entries[from-1]
		start = previous.start + len(previous.lines)
		worked = previous.workedAfter
	}
	layout.entries = layout.entries[:from]
	anchorCount := sort.Search(len(layout.anchors), func(i int) bool { return layout.anchors[i].line >= start })
	layout.anchors = layout.anchors[:anchorCount]
	for i := from; i < len(m.messages); i++ {
		lines, anchor, workedAfter := renderTranscriptEntry(m.messages[i], width, m.width, thread.Cwd, m.expandedTools, i > 0, worked)
		entry := transcriptEntry{start: start, lines: lines, anchor: anchor, workedAfter: workedAfter}
		layout.entries = append(layout.entries, entry)
		if anchor != "" {
			anchorLine := start
			if i > 0 {
				anchorLine += 2 // inter-turn breathing room precedes the prompt row
			}
			layout.anchors = append(layout.anchors, transcriptAnchor{line: anchorLine, text: anchor})
		}
		start += len(lines)
		worked = workedAfter
	}
	layout.dirtyFrom = -1
	return layout
}

func (l *transcriptLayout) totalRows() int {
	if len(l.entries) == 0 {
		return 0
	}
	last := l.entries[len(l.entries)-1]
	return last.start + len(last.lines)
}

func (l *transcriptLayout) rows(start, end int) []string {
	start, end = max(0, start), min(l.totalRows(), end)
	if start >= end {
		return nil
	}
	first := sort.Search(len(l.entries), func(i int) bool {
		return l.entries[i].start+len(l.entries[i].lines) > start
	})
	rows := make([]string, 0, end-start)
	for i := first; i < len(l.entries) && l.entries[i].start < end; i++ {
		entry := l.entries[i]
		lo := max(0, start-entry.start)
		hi := min(len(entry.lines), end-entry.start)
		rows = append(rows, entry.lines[lo:hi]...)
	}
	return rows
}

func (m Model) renderTranscript(thread appserver.Thread) ([]string, []transcriptAnchor) {
	var lines []string
	var anchors []transcriptAnchor
	width := max(20, m.width-4)
	workedSinceUser := false
	for _, message := range m.messages {
		if message.Role == "user" {
			wrapped := wrap(message.Text, width)
			if len(wrapped) == 0 {
				wrapped = []string{""}
			}
			if len(lines) > 0 {
				lines = append(lines, "", "")
			}
			anchors = append(anchors, transcriptAnchor{line: len(lines), text: wrapped[0]})
			for _, line := range wrapped {
				lines = append(lines, userMessageLine("  "+line, m.width))
			}
			workedSinceUser = false
			continue
		}
		if message.Role == "activity" {
			if len(lines) > 0 {
				lines = append(lines, "")
			}
			lines = append(lines, renderActivity(message, width, thread.Cwd, m.expandedTools)...)
			workedSinceUser = true
			continue
		}
		if workedSinceUser && isFinalPhase(message.Phase) {
			if len(lines) > 0 {
				lines = append(lines, "")
			}
			lines = append(lines, turnSeparator(message.TurnDurationMS, m.width), "")
			workedSinceUser = false
		} else if len(lines) > 0 {
			lines = append(lines, "")
		}
		rendered := renderMarkdown(message.Text, width, thread.Cwd)
		for i, line := range rendered {
			prefix := "  "
			if i == 0 {
				prefix = dim + "•" + reset + " "
			}
			lines = append(lines, prefix+line)
		}
	}
	return lines, anchors
}

func renderTranscriptEntry(message chatMessage, width, screenWidth int, cwd string, expanded, hasPrevious, workedBefore bool) ([]string, string, bool) {
	var lines []string
	if message.Role == "user" {
		wrapped := wrap(message.Text, width)
		if len(wrapped) == 0 {
			wrapped = []string{""}
		}
		if hasPrevious {
			lines = append(lines, "", "")
		}
		anchor := wrapped[0]
		for _, line := range wrapped {
			lines = append(lines, userMessageLine("  "+line, screenWidth))
		}
		return lines, anchor, false
	}
	if message.Role == "activity" {
		if hasPrevious {
			lines = append(lines, "")
		}
		lines = append(lines, renderActivity(message, width, cwd, expanded)...)
		return lines, "", true
	}
	final := workedBefore && isFinalPhase(message.Phase)
	if final {
		if hasPrevious {
			lines = append(lines, "")
		}
		lines = append(lines, turnSeparator(message.TurnDurationMS, screenWidth), "")
	} else if hasPrevious {
		lines = append(lines, "")
	}
	for i, line := range renderMarkdown(message.Text, width, cwd) {
		prefix := "  "
		if i == 0 {
			prefix = dim + "•" + reset + " "
		}
		lines = append(lines, prefix+line)
	}
	return lines, "", workedBefore && !final
}

func isFinalPhase(phase string) bool {
	return phase == "final_answer" || phase == "finalAnswer"
}

func renderActivity(message chatMessage, width int, cwd string, expanded bool) []string {
	if message.Kind == "review" {
		return renderReview(message, width, cwd)
	}
	if message.Kind == "file" && len(message.Changes) > 0 {
		return renderFileChanges(message.Changes, width, cwd)
	}
	if message.Kind == "command" && explorable(message.Actions) {
		return renderExplored(message.Actions, width, cwd)
	}
	verb := "Used"
	switch message.Kind {
	case "command":
		if activityDone(message.Status) {
			verb = "Ran"
		} else {
			verb = "Running"
		}
	case "mcp":
		if activityDone(message.Status) {
			verb = "Called"
		} else {
			verb = "Calling"
		}
	case "file":
		if activityDone(message.Status) {
			verb = "Edited"
		} else {
			verb = "Editing"
		}
	case "web":
		if activityDone(message.Status) {
			verb = "Searched the web"
		} else {
			verb = "Searching the web"
		}
	case "review":
		switch message.Status {
		case "approved":
			verb = "Auto-review approved"
		case "denied":
			verb = "Auto-review denied"
		case "timedOut", "aborted":
			verb = "Auto-review stopped"
		default:
			verb = "Auto-reviewing"
		}
	}
	style := cyan
	if activityDone(message.Status) {
		style = green
	}
	content := highlightText(message.Text, cwd)
	if message.Kind == "web" {
		if !activityDone(message.Status) || strings.TrimSpace(message.Text) == "" {
			content = ""
		} else {
			content = "for " + content
		}
	}
	if message.Kind == "command" {
		content = highlightShell(message.Text, cwd)
	}
	header := style + "•" + reset + " " + bold + verb + reset + " " + content
	lines := strings.Split(ansi.Hardwrap(ansi.Wordwrap(header, width, ""), width, false), "\n")
	detail := strings.TrimSpace(message.Detail)
	if detail == "" {
		return lines
	}
	output := strings.Split(detail, "\n")
	if len(output) > 6 && !expanded {
		omitted := len(output) - 4
		output = append(append(append([]string{}, output[:2]...), fmt.Sprintf("… +%d lines (ctrl + t to view transcript)", omitted)), output[len(output)-2:]...)
	}
	for i, line := range output {
		branch := "│ "
		if i == len(output)-1 {
			branch = "└ "
		}
		wrappedOutput := wrap(line, max(8, width-4))
		if len(wrappedOutput) == 0 {
			wrappedOutput = []string{""}
		}
		for _, wrapped := range wrappedOutput {
			lines = append(lines, dim+"  "+branch+wrapped+reset)
			branch = "  "
		}
	}
	return lines
}

func renderReview(message chatMessage, width int, cwd string) []string {
	if message.Status == "approved" {
		var lines []string
		if message.Detail != "" || message.RiskLevel != "" || message.Authorization != "" {
			meta := ""
			if message.RiskLevel != "" {
				meta = "risk: " + message.RiskLevel
			}
			if message.Authorization != "" {
				if meta != "" {
					meta += ", "
				}
				meta += "authorization: " + message.Authorization
			}
			if meta != "" {
				meta = " (" + meta + ")"
			}
			warning := yellow + "⚠ Automatic approval review " + bold + "approved" + reset + yellow + meta
			if message.Detail != "" {
				warning += ": " + message.Detail
			}
			warning += reset
			lines = append(lines, strings.Split(ansi.Hardwrap(ansi.Wordwrap(warning, width, ""), width, false), "\n")...)
			lines = append(lines, "")
		}
		row := green + "✓" + reset + " Auto-reviewer " + bold + "approved" + reset + " codex to run " +
			dim + highlightShell(message.Text, cwd) + reset + " " + bold + "this time" + reset
		return append(lines, strings.Split(ansi.Hardwrap(ansi.Wordwrap(row, width, ""), width, false), "\n")...)
	}
	verb := "reviewing"
	if message.Status == "denied" {
		verb = "denied"
	}
	row := cyan + "•" + reset + " Auto-reviewer " + bold + verb + reset + " " + highlightShell(message.Text, cwd)
	return strings.Split(ansi.Hardwrap(ansi.Wordwrap(row, width, ""), width, false), "\n")
}

func activityDone(status string) bool {
	return status == "completed" || status == "failed" || status == "declined" || status == "approved" || status == "denied" || status == "timedOut" || status == "aborted"
}

func turnSeparator(durationMS int64, width int) string {
	label := "─"
	if durationMS >= 1000 {
		label = "─ Worked for " + formatWorkedDuration(durationMS) + " ─"
	}
	return dim + label + strings.Repeat("─", max(0, width-ansi.StringWidth(label))) + reset
}

func formatWorkedDuration(durationMS int64) string {
	seconds := durationMS / 1000
	if seconds < 60 {
		return fmt.Sprintf("%ds", seconds)
	}
	return fmt.Sprintf("%dm %02ds", seconds/60, seconds%60)
}

func (m Model) workingStatus() string {
	thread, _ := m.threadByID(m.sessionID)
	if m.activeTurns[m.sessionID] == "" && thread.Status.Type != "active" {
		return ""
	}
	started := m.turnStarted[m.sessionID]
	if started.IsZero() && thread.UpdatedAt > 0 {
		started = time.Unix(thread.UpdatedAt, 0)
	}
	elapsed := time.Duration(0)
	if !started.IsZero() {
		elapsed = time.Since(started).Truncate(time.Second)
	}
	return cyan + "•" + reset + " " + bold + "Working" + reset + dim + fmt.Sprintf(" (%s • esc to interrupt)", compactElapsed(elapsed)) + reset
}

func compactElapsed(elapsed time.Duration) string {
	seconds := int(elapsed.Seconds())
	if seconds < 60 {
		return fmt.Sprintf("%ds", seconds)
	}
	return fmt.Sprintf("%dm %02ds", seconds/60, seconds%60)
}

func (m *Model) transcriptGeometry() (layout *transcriptLayout, bodyRows int) {
	thread, _ := m.threadByID(m.sessionID)
	layout = m.ensureTranscriptLayout(thread)
	_, composerRows := m.composer("message Codex…")
	_, popupRows := m.commandPopup()
	if m.workingStatus() != "" {
		popupRows++
	}
	// Header, sticky prompt, composer, popup, and footer stay fixed.
	bodyRows = max(1, m.height-(composerRows+popupRows+1)-2)
	return layout, bodyRows
}

func (m *Model) scrollConversation(delta int) {
	if m.mode != sessionMode {
		return
	}
	layout, bodyRows := m.transcriptGeometry()
	maxOffset := max(0, layout.totalRows()-bodyRows)
	m.scrollOffset = max(0, min(maxOffset, m.scrollOffset+delta))
}

func stickyPrompt(anchors []transcriptAnchor, viewportStart int, width int) string {
	if len(anchors) == 0 {
		return dim + "  no user message" + reset
	}
	index := sort.Search(len(anchors), func(i int) bool { return anchors[i].line > viewportStart }) - 1
	if index < 0 {
		index = 0
	}
	selected := anchors[index]
	text := truncate(clean(selected.text), max(8, width-6))
	return userMessageLine("  "+text, width)
}

// promptStartsViewport reports whether the sticky prompt would otherwise be
// duplicated as the first transcript row. The sticky row occupies that row;
// the viewport can therefore begin with the following transcript line.
func promptStartsViewport(anchors []transcriptAnchor, viewportStart int) bool {
	index := sort.Search(len(anchors), func(i int) bool { return anchors[i].line >= viewportStart })
	return index < len(anchors) && anchors[index].line == viewportStart
}

func clipViewWidth(content string, width int) string {
	if width <= 0 {
		return content
	}
	rows := strings.Split(content, "\n")
	for i, row := range rows {
		if ansi.StringWidth(row) > width {
			rows[i] = ansi.Truncate(row, width, "")
		}
	}
	return strings.Join(rows, "\n")
}

func scrollStatus(offset, maxOffset int) string {
	if maxOffset == 0 {
		return ""
	}
	if offset == 0 {
		return dim + "bottom" + reset
	}
	return fmt.Sprintf("%s%d lines above bottom%s", dim, offset, reset)
}

func joinFooter(parts ...string) string {
	var nonempty []string
	for _, part := range parts {
		if strings.TrimSpace(part) != "" {
			nonempty = append(nonempty, part)
		}
	}
	return strings.Join(nonempty, "   ")
}
