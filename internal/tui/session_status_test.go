package tui

import (
	"os"
	"path/filepath"
	"strings"
	"testing"
)

func TestStatusFromRollout(t *testing.T) {
	path := filepath.Join(t.TempDir(), "rollout.jsonl")
	content := "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\"}}\n" +
		"{\"type\":\"event_msg\",\"payload\":{\"type\":\"request_user_input\"}}\n"
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	status, ok := statusFromRollout(path)
	if !ok || status.Type != "active" || len(status.ActiveFlags) != 1 || status.ActiveFlags[0] != "waitingOnUserInput" {
		t.Fatalf("unexpected live status: %#v, %v", status, ok)
	}
	file, err := os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	_, _ = file.WriteString("{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_complete\"}}\n")
	_ = file.Close()
	status, ok = statusFromRollout(path)
	if !ok || status.Type != "idle" {
		t.Fatalf("completed rollout status: %#v, %v", status, ok)
	}
}

func TestStatusFromLargeActiveRollout(t *testing.T) {
	path := filepath.Join(t.TempDir(), "large-rollout.jsonl")
	content := "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"started_at\":12345}}\n" +
		strings.Repeat("{\"type\":\"response_item\",\"payload\":{\"text\":\"noise\"}}\n", 100_000)
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	status, ok := statusFromRollout(path)
	if !ok || status.Type != "active" || status.StartedAt != 12345 {
		t.Fatalf("large active rollout was not detected: %#v, %v", status, ok)
	}
}

func TestRolloutApprovalIndexIsIncremental(t *testing.T) {
	root := t.TempDir()
	path := filepath.Join(root, "rollout-2026-08-11T00-00-00-thread-id-with-five-parts.jsonl")
	line := `{"type":"response_item","payload":{"type":"custom_tool_call","id":"call-item","call_id":"call-1","name":"exec","input":"sandbox_permissions: \"require_escalated\"","internal_chat_message_metadata_passthrough":{"turn_id":"turn-1"}}}` + "\n"
	if err := os.WriteFile(path, []byte(line), 0o600); err != nil {
		t.Fatal(err)
	}
	probe := newSessionStatusProbe()
	probe.indexed = true
	probe.paths["thread"] = path
	first := probe.approvals("thread")
	second := probe.approvals("thread")
	if len(first) != 1 || len(second) != 1 || first[0].TurnID != "turn-1" {
		t.Fatalf("approval index duplicated or missed review: first=%#v second=%#v", first, second)
	}
}

func TestRolloutRecapUsesNewestMeaningfulActivity(t *testing.T) {
	path := filepath.Join(t.TempDir(), "recap-rollout.jsonl")
	content := "{\"type\":\"event_msg\",\"payload\":{\"type\":\"task_started\",\"started_at\":12345}}\n" +
		"{\"type\":\"response_item\",\"payload\":{\"type\":\"message\",\"role\":\"assistant\",\"content\":[{\"type\":\"output_text\",\"text\":\"Older answer\"}]}}\n" +
		"{\"type\":\"event_msg\",\"payload\":{\"type\":\"agent_message\",\"message\":\"Newest concise update\"}}\n"
	if err := os.WriteFile(path, []byte(content), 0o600); err != nil {
		t.Fatal(err)
	}
	status, recap, ok := rolloutStateSince(path, -1)
	if !ok || status.Type != "active" || recap != "Newest concise update" {
		t.Fatalf("initial rollout snapshot = %#v, %q, %v", status, recap, ok)
	}
	start := int64(len(content))
	file, err := os.OpenFile(path, os.O_APPEND|os.O_WRONLY, 0)
	if err != nil {
		t.Fatal(err)
	}
	_, _ = file.WriteString("{\"type\":\"event_msg\",\"payload\":{\"type\":\"request_user_input\",\"questions\":[{\"header\":\"Choice\",\"question\":\"Which approach should I use?\"}]}}\n")
	_ = file.Close()
	status, recap, ok = rolloutStateSince(path, start)
	if !ok || len(status.ActiveFlags) != 1 || status.ActiveFlags[0] != "waitingOnUserInput" || recap != "Waiting for input: Which approach should I use?" {
		t.Fatalf("incremental rollout snapshot = %#v, %q, %v", status, recap, ok)
	}
}

func TestRolloutRecapSkipsInternalDeveloperMessages(t *testing.T) {
	developer := []byte(`{"type":"response_item","payload":{"type":"message","role":"developer","content":[{"type":"input_text","text":"internal instructions"}]}}`)
	entryType, payload, ok := rolloutEntry(developer)
	if !ok || recapFromRolloutPayload(entryType, payload) != "" {
		t.Fatal("developer message leaked into the overview recap")
	}
	tool := []byte(`{"type":"response_item","payload":{"type":"custom_tool_call","name":"exec"}}`)
	entryType, payload, ok = rolloutEntry(tool)
	if !ok || recapFromRolloutPayload(entryType, payload) != "Running a command" {
		t.Fatal("tool activity did not produce a concise recap")
	}
}
