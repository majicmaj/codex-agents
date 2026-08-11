package tui

import (
	"fmt"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"unicode"
)

const (
	underline = "\x1b[4m"
	noLine    = "\x1b[24m"
	yellow    = "\x1b[33m"
)

type slashCommand struct {
	Name        string
	Description string
	Scope       string
}

// Presentation order follows Codex's frequently-used-first command palette.
// Only commands that this App Server overview can implement honestly are
// executable here; native commands remain discoverable and clearly labelled.
var slashCommands = []slashCommand{
	{"new", "start a new session", "agents"},
	{"resume", "return to the session overview", "agents"},
	{"rename", "rename the current session", "agents"},
	{"status", "show this session's live state", "agents"},
	{"stop", "interrupt the active turn", "agents"},
	{"clear", "clear this conversation view", "agents"},
	{"help", "show keyboard shortcuts", "agents"},
	{"quit", "exit Codex agents", "agents"},
	{"exit", "exit Codex agents", "agents"},
	{"model", "choose model and reasoning effort", "codex"},
	{"permissions", "choose what Codex may do", "codex"},
	{"review", "review current changes", "codex"},
	{"init", "create AGENTS.md instructions", "codex"},
	{"compact", "summarize conversation context", "codex"},
	{"diff", "show the current git diff", "codex"},
	{"mention", "mention a file", "codex"},
	{"usage", "view account usage", "codex"},
	{"mcp", "list configured MCP tools", "codex"},
	{"apps", "manage apps", "codex"},
	{"skills", "browse available skills", "codex"},
	{"theme", "choose syntax highlighting theme", "codex"},
	{"keymap", "remap Codex TUI shortcuts", "codex"},
	{"vim", "toggle Vim composer mode", "codex"},
}

func (m Model) matchingCommands() []slashCommand {
	if len(m.input) == 0 || m.input[0] != '/' || strings.ContainsRune(string(m.input), '\n') {
		return nil
	}
	first := strings.Fields(string(m.input))
	query := strings.TrimPrefix(first[0], "/")
	if len(first) > 1 || strings.Contains(string(m.input), " ") {
		return nil
	}
	var matches []slashCommand
	for _, command := range slashCommands {
		if strings.Contains(command.Name, query) {
			matches = append(matches, command)
		}
	}
	return matches
}

func (m Model) commandPopup() ([]string, int) {
	matches := m.matchingCommands()
	if len(matches) == 0 {
		return nil, 0
	}
	limit := min(7, len(matches))
	selected := min(m.popupSelected, len(matches)-1)
	start := max(0, min(len(matches)-limit, selected-limit/2))
	width := max(20, m.width-4)
	lines := make([]string, 0, limit+1)
	lines = append(lines, dim+"  commands"+reset)
	for i := start; i < start+limit; i++ {
		command := matches[i]
		marker := "  "
		style := ""
		if i == selected {
			marker, style = "› ", cyan
		}
		scope := ""
		if command.Scope == "codex" {
			scope = dim + "  native Codex" + reset
		}
		name := "/" + command.Name
		descriptionWidth := max(8, width-len(name)-19)
		lines = append(lines, fmt.Sprintf("  %s%s%-14s%s %-*s%s", style, marker, name, reset, descriptionWidth, truncate(command.Description, descriptionWidth), scope))
	}
	return lines, len(lines)
}

func (m *Model) completeSlashCommand() bool {
	matches := m.matchingCommands()
	if len(matches) == 0 {
		return false
	}
	selected := min(m.popupSelected, len(matches)-1)
	m.input = []rune("/" + matches[selected].Name + " ")
	m.cursor = len(m.input)
	m.hasSelection = false
	m.popupSelected = 0
	return true
}

func parseSlashCommand(text string) (string, string, bool) {
	line := strings.TrimSpace(text)
	if !strings.HasPrefix(line, "/") {
		return "", "", false
	}
	parts := strings.SplitN(strings.TrimPrefix(line, "/"), " ", 2)
	name := parts[0]
	args := ""
	if len(parts) == 2 {
		args = strings.TrimSpace(parts[1])
	}
	for _, command := range slashCommands {
		if command.Name == name {
			return name, args, true
		}
	}
	return name, args, false
}

func (m *Model) deleteForward() {
	if m.deleteSelection() || m.cursor >= len(m.input) {
		return
	}
	m.input = append(m.input[:m.cursor], m.input[m.cursor+1:]...)
}

func (m *Model) deleteWordForward() {
	if m.deleteSelection() || m.cursor >= len(m.input) {
		return
	}
	end := m.cursor
	for end < len(m.input) && isWordSpace(m.input[end]) {
		end++
	}
	for end < len(m.input) && !isWordSpace(m.input[end]) {
		end++
	}
	m.input = append(m.input[:m.cursor], m.input[end:]...)
}

