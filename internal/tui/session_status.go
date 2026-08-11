package tui

import (
	"bufio"
	"bytes"
	"encoding/json"
	"fmt"
	"io"
	"os"
	"path/filepath"
	"strconv"
	"strings"
	"sync"
	"time"

	tea "charm.land/bubbletea/v2"
	"github.com/majd/codex-agents/internal/appserver"
)

// sessionStatusProbe overlays the status of Codex CLI processes that belong to
// a different App Server. thread/list calls these threads notLoaded; rollout
// task events provide the cross-process live signal Agent View needs.
type sessionStatusProbe struct {
	mu        sync.Mutex
	indexed   bool
	paths     map[string]string
	modified  map[string]int64
	sizes     map[string]int64
	statuses  map[string]appserver.Status
	recaps    map[string]string
	reviewAt  map[string]int64
	reviews   map[string][]rolloutReview
	lastIndex time.Time
}

func newSessionStatusProbe() *sessionStatusProbe {
	return &sessionStatusProbe{
		paths: make(map[string]string), modified: make(map[string]int64), sizes: make(map[string]int64), statuses: make(map[string]appserver.Status),
		recaps: make(map[string]string), reviewAt: make(map[string]int64), reviews: make(map[string][]rolloutReview),
	}
}

type rolloutReview struct {
	ID     string
	TurnID string
}

func (p *sessionStatusProbe) approvals(id string) []rolloutReview {
	if p == nil {
		return nil
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if !p.indexed {
		p.index()
	}
	path := p.paths[id]
	file, err := os.Open(path)
	if path == "" || err != nil {
		return nil
	}
	defer file.Close()
	start := p.reviewAt[id]
	if start > 0 {
		_, _ = file.Seek(start, io.SeekStart)
	}
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 64*1024), 64*1024*1024)
	for scanner.Scan() {
		var entry struct {
			Type    string `json:"type"`
			Payload struct {
				Type   string `json:"type"`
				ID     string `json:"id"`
				CallID string `json:"call_id"`
				Name   string `json:"name"`
				Input  string `json:"input"`
				Meta   struct {
					TurnID string `json:"turn_id"`
				} `json:"internal_chat_message_metadata_passthrough"`
			} `json:"payload"`
		}
		if json.Unmarshal(scanner.Bytes(), &entry) != nil || entry.Type != "response_item" || entry.Payload.Type != "custom_tool_call" {
			continue
		}
		if entry.Payload.Name != "exec" || !strings.Contains(entry.Payload.Input, "require_escalated") {
			continue
		}
		key := entry.Payload.CallID
		if key == "" {
			key = entry.Payload.ID
		}
		p.reviews[id] = append(p.reviews[id], rolloutReview{ID: "review-" + key, TurnID: entry.Payload.Meta.TurnID})
	}
	if info, statErr := file.Stat(); statErr == nil {
		p.reviewAt[id] = info.Size()
	}
	return append([]rolloutReview(nil), p.reviews[id]...)
}

func scanExternalStatuses(probe *sessionStatusProbe, threads []appserver.Thread) tea.Cmd {
	return func() tea.Msg {
		if probe == nil {
			return externalStatusesMsg{}
		}
		return externalStatusesMsg(probe.scan(threads))
	}
}

func (p *sessionStatusProbe) scan(threads []appserver.Thread) map[string]sessionStatusSnapshot {
	p.mu.Lock()
	defer p.mu.Unlock()
	if !p.indexed {
		p.index()
	}
	for _, thread := range threads {
		if p.paths[thread.ID] == "" && time.Since(p.lastIndex) >= 10*time.Second {
			p.index()
			break
		}
	}
	result := make(map[string]sessionStatusSnapshot)
	for _, thread := range threads {
		path := p.paths[thread.ID]
		if path == "" {
			continue
		}
		info, err := os.Stat(path)
		if err != nil {
			continue
		}
		stamp := info.ModTime().UnixNano()
		if p.modified[thread.ID] != stamp {
			start := int64(-1)
			if _, cached := p.statuses[thread.ID]; cached && p.sizes[thread.ID] <= info.Size() {
				start = p.sizes[thread.ID]
			}
			status, recap, ok := rolloutStateSince(path, start)
			if ok {
				p.statuses[thread.ID] = status
			}
			if recap != "" {
				p.recaps[thread.ID] = recap
			}
			p.modified[thread.ID] = stamp
			p.sizes[thread.ID] = info.Size()
		}
		if status, ok := p.statuses[thread.ID]; ok {
			result[thread.ID] = sessionStatusSnapshot{Status: status, Recap: p.recaps[thread.ID]}
		}
	}
	return result
}

