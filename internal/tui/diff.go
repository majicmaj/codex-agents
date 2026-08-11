package tui

import (
	"encoding/json"
	"fmt"
	"path/filepath"
	"regexp"
	"sort"
	"strconv"
	"strings"
	"unicode"

	"github.com/charmbracelet/x/ansi"
)

const (
	diffAddBG    = "\x1b[48;2;25;55;42m"
	diffDeleteBG = "\x1b[48;2;64;34;31m"
	diffAddFG    = "\x1b[38;2;166;227;161m"
	diffDeleteFG = "\x1b[38;2;243;139;168m"
	diffKeyword  = "\x1b[38;2;203;166;247m"
	diffString   = "\x1b[38;2;166;227;161m"
	diffNumber   = "\x1b[38;2;250;179;135m"
	diffType     = "\x1b[38;2;137;220;235m"
)

type fileChange struct {
	Path     string
	Kind     string
	MovePath string
	Diff     string
}

type commandAction struct {
	Type    string `json:"type"`
	Command string `json:"command"`
	Name    string `json:"name"`
	Path    string `json:"path"`
	Query   string `json:"query"`
}

func explorable(actions []commandAction) bool {
	if len(actions) == 0 {
		return false
	}
	for _, action := range actions {
		if action.Type == "unknown" || action.Type == "" {
			return false
		}
	}
	return true
}

func renderExplored(actions []commandAction, width int, cwd string) []string {
	lines := []string{dim + "•" + reset + " " + bold + "Explored" + reset}
	for i, action := range actions {
		title, detail := "Run", action.Command
		switch action.Type {
		case "read":
			title, detail = "Read", action.Name
			if detail == "" {
				detail = displayChangePath(action.Path, cwd)
			}
		case "listFiles":
			title, detail = "List", action.Path
			if detail == "" {
				detail = action.Command
			}
		case "search":
			title, detail = "Search", action.Query
			if detail == "" {
				detail = action.Command
			}
			if action.Path != "" {
				detail += " in " + displayChangePath(action.Path, cwd)
			}
		}
		prefix := "    "
		if i == 0 {
			prefix = dim + "  └ " + reset
		}
		row := prefix + cyan + title + reset + " " + highlightText(detail, cwd)
		lines = append(lines, strings.Split(ansi.Hardwrap(ansi.Wordwrap(row, width, ""), width, false), "\n")...)
	}
	return lines
}

func parseFileChanges(rawChanges []json.RawMessage) []fileChange {
	changes := make([]fileChange, 0, len(rawChanges))
	for _, raw := range rawChanges {
		var wire struct {
			Path string          `json:"path"`
			Kind json.RawMessage `json:"kind"`
			Diff string          `json:"diff"`
		}
		if json.Unmarshal(raw, &wire) != nil || wire.Path == "" {
			continue
		}
		change := fileChange{Path: wire.Path, Diff: wire.Diff}
		if json.Unmarshal(wire.Kind, &change.Kind) != nil {
			var kind struct {
				Type     string `json:"type"`
				MovePath string `json:"movePath"`
			}
			_ = json.Unmarshal(wire.Kind, &kind)
			change.Kind, change.MovePath = kind.Type, kind.MovePath
		}
		changes = append(changes, change)
	}
	sort.Slice(changes, func(i, j int) bool { return changes[i].Path < changes[j].Path })
	return changes
}

type renderedDiffLine struct {
	number int
	kind   byte
	text   string
}

var hunkHeader = regexp.MustCompile(`^@@ -([0-9]+)(?:,[0-9]+)? \+([0-9]+)(?:,[0-9]+)? @@`)

func parseDiffLines(change fileChange) []renderedDiffLine {
	if change.Kind == "add" || change.Kind == "delete" {
		if change.Diff == "" {
			return nil
		}
		kind := byte('+')
		if change.Kind == "delete" {
			kind = '-'
		}
		var lines []renderedDiffLine
		for i, line := range strings.Split(strings.TrimSuffix(change.Diff, "\n"), "\n") {
			lines = append(lines, renderedDiffLine{number: i + 1, kind: kind, text: line})
		}
		return lines
	}
	var lines []renderedDiffLine
	oldLine, newLine := 0, 0
	for _, line := range strings.Split(change.Diff, "\n") {
		if match := hunkHeader.FindStringSubmatch(line); match != nil {
			oldLine, _ = strconv.Atoi(match[1])
			newLine, _ = strconv.Atoi(match[2])
			if len(lines) > 0 {
				lines = append(lines, renderedDiffLine{kind: '@', text: "⋮"})
			}
			continue
		}
		if oldLine == 0 && newLine == 0 || strings.HasPrefix(line, "---") || strings.HasPrefix(line, "+++") || strings.HasPrefix(line, `\ No newline`) {
			continue
		}
		switch {
		case strings.HasPrefix(line, "+"):
			lines = append(lines, renderedDiffLine{number: newLine, kind: '+', text: line[1:]})
			newLine++
		case strings.HasPrefix(line, "-"):
			lines = append(lines, renderedDiffLine{number: oldLine, kind: '-', text: line[1:]})
			oldLine++
		default:
			text := strings.TrimPrefix(line, " ")
			lines = append(lines, renderedDiffLine{number: newLine, kind: ' ', text: text})
			oldLine++
			newLine++
		}
	}
	return lines
}