func (m *Model) moveWordBackward() {
	position := m.cursor
	for position > 0 && unicode.IsSpace(m.input[position-1]) {
		position--
	}
	for position > 0 && !unicode.IsSpace(m.input[position-1]) {
		position--
	}
	m.moveCursorTo(position, false)
}

func (m *Model) moveWordForward() {
	position := m.cursor
	for position < len(m.input) && unicode.IsSpace(m.input[position]) {
		position++
	}
	for position < len(m.input) && !unicode.IsSpace(m.input[position]) {
		position++
	}
	m.moveCursorTo(position, false)
}

func (m *Model) lineStart() int {
	start := m.cursor
	for start > 0 && m.input[start-1] != '\n' {
		start--
	}
	return start
}

func (m *Model) lineEnd() int {
	end := m.cursor
	for end < len(m.input) && m.input[end] != '\n' {
		end++
	}
	return end
}

func (m *Model) moveLineStart() { m.moveCursorTo(m.lineStart(), false) }

func (m *Model) moveLineEnd() { m.moveCursorTo(m.lineEnd(), false) }

func (m *Model) moveCursorVertical(delta, visualWidth int) bool {
	segments := composerSegments(m.input, max(1, visualWidth))
	current := -1
	column := 0
	for i, segment := range segments {
		start, end := segment[0], segment[1]
		cursorHere := m.cursor >= start && m.cursor < end
		if m.cursor == end && (end == len(m.input) || (end < len(m.input) && m.input[end] == '\n')) {
			cursorHere = true
		}
		if cursorHere {
			current, column = i, m.cursor-start
			break
		}
	}
	target := current + delta
	if current < 0 || target < 0 || target >= len(segments) {
		return false
	}
	segment := segments[target]
	m.moveCursorTo(min(segment[1], segment[0]+column), false)
	return true
}

func (m *Model) killToStart() {
	if m.deleteSelection() {
		return
	}
	start := m.lineStart()
	m.killBuffer = string(m.input[start:m.cursor])
	m.input = append(m.input[:start], m.input[m.cursor:]...)
	m.cursor = start
}

func (m *Model) killToEnd() {
	if m.deleteSelection() {
		return
	}
	end := m.lineEnd()
	if end == m.cursor && end < len(m.input) && m.input[end] == '\n' {
		end++
	}
	m.killBuffer = string(m.input[m.cursor:end])
	m.input = append(m.input[:m.cursor], m.input[end:]...)
}

func (m *Model) recordHistory(text string) {
	if strings.TrimSpace(text) == "" {
		return
	}
	if len(m.history) == 0 || m.history[len(m.history)-1] != text {
		m.history = append(m.history, text)
	}
	m.historyIndex = len(m.history)
}

func (m *Model) recallHistory(delta int) bool {
	if len(m.history) == 0 {
		return false
	}
	next := max(0, min(len(m.history), m.historyIndex+delta))
	if next == m.historyIndex {
		return false
	}
	m.historyIndex = next
	if next == len(m.history) {
		m.clearInput()
	} else {
		m.input = []rune(m.history[next])
		m.cursor = len(m.input)
		m.hasSelection = false
	}
	return true
}

func tokenStyle(input []rune, index int) string {
	start, end := index, index
	for start > 0 && !unicode.IsSpace(input[start-1]) {
		start--
	}
	for end < len(input) && !unicode.IsSpace(input[end]) {
		end++
	}
	token := string(input[start:end])
	if strings.HasPrefix(token, "http://") || strings.HasPrefix(token, "https://") {
		return cyan + underline
	}
	if strings.HasPrefix(token, "./") || strings.HasPrefix(token, "../") || strings.HasPrefix(token, "~/") || strings.HasPrefix(token, "/") || strings.HasPrefix(token, "@") {
		return cyan
	}
	if start == 0 && strings.HasPrefix(token, "/") {
		return cyan
	}
	return ""
}

