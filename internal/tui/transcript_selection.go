package tui

import (
	"os/exec"
	"runtime"
	"strings"

	tea "charm.land/bubbletea/v2"
	"github.com/charmbracelet/x/ansi"
)

// copyText writes through both OSC 52 and the host clipboard. OSC 52 keeps
// remote/modern terminals working, while the native writer covers terminals
// (including some macOS configurations) that ignore OSC 52. Neither path
// changes mouse capture, alternate-screen scrolling, or transcript rendering.
func copyText(text string) tea.Cmd {
	return tea.Batch(tea.SetClipboard(text), nativeClipboardWrite(text))
}

func nativeClipboardWrite(text string) tea.Cmd {
	return func() tea.Msg {
		var command *exec.Cmd
		switch runtime.GOOS {
		case "darwin":
			command = exec.Command("pbcopy")
		case "windows":
			command = exec.Command("clip.exe")
		default:
			for _, candidate := range [][]string{
				{"wl-copy"},
				{"xclip", "-selection", "clipboard"},
				{"xsel", "--clipboard", "--input"},
			} {
				if _, err := exec.LookPath(candidate[0]); err == nil {
					command = exec.Command(candidate[0], candidate[1:]...)
					break
				}
			}
		}
		if command == nil {
			return nil
		}
		command.Stdin = strings.NewReader(text)
		_ = command.Run() // OSC 52 remains the fallback if this writer fails.
		return nil
	}
}

const transcriptBodyTop = 4 // header plus the three-row pinned prompt

func (m *Model) transcriptViewport(layout *transcriptLayout) (start, end, stickyStart, maxOffset int) {
	_, composerRows := m.composer("message Codex…")
	_, popupRows := m.commandPopup()
	if m.workingStatus() != "" {
		popupRows++
	}
	if m.ownershipNotice() != "" {
		popupRows++
	}
	reservedRows := composerRows + popupRows + 1 // footer
	visible := max(1, m.height-reservedRows-transcriptBodyTop)
	maxOffset = max(0, layout.totalRows()-visible)
	offset := min(m.scrollOffset, maxOffset)
	start = max(0, layout.totalRows()-visible-offset)
	stickyStart = start
	if promptStartsViewport(layout.anchors, start) {
		start++
	}
	end = min(layout.totalRows(), start+visible)
	return start, end, stickyStart, maxOffset
}

func (m *Model) transcriptPointAt(x, y int, clampOutside bool) (transcriptPoint, bool) {
	if m.mode != sessionMode || m.width <= 0 || m.height <= 0 {
		return transcriptPoint{}, false
	}
	thread, _ := m.threadByID(m.sessionID)
	layout := m.ensureTranscriptLayout(thread)
	start, end, _, _ := m.transcriptViewport(layout)
	if start >= end {
		return transcriptPoint{}, false
	}
	firstY, lastY := transcriptBodyTop, transcriptBodyTop+(end-start)-1
	if !clampOutside && (y < firstY || y > lastY || x < 0 || x >= m.width) {
		return transcriptPoint{}, false
	}
	y = max(firstY, min(lastY, y))
	row := start + y - firstY
	line := layout.rows(row, row+1)[0]
	col := max(0, min(ansi.StringWidth(line), x))
	return transcriptPoint{row: row, col: col}, true
}

func (m *Model) clearTranscriptSelection() {
	m.transcriptSelecting = false
	m.transcriptSelected = false
	m.transcriptAnchor = transcriptPoint{}
	m.transcriptHead = transcriptPoint{}
}

func orderedTranscriptSelection(anchor, head transcriptPoint) (transcriptPoint, transcriptPoint) {
	if anchor.row < head.row || anchor.row == head.row && anchor.col <= head.col {
		return anchor, head
	}
	return head, anchor
}

func (m *Model) selectedTranscriptText() string {
	if !m.transcriptSelected {
		return ""
	}
	thread, _ := m.threadByID(m.sessionID)
	layout := m.ensureTranscriptLayout(thread)
	start, end := orderedTranscriptSelection(m.transcriptAnchor, m.transcriptHead)
	rows := layout.rows(start.row, end.row+1)
	selected := make([]string, 0, len(rows))
	for index, row := range rows {
		plain := ansi.Strip(row)
		left, right := 0, ansi.StringWidth(plain)
		if index == 0 {
			left = min(start.col, right)
		}
		if index == len(rows)-1 {
			right = min(end.col, right)
		}
		if right < left {
			right = left
		}
		selected = append(selected, ansi.Cut(plain, left, right))
	}
	return strings.Join(selected, "\n")
}

func (m *Model) highlightTranscriptSelection(lines []string, firstRow int) []string {
	if !m.transcriptSelected {
		return lines
	}
	start, end := orderedTranscriptSelection(m.transcriptAnchor, m.transcriptHead)
	highlighted := append([]string(nil), lines...)
	_, selectionBG := composerBackgrounds()
	for index, line := range highlighted {
		row := firstRow + index
		if row < start.row || row > end.row {
			continue
		}
		width := ansi.StringWidth(line)
		left, right := 0, width
		if row == start.row {
			left = min(start.col, width)
		}
		if row == end.row {
			right = min(end.col, width)
		}
		if right <= left {
			continue
		}
		prefix := ansi.Cut(line, 0, left)
		middle := ansi.Cut(line, left, right)
		suffix := ansi.Cut(line, right, width)
		// A selection is a background overlay, not a replacement text style.
		// Reapply it after SGR resets inside the span so command foregrounds,
		// flags, strings, operators, and links remain exactly as rendered.
		middle = strings.ReplaceAll(middle, reset, reset+selectionBG)
		highlighted[index] = prefix + selectionBG + middle + reset + suffix
	}
	return highlighted
}