func renderFileChanges(changes []fileChange, width int, cwd string) []string {
	totalAdd, totalDelete := 0, 0
	parsed := make([][]renderedDiffLine, len(changes))
	for i, change := range changes {
		parsed[i] = parseDiffLines(change)
		for _, line := range parsed[i] {
			if line.kind == '+' {
				totalAdd++
			}
			if line.kind == '-' {
				totalDelete++
			}
		}
	}
	verb := "Edited"
	if len(changes) == 1 {
		if changes[0].Kind == "add" {
			verb = "Added"
		}
		if changes[0].Kind == "delete" {
			verb = "Deleted"
		}
	}
	target := fmt.Sprintf("%d files", len(changes))
	if len(changes) == 1 {
		target = displayChangePath(changes[0].Path, cwd)
		if changes[0].MovePath != "" {
			target += " → " + displayChangePath(changes[0].MovePath, cwd)
		}
	}
	header := dim + "•" + reset + " " + bold + verb + reset + " " + target + " (" + green + fmt.Sprintf("+%d", totalAdd) + reset + " " + red + fmt.Sprintf("-%d", totalDelete) + reset + ")"
	lines := []string{header}
	for i, change := range changes {
		if i > 0 {
			lines = append(lines, "")
		}
		if len(changes) > 1 {
			add, remove := 0, 0
			for _, line := range parsed[i] {
				if line.kind == '+' {
					add++
				}
				if line.kind == '-' {
					remove++
				}
			}
			lines = append(lines, dim+"  └ "+reset+displayChangePath(change.Path, cwd)+" ("+green+fmt.Sprintf("+%d", add)+reset+" "+red+fmt.Sprintf("-%d", remove)+reset+")")
		}
		lines = append(lines, renderDiffBody(parsed[i], change.Path, width)...)
	}
	return lines
}

func displayChangePath(path, cwd string) string {
	if filepath.IsAbs(path) {
		if relative, err := filepath.Rel(cwd, path); err == nil && relative != ".." && !strings.HasPrefix(relative, "../") {
			return relative
		}
	}
	return path
}

func renderDiffBody(diff []renderedDiffLine, path string, width int) []string {
	maxNumber := 1
	for _, line := range diff {
		maxNumber = max(maxNumber, line.number)
	}
	numberWidth := len(strconv.Itoa(maxNumber))
	contentWidth := max(1, width-numberWidth-6)
	var result []string
	for _, line := range diff {
		if line.kind == '@' {
			result = append(result, strings.Repeat(" ", numberWidth+5)+dim+"⋮"+reset)
			continue
		}
		wrapped := wrap(line.text, contentWidth)
		if len(wrapped) == 0 {
			wrapped = []string{""}
		}
		for i, content := range wrapped {
			number := ""
			sign := byte(' ')
			if i == 0 {
				number, sign = strconv.Itoa(line.number), line.kind
			}
			prefix := fmt.Sprintf("  %*s %c", numberWidth, number, sign)
			bg, fg := "", ""
			if line.kind == '+' {
				bg, fg = diffAddBG, diffAddFG
			}
			if line.kind == '-' {
				bg, fg = diffDeleteBG, diffDeleteFG+dim
			}
			styled := highlightCodeLine(content, path, bg)
			row := bg + fg + prefix + reset + bg + styled
			row += backgroundFill(max(0, width-ansi.StringWidth(row))) + reset
			result = append(result, row)
		}
	}
	return result
}

var codeKeywords = map[string]bool{
	"break": true, "case": true, "chan": true, "const": true, "continue": true, "default": true, "defer": true, "else": true, "fallthrough": true, "for": true, "func": true, "go": true, "goto": true, "if": true, "import": true, "interface": true, "map": true, "package": true, "range": true, "return": true, "select": true, "struct": true, "switch": true, "type": true, "var": true,
	"class": true, "def": true, "from": true, "in": true, "is": true, "lambda": true, "new": true, "private": true, "protected": true, "public": true, "static": true, "try": true, "except": true, "finally": true, "throw": true, "throws": true, "while": true,
}
var codeTypes = map[string]bool{"bool": true, "byte": true, "error": true, "float32": true, "float64": true, "int": true, "int8": true, "int16": true, "int32": true, "int64": true, "rune": true, "string": true, "uint": true, "uint8": true, "uint16": true, "uint32": true, "uint64": true, "uintptr": true, "nil": true, "true": true, "false": true}

func highlightCodeLine(line, path, bg string) string {
	_ = path // Kept in the API so language-specific lexers can remain lightweight.
	var out strings.Builder
	for i := 0; i < len(line); {
		if strings.HasPrefix(line[i:], "//") || line[i] == '#' {
			out.WriteString(dim + line[i:] + reset + bg)
			break
		}
		if line[i] == '\'' || line[i] == '"' || line[i] == '`' {
			quote, end := line[i], i+1
			for end < len(line) {
				if line[end] == quote && (end == i+1 || line[end-1] != '\\') {
					end++
					break
				}
				end++
			}
			out.WriteString(diffString + line[i:end] + reset + bg)
			i = end
			continue
		}
		if unicode.IsLetter(rune(line[i])) || line[i] == '_' {
			end := i + 1
			for end < len(line) && (unicode.IsLetter(rune(line[end])) || unicode.IsDigit(rune(line[end])) || line[end] == '_') {
				end++
			}
			word, style := line[i:end], ""
			if codeKeywords[word] {
				style = diffKeyword
			}
			if codeTypes[word] {
				style = diffType
			}
			out.WriteString(style + word)
			if style != "" {
				out.WriteString(reset + bg)
			}
			i = end
			continue
		}
		if unicode.IsDigit(rune(line[i])) {
			end := i + 1
			for end < len(line) && (unicode.IsDigit(rune(line[end])) || strings.ContainsRune("._xabcdefABCDEF", rune(line[end]))) {
				end++
			}
			out.WriteString(diffNumber + line[i:end] + reset + bg)
			i = end
			continue
		}
		out.WriteByte(line[i])
		i++
	}
	return out.String()
}