func (m Model) composer(placeholder string) ([]string, int) {
	width := max(8, m.width)
	innerWidth := max(1, width-6) // two columns of padding plus prompt and gap
	inputBG, selectionBG := composerBackgrounds()
	start, end, selected := m.selection()
	segments := composerSegments(m.input, innerWidth)
	lines := make([]string, 0, len(segments)+2)
	lines = append(lines, inputBG+eraseToEnd+reset)
	for lineIndex, segment := range segments {
		var b strings.Builder
		b.WriteString(inputBG + "  ")
		if lineIndex == 0 {
			b.WriteString(bold + "›" + reset + inputBG + " ")
		} else {
			b.WriteString("  ")
		}
		lineStart, lineEnd := segment[0], segment[1]
		if len(m.input) == 0 {
			b.WriteString("\x1b[7m \x1b[27m" + dim + placeholder + reset + inputBG)
			b.WriteString(inputBG + eraseToEnd)
		} else {
			for i := lineStart; i < lineEnd; i++ {
				style := inputBG + tokenStyle(m.input, i)
				if selected && i >= start && i < end {
					style = selectionBG
				}
				b.WriteString(style)
				if i == m.cursor {
					b.WriteString("\x1b[7m")
				}
				b.WriteRune(m.input[i])
				if i == m.cursor {
					b.WriteString("\x1b[27m")
				}
				if strings.Contains(style, underline) {
					b.WriteString(noLine)
				}
			}
			cursorAtEnd := m.cursor == lineEnd && (lineEnd == len(m.input) || m.input[lineEnd] == '\n')
			if cursorAtEnd {
				b.WriteString(inputBG + "\x1b[7m \x1b[27m")
			}
			b.WriteString(inputBG + eraseToEnd)
		}
		b.WriteString(reset)
		lines = append(lines, b.String())
	}
	lines = append(lines, inputBG+eraseToEnd+reset)
	return lines, len(lines)
}

func (m Model) composerPosition(x, y int, clampOutside bool) (int, bool) {
	if m.width <= 0 || m.height <= 0 {
		return 0, false
	}
	innerWidth := max(1, m.width-6)
	segments := composerSegments(m.input, innerWidth)
	_, rows := m.composer("")
	startY := m.height - 1 - rows
	endY := startY + rows - 1
	if !clampOutside && (y < startY || y > endY || x < 0 || x >= m.width) {
		return 0, false
	}
	if len(m.input) == 0 {
		return 0, true
	}
	line := y - startY - 1
	if line < 0 {
		line = 0
	}
	if line >= len(segments) {
		line = len(segments) - 1
	}
	segment := segments[line]
	column := x - 4
	if column < 0 {
		column = 0
	}
	position := segment[0] + column
	return max(segment[0], min(segment[1], position)), true
}

func composerSegments(input []rune, width int) [][2]int {
	if len(input) == 0 {
		return [][2]int{{0, 0}}
	}
	var segments [][2]int
	start := 0
	for i, r := range input {
		if i-start == width {
			segments = append(segments, [2]int{start, i})
			start = i
		}
		if r == '\n' {
			segments = append(segments, [2]int{start, i})
			start = i + 1
		}
	}
	if start < len(input) {
		segments = append(segments, [2]int{start, len(input)})
	} else if input[len(input)-1] == '\n' || (len(segments) > 0 && segments[len(segments)-1][1]-segments[len(segments)-1][0] == width) {
		segments = append(segments, [2]int{start, start})
	}
	return segments
}

func highlightText(text, cwd string) string {
	var out strings.Builder
	for start := 0; start < len(text); {
		end := start
		space := unicode.IsSpace(rune(text[start]))
		for end < len(text) && unicode.IsSpace(rune(text[end])) == space {
			end++
		}
		part := text[start:end]
		if !space {
			out.WriteString(highlightToken(part, cwd))
		} else {
			out.WriteString(part)
		}
		start = end
	}
	return out.String()
}

func highlightToken(raw, cwd string) string {
	left := len(raw) - len(strings.TrimLeft(raw, "()[]{}<>,.;:\"'"))
	rightTrimmed := strings.TrimRight(raw[left:], "()[]{}<>,.;:\"'")
	coreEnd := left + len(rightTrimmed)
	if coreEnd <= left {
		return raw
	}
	core := raw[left:coreEnd]
	prefix, suffix := raw[:left], raw[coreEnd:]
	if strings.HasPrefix(core, "http://") || strings.HasPrefix(core, "https://") {
		return prefix + osc8(core, cyan+underline+core+noLine+reset) + suffix
	}
	if core == "/" || strings.Trim(core, "./") == "" || !strings.Contains(core, "/") {
		return raw
	}
	path := core
	if strings.HasPrefix(path, "~/") {
		if home, err := os.UserHomeDir(); err == nil {
			path = filepath.Join(home, strings.TrimPrefix(path, "~/"))
		}
	} else if !filepath.IsAbs(path) {
		path = filepath.Join(cwd, path)
	}
	if !fileExists(path) {
		return raw
	}
	return prefix + osc8("file://"+filepath.Clean(path), cyan+core+reset) + suffix
}

func stripShellWrapper(command string) string {
	fields := strings.Fields(command)
	if len(fields) < 3 || fields[1] != "-lc" {
		return command
	}
	shell := filepath.Base(strings.Trim(fields[0], "'\""))
	if shell != "bash" && shell != "zsh" && shell != "sh" {
		return command
	}
	index := strings.Index(command, fields[1])
	script := strings.TrimSpace(command[index+len(fields[1]):])
	if len(script) >= 2 && script[0] == '\'' && script[len(script)-1] == '\'' {
		return strings.ReplaceAll(script[1:len(script)-1], `'\''`, `'`)
	}
	if len(script) >= 2 && script[0] == '"' && script[len(script)-1] == '"' {
		if unquoted, err := strconv.Unquote(script); err == nil {
			return unquoted
		}
	}
	return script
}