func (p *sessionStatusProbe) rolloutStamp(id string) string {
	if p == nil {
		return ""
	}
	p.mu.Lock()
	defer p.mu.Unlock()
	if !p.indexed {
		p.index()
	}
	path := p.paths[id]
	info, err := os.Stat(path)
	if path == "" || err != nil {
		return ""
	}
	return fmt.Sprintf("%d:%d", info.ModTime().UnixNano(), info.Size())
}

func (p *sessionStatusProbe) index() {
	p.indexed = true
	p.lastIndex = time.Now()
	home, err := os.UserHomeDir()
	if err != nil {
		return
	}
	root := filepath.Join(home, ".codex", "sessions")
	_ = filepath.WalkDir(root, func(path string, entry os.DirEntry, walkErr error) error {
		if walkErr != nil || entry.IsDir() || !strings.HasSuffix(entry.Name(), ".jsonl") {
			return nil
		}
		name := strings.TrimSuffix(entry.Name(), ".jsonl")
		parts := strings.Split(name, "-")
		if len(parts) >= 7 {
			p.paths[strings.Join(parts[len(parts)-5:], "-")] = path
		}
		return nil
	})
}

func statusFromRollout(path string) (appserver.Status, bool) {
	return statusFromRolloutSince(path, -1)
}

func statusFromRolloutSince(path string, start int64) (appserver.Status, bool) {
	status, _, ok := rolloutStateSince(path, start)
	return status, ok
}

func rolloutStateSince(path string, start int64) (appserver.Status, string, bool) {
	file, err := os.Open(path)
	if err != nil {
		return appserver.Status{}, "", false
	}
	defer file.Close()
	if start < 0 {
		status, ok := latestTaskStatus(file)
		return status, latestRolloutRecap(file), ok
	}
	if start > 0 {
		_, _ = file.Seek(start, io.SeekStart)
	}
	scanner := bufio.NewScanner(file)
	scanner.Buffer(make([]byte, 64*1024), 8*1024*1024)
	status := appserver.Status{}
	recap := ""
	found := false
	for scanner.Scan() {
		entryType, payload, ok := rolloutEntry(scanner.Bytes())
		if !ok {
			continue
		}
		if candidate := recapFromRolloutPayload(entryType, payload); candidate != "" {
			recap = candidate
		}
		if entryType != "event_msg" {
			continue
		}
		switch payload.Type {
		case "task_started":
			status, found = appserver.Status{Type: "active", StartedAt: payload.StartedAt}, true
		case "task_complete", "turn_aborted":
			status, found = appserver.Status{Type: "idle"}, true
		case "exec_approval_request", "apply_patch_approval_request":
			status, found = appserver.Status{Type: "active", ActiveFlags: []string{"waitingOnApproval"}}, true
		case "request_user_input":
			status, found = appserver.Status{Type: "active", ActiveFlags: []string{"waitingOnUserInput"}}, true
		}
	}
	return status, recap, found
}

type rolloutPayload struct {
	Type      string `json:"type"`
	StartedAt int64  `json:"started_at"`
	Role      string `json:"role"`
	Name      string `json:"name"`
	Message   string `json:"message"`
	Content   []struct {
		Type string `json:"type"`
		Text string `json:"text"`
	} `json:"content"`
	Questions []struct {
		Header   string `json:"header"`
		Question string `json:"question"`
	} `json:"questions"`
}

func rolloutEntry(line []byte) (string, rolloutPayload, bool) {
	var entry struct {
		Type    string          `json:"type"`
		Payload json.RawMessage `json:"payload"`
	}
	if json.Unmarshal(line, &entry) != nil || len(entry.Payload) == 0 {
		return "", rolloutPayload{}, false
	}
	var payload rolloutPayload
	if json.Unmarshal(entry.Payload, &payload) != nil {
		return "", rolloutPayload{}, false
	}
	return entry.Type, payload, true
}

func recapFromRolloutPayload(entryType string, payload rolloutPayload) string {
	switch entryType {
	case "event_msg":
		switch payload.Type {
		case "agent_message":
			return clean(payload.Message)
		case "user_message":
			if text := clean(payload.Message); text != "" {
				return "Asked: " + text
			}
		case "request_user_input":
			if len(payload.Questions) > 0 {
				question := clean(payload.Questions[0].Question)
				if question == "" {
					question = clean(payload.Questions[0].Header)
				}
				if question != "" {
					return "Waiting for input: " + question
				}
			}
			return "Waiting for your response"
		case "exec_approval_request", "apply_patch_approval_request":
			return "Waiting for approval"
		case "turn_aborted":
			return "Turn interrupted"
		}
	case "response_item":
		switch payload.Type {
		case "message":
			if payload.Role != "assistant" && payload.Role != "user" {
				return ""
			}
			for i := len(payload.Content) - 1; i >= 0; i-- {
				if text := clean(payload.Content[i].Text); text != "" {
					if payload.Role == "user" {
						return "Asked: " + text
					}
					return text
				}
			}
		case "function_call", "custom_tool_call":
			if name := clean(payload.Name); name != "" {
				switch name {
				case "exec", "exec_command", "shell", "shell_command":
					return "Running a command"
				case "apply_patch":
					return "Editing files"
				case "web", "web_search":
					return "Searching the web"
				default:
					return "Using: " + name
				}
			}
		}
	}
	return ""
}

