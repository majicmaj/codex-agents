package tui

import (
	"strings"

	"github.com/charmbracelet/x/ansi"
)

const (
	italic   = "\x1b[3m"
	noItalic = "\x1b[23m"
	strike   = "\x1b[9m"
	noStrike = "\x1b[29m"
)

// renderMarkdown mirrors Codex's restrained transcript styling: Markdown
// punctuation disappears, code and links are cyan, and prose remains plain.
func renderMarkdown(text string, width int, cwd string) []string {
	width = max(1, width)
	text = expandTranscriptTabs(text)
	var result []string
	inFence := false
	for _, source := range strings.Split(text, "\n") {
		trimmed := strings.TrimSpace(source)
		if strings.HasPrefix(trimmed, "```") {
			inFence = !inFence
			continue
		}
		if source == "" {
			result = append(result, "")
			continue
		}
		prefix, body, blockStyle := markdownBlock(source, inFence)
		styled := blockStyle + prefix + renderInlineMarkdown(body, cwd)
		if blockStyle != "" {
			styled += reset
		}
		wrapped := ansi.Hardwrap(ansi.Wordwrap(styled, width, ""), width, false)
		result = append(result, strings.Split(wrapped, "\n")...)
	}
	if len(result) == 0 {
		return []string{""}
	}
	return result
}

// Match Codex's transcript normalization: raw tab controls interact badly
// with gutters and terminal width accounting, so each tab has a stable
// four-column representation before styling, wrapping, and virtualization.
func expandTranscriptTabs(text string) string {
	if !strings.ContainsRune(text, '\t') {
		return text
	}
	return strings.ReplaceAll(text, "\t", "    ")
}

func markdownBlock(line string, inFence bool) (prefix, body, style string) {
	if inFence {
		return "  ", line, cyan
	}
	trimmed := strings.TrimLeft(line, " ")
	indent := line[:len(line)-len(trimmed)]
	for level := 6; level >= 1; level-- {
		marker := strings.Repeat("#", level) + " "
		if strings.HasPrefix(trimmed, marker) {
			return indent, strings.TrimPrefix(trimmed, marker), bold
		}
	}
	if len(trimmed) >= 2 && strings.Contains("-*+", trimmed[:1]) && trimmed[1] == ' ' {
		return indent + "- ", trimmed[2:], ""
	}
	if strings.HasPrefix(trimmed, "> ") {
		return indent + "│ ", trimmed[2:], green
	}
	return "", line, ""
}

func renderInlineMarkdown(text, cwd string) string {
	var out strings.Builder
	for len(text) > 0 {
		switch {
		case strings.HasPrefix(text, "["):
			closeLabel := strings.Index(text, "](")
			if closeLabel > 0 {
				closeURL := strings.Index(text[closeLabel+2:], ")")
				if closeURL >= 0 {
					closeURL += closeLabel + 2
					label, destination := text[1:closeLabel], text[closeLabel+2:closeURL]
					out.WriteString(osc8(destination, cyan+underline+label+noLine+reset))
					text = text[closeURL+1:]
					continue
				}
			}
		case strings.HasPrefix(text, "**"):
			if end := strings.Index(text[2:], "**"); end >= 0 {
				out.WriteString(bold + text[2:2+end] + reset)
				text = text[2+end+2:]
				continue
			}
		case strings.HasPrefix(text, "~~"):
			if end := strings.Index(text[2:], "~~"); end >= 0 {
				out.WriteString(strike + text[2:2+end] + noStrike)
				text = text[2+end+2:]
				continue
			}
		case strings.HasPrefix(text, "`"):
			if end := strings.Index(text[1:], "`"); end >= 0 {
				out.WriteString(cyan + text[1:1+end] + reset)
				text = text[1+end+1:]
				continue
			}
		case strings.HasPrefix(text, "*"):
			if end := strings.Index(text[1:], "*"); end > 0 {
				out.WriteString(italic + text[1:1+end] + noItalic)
				text = text[1+end+1:]
				continue
			}
		}
		next := nextMarkdownStart(text)
		out.WriteString(highlightText(text[:next], cwd))
		text = text[next:]
	}
	return out.String()
}

func nextMarkdownStart(text string) int {
	if len(text) <= 1 {
		return len(text)
	}
	next := len(text)
	for _, marker := range []string{"[", "**", "~~", "`", "*"} {
		if index := strings.Index(text[1:], marker); index >= 0 && index+1 < next {
			next = index + 1
		}
	}
	return next
}