const (
	shellCommand  = "\x1b[38;2;137;180;250m"
	shellFlag     = "\x1b[38;2;243;139;168m"
	shellString   = "\x1b[38;2;166;227;161m"
	shellOperator = "\x1b[38;2;148;226;213m"
	shellNumber   = "\x1b[38;2;250;179;135m"
	shellVariable = "\x1b[38;2;203;166;247m"
)

func unwrapShellScript(command string) string {
	command = strings.TrimSpace(command)
	if len(command) < 2 || command[0] != command[len(command)-1] {
		return command
	}
	switch command[0] {
	case '\'':
		return strings.ReplaceAll(command[1:len(command)-1], `'\''`, `'`)
	case '"':
		if unquoted, err := strconv.Unquote(command); err == nil {
			return unquoted
		}
	}
	return command
}

func highlightShell(command, cwd string) string {
	command = unwrapShellScript(stripShellWrapper(command))
	var out strings.Builder
	expectCommand := true
	for i := 0; i < len(command); {
		if unicode.IsSpace(rune(command[i])) {
			end := i + 1
			for end < len(command) && unicode.IsSpace(rune(command[end])) {
				end++
			}
			out.WriteString(command[i:end])
			i = end
			continue
		}
		if command[i] == '\'' || command[i] == '"' {
			quote, end := command[i], i+1
			for end < len(command) {
				if command[end] == quote && (end == i+1 || command[end-1] != '\\') {
					end++
					break
				}
				end++
			}
			out.WriteString(shellString + command[i:end] + reset)
			expectCommand = false
			i = end
			continue
		}
		if strings.ContainsRune("|&;<>", rune(command[i])) {
			end := i + 1
			for end < len(command) && strings.ContainsRune("|&;<>", rune(command[end])) {
				end++
			}
			operator := command[i:end]
			out.WriteString(shellOperator + operator + reset)
			if strings.ContainsAny(operator, "|;") || strings.Contains(operator, "&&") {
				expectCommand = true
			}
			i = end
			continue
		}
		end := i + 1
		for end < len(command) && !unicode.IsSpace(rune(command[end])) && command[end] != '\'' && command[end] != '"' && !strings.ContainsRune("|&;<>", rune(command[end])) {
			end++
		}
		word := command[i:end]
		switch {
		case expectCommand && strings.Contains(word, "=") && !strings.HasPrefix(word, "="):
			parts := strings.SplitN(word, "=", 2)
			out.WriteString(shellVariable + parts[0] + reset + shellOperator + "=" + reset + highlightShellWord(parts[1], cwd, false))
			// Environment assignments precede, rather than consume, a command.
		case expectCommand:
			out.WriteString(shellCommand + highlightShellWord(word, cwd, true) + reset)
			expectCommand = false
		case strings.HasPrefix(word, "-") && word != "-":
			out.WriteString(shellFlag + word + reset)
		case strings.HasPrefix(word, "$"):
			out.WriteString(shellVariable + word + reset)
		case isShellNumber(word):
			out.WriteString(shellNumber + word + reset)
		default:
			out.WriteString(highlightShellWord(word, cwd, false))
		}
		i = end
	}
	return out.String()
}

func highlightShellWord(word, cwd string, command bool) string {
	if strings.HasPrefix(word, "http://") || strings.HasPrefix(word, "https://") {
		return osc8(word, cyan+underline+word+noLine+reset)
	}
	if strings.Contains(word, "/") && word != "/" && strings.Trim(word, "./") != "" {
		path := word
		if strings.HasPrefix(path, "~/") {
			if home, err := os.UserHomeDir(); err == nil {
				path = filepath.Join(home, strings.TrimPrefix(path, "~/"))
			}
		} else if !filepath.IsAbs(path) {
			path = filepath.Join(cwd, path)
		}
		if fileExists(path) {
			label := word
			if !command {
				label = cyan + word + reset
			}
			return osc8("file://"+filepath.Clean(path), label)
		}
	}
	return word
}

func isShellNumber(word string) bool {
	if word == "" {
		return false
	}
	for _, r := range word {
		if r < '0' || r > '9' {
			return false
		}
	}
	return true
}

func fileExists(path string) bool {
	_, err := os.Stat(path)
	return err == nil
}

func osc8(destination, label string) string {
	return "\x1b]8;;" + destination + "\x1b\\" + label + "\x1b]8;;\x1b\\"
}