func latestRolloutRecap(file *os.File) string {
	info, err := file.Stat()
	if err != nil || info.Size() == 0 {
		return ""
	}
	const tailSize int64 = 512 * 1024
	start := max(int64(0), info.Size()-tailSize)
	buffer := make([]byte, info.Size()-start)
	if _, err := file.ReadAt(buffer, start); err != nil && err != io.EOF {
		return ""
	}
	lines := bytes.Split(buffer, []byte{'\n'})
	for i := len(lines) - 1; i >= 0; i-- {
		entryType, payload, ok := rolloutEntry(lines[i])
		if !ok {
			continue
		}
		if recap := recapFromRolloutPayload(entryType, payload); recap != "" {
			return recap
		}
	}
	return ""
}

// latestTaskStatus searches backward in fixed-size blocks. It finds the most
// recent lifecycle marker even when a running turn has emitted gigabytes after
// task_started, without parsing the whole rollout from the beginning.
func latestTaskStatus(file *os.File) (appserver.Status, bool) {
	info, err := file.Stat()
	if err != nil {
		return appserver.Status{}, false
	}
	const blockSize int64 = 256 * 1024
	const overlap int64 = 256
	tokens := []struct {
		value  []byte
		kind   string
		flag   string
		starts bool
	}{
		{[]byte(`"type":"task_started"`), "active", "", true},
		{[]byte(`"type":"task_complete"`), "idle", "", false},
		{[]byte(`"type":"turn_aborted"`), "idle", "", false},
		{[]byte(`"type":"exec_approval_request"`), "active", "waitingOnApproval", false},
		{[]byte(`"type":"apply_patch_approval_request"`), "active", "waitingOnApproval", false},
		{[]byte(`"type":"request_user_input"`), "active", "waitingOnUserInput", false},
	}
	for end := info.Size(); end > 0; {
		start := max(int64(0), end-blockSize)
		buffer := make([]byte, end-start)
		if _, err := file.ReadAt(buffer, start); err != nil && err != io.EOF {
			return appserver.Status{}, false
		}
		kind, flag, starts, index := "", "", false, -1
		for _, token := range tokens {
			if candidate := bytes.LastIndex(buffer, token.value); candidate > index {
				kind, flag, starts, index = token.kind, token.flag, token.starts, candidate
			}
		}
		if index >= 0 {
			status := appserver.Status{Type: kind}
			if flag != "" {
				status.ActiveFlags = []string{flag}
			}
			if starts {
				lineEnd := bytes.IndexByte(buffer[index:], '\n')
				if lineEnd < 0 {
					lineEnd = len(buffer) - index
				}
				status.StartedAt = startedAtFromTaskLine(buffer[index : index+lineEnd])
			}
			return status, true
		}
		if start == 0 {
			break
		}
		end = start + overlap
	}
	return appserver.Status{}, false
}

func startedAtFromTaskLine(line []byte) int64 {
	const marker = `"started_at":`
	index := bytes.Index(line, []byte(marker))
	if index < 0 {
		return 0
	}
	value := line[index+len(marker):]
	end := 0
	for end < len(value) && value[end] >= '0' && value[end] <= '9' {
		end++
	}
	started, _ := strconv.ParseInt(string(value[:end]), 10, 64)
	return started
}

func (m *Model) mergeExternalStatus(id string, status appserver.Status) {
	for i := range m.threads {
		if m.threads[i].ID != id {
			continue
		}
		if m.ownedThreads[id] && m.threads[i].Status.Type == "active" && status.Type != "active" {
			return
		}
		wasActive := m.threads[i].Status.Type == "active"
		m.threads[i].Status = status
		if status.Type == "active" && (!wasActive || m.turnStarted[id].IsZero()) {
			m.turnStarted[id] = time.Now()
			if status.StartedAt > 0 {
				m.turnStarted[id] = time.Unix(status.StartedAt, 0)
			}
		}
		if status.Type != "active" {
			delete(m.turnStarted, id)
			if wasActive && m.sessionID != id {
				if m.unread == nil {
					m.unread = make(map[string]bool)
				}
				m.unread[id] = true
			}
		}
		return
	}
}
