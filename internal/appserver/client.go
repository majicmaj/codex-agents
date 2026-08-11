package appserver

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"net"
	"os/exec"
	"strings"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gorilla/websocket"
)

type Status struct {
	Type        string   `json:"type"`
	ActiveFlags []string `json:"activeFlags,omitempty"`
	StartedAt   int64    `json:"-"`
}

type Thread struct {
	ID        string  `json:"id"`
	Path      string  `json:"path"`
	Name      *string `json:"name"`
	Preview   string  `json:"preview"`
	Cwd       string  `json:"cwd"`
	CreatedAt int64   `json:"createdAt"`
	UpdatedAt int64   `json:"updatedAt"`
	RecencyAt *int64  `json:"recencyAt"`
	Status    Status  `json:"status"`
	Turns     []Turn  `json:"turns"`
}

type Turn struct {
	ID          string            `json:"id"`
	Status      string            `json:"status"`
	StartedAt   *int64            `json:"startedAt"`
	CompletedAt *int64            `json:"completedAt"`
	DurationMS  *int64            `json:"durationMs"`
	Items       []json.RawMessage `json:"items"`
}

type Event struct {
	Method string
	Params json.RawMessage
	ID     json.RawMessage
}

type rpcMessage struct {
	ID     json.RawMessage `json:"id,omitempty"`
	Method string          `json:"method,omitempty"`
	Params json.RawMessage `json:"params,omitempty"`
	Result json.RawMessage `json:"result,omitempty"`
	Error  *rpcError       `json:"error,omitempty"`
}

type rpcError struct {
	Code    int             `json:"code"`
	Message string          `json:"message"`
	Data    json.RawMessage `json:"data,omitempty"`
}

func (e *rpcError) Error() string { return fmt.Sprintf("app-server %d: %s", e.Code, e.Message) }

var ErrActiveWriter = errors.New("thread already has an active writer")

func IsActiveWriterError(err error) bool {
	if errors.Is(err, ErrActiveWriter) {
		return true
	}
	var rpcErr *rpcError
	return errors.As(err, &rpcErr) && rpcErr.Code == -32600 && strings.Contains(rpcErr.Message, "already has an active writer")
}

type Client struct {
	conn    *websocket.Conn
	events  chan Event
	pending map[string]chan rpcMessage
	mu      sync.Mutex
	writeMu sync.Mutex
	nextID  atomic.Uint64
	done    chan struct{}
	errMu   sync.Mutex
	err     error
}

func Start(ctx context.Context, clientVersion string) (*Client, error) {
	if clientVersion == "" {
		clientVersion = "dev"
	}
	if _, err := exec.LookPath("codex"); err != nil {
		return nil, errors.New("codex CLI was not found in PATH")
	}

	// Keep one durable App Server as the sole rollout writer, then attach this
	// client through its Unix-socket WebSocket transport. Native Codex TUIs can
	// attach to the same server with `--remote unix://`, so both surfaces can
	// subscribe, steer, interrupt, and start turns without competing writer
	// locks. Starting the daemon is idempotent.
	startCtx, cancel := context.WithTimeout(ctx, 15*time.Second)
	defer cancel()
	startCommand := exec.CommandContext(startCtx, "codex", "app-server", "daemon", "start")
	var startStderr bytes.Buffer
	startCommand.Stderr = &startStderr
	output, err := startCommand.Output()
	if err != nil {
		message := strings.TrimSpace(startStderr.String())
		if message != "" {
			return nil, fmt.Errorf("start shared app-server daemon: %w: %s", err, message)
		}
		return nil, fmt.Errorf("start shared app-server daemon: %w", err)
	}

	var daemon struct {
		SocketPath string `json:"socketPath"`
	}
	if err := json.Unmarshal(output, &daemon); err != nil {
		return nil, fmt.Errorf("decode shared app-server address: %w", err)
	}
	if daemon.SocketPath == "" {
		return nil, errors.New("shared app-server did not report its control socket")
	}

	dialer := websocket.Dialer{
		NetDialContext: func(dialCtx context.Context, _, _ string) (net.Conn, error) {
			var unixDialer net.Dialer
			return unixDialer.DialContext(dialCtx, "unix", daemon.SocketPath)
		},
		HandshakeTimeout: 10 * time.Second,
	}
	conn, _, err := dialer.DialContext(ctx, "ws://localhost/", nil)
	if err != nil {
		return nil, fmt.Errorf("connect shared app-server at %s: %w", daemon.SocketPath, err)
	}

	c := &Client{
		conn: conn, events: make(chan Event, 256),
		pending: make(map[string]chan rpcMessage), done: make(chan struct{}),
	}
	go c.readLoop()

	initCtx, cancel := context.WithTimeout(ctx, 10*time.Second)
	defer cancel()
	var initResult json.RawMessage
	err = c.Request(initCtx, "initialize", map[string]any{
		"clientInfo":   map[string]string{"name": "codex-agents", "version": clientVersion},
		"capabilities": map[string]any{"experimentalApi": true},
	}, &initResult)
	if err != nil {
		_ = c.Close()
		return nil, fmt.Errorf("initialize app-server: %w", err)
	}
	if err := c.Notify("initialized", map[string]any{}); err != nil {
		_ = c.Close()
		return nil, err
	}
	return c, nil
}

func (c *Client) Events() <-chan Event { return c.events }

func (c *Client) ListThreads(ctx context.Context) ([]Thread, error) {
	var response struct {
		Data []Thread `json:"data"`
	}
	err := c.Request(ctx, "thread/list", map[string]any{
		"limit": 100, "sortKey": "recency_at", "sortDirection": "desc", "useStateDbOnly": true,
	}, &response)
	return response.Data, err
}

func (c *Client) StartThread(ctx context.Context, cwd string) (Thread, error) {
	var response struct {
		Thread Thread `json:"thread"`
	}
	err := c.Request(ctx, "thread/start", map[string]any{"cwd": cwd}, &response)
	return response.Thread, err
}

func (c *Client) ResumeThread(ctx context.Context, id string) (Thread, error) {
	var response struct {
		Thread Thread `json:"thread"`
	}
	err := c.Request(ctx, "thread/resume", map[string]any{"threadId": id}, &response)
	return response.Thread, err
}

func (c *Client) ReadThread(ctx context.Context, id string) (Thread, error) {
	return c.readThread(ctx, id, true)
}

func (c *Client) readThread(ctx context.Context, id string, includeTurns bool) (Thread, error) {
	var response struct {
		Thread Thread `json:"thread"`
	}
	err := c.Request(ctx, "thread/read", map[string]any{"threadId": id, "includeTurns": includeTurns}, &response)
	return response.Thread, err
}

// ReadThreadHistory follows the same paginated history API used by the Codex
// TUI. Newer rollouts may only expose a bounded view through thread/read.
func (c *Client) ReadThreadHistory(ctx context.Context, id string) (Thread, error) {
	thread, err := c.readThread(ctx, id, false)
	if err != nil {
		return Thread{}, err
	}
	var turns []Turn
	var cursor string
	seen := make(map[string]bool)
	for {
		params := map[string]any{
			"threadId": id, "limit": 100, "sortDirection": "desc", "itemsView": "full",
		}
		if cursor != "" {
			params["cursor"] = cursor
		}
		var page struct {
			Data       []Turn  `json:"data"`
			NextCursor *string `json:"nextCursor"`
		}
		if pageErr := c.Request(ctx, "thread/turns/list", params, &page); pageErr != nil {
			// Older App Servers only support thread/read(includeTurns=true).
			return c.ReadThread(ctx, id)
		}
		pageTurns := make([]Turn, 0, len(page.Data))
		for i := len(page.Data) - 1; i >= 0; i-- {
			pageTurns = append(pageTurns, page.Data[i])
		}
		turns = append(pageTurns, turns...)
		if page.NextCursor == nil || *page.NextCursor == "" || seen[*page.NextCursor] {
			break
		}
		cursor = *page.NextCursor
		seen[cursor] = true
	}
	if len(turns) > 0 {
		thread.Turns = turns
	}
	return thread, nil
}

func (c *Client) StartTurn(ctx context.Context, threadID, text string) (Turn, error) {
	var response struct {
		Turn Turn `json:"turn"`
	}
	err := c.Request(ctx, "turn/start", map[string]any{
		"threadId": threadID,
		"input":    []map[string]any{{"type": "text", "text": text}},
	}, &response)
	return response.Turn, err
}

func (c *Client) SteerTurn(ctx context.Context, threadID, turnID, text string) (Turn, error) {
	var response struct {
		TurnID string `json:"turnId"`
	}
	err := c.Request(ctx, "turn/steer", map[string]any{
		"threadId":       threadID,
		"expectedTurnId": turnID,
		"input":          []map[string]any{{"type": "text", "text": text}},
	}, &response)
	return Turn{ID: response.TurnID, Status: "inProgress"}, err
}

func (c *Client) InterruptTurn(ctx context.Context, threadID, turnID string) error {
	var response json.RawMessage
	return c.Request(ctx, "turn/interrupt", map[string]any{"threadId": threadID, "turnId": turnID}, &response)
}

func (c *Client) SetThreadName(ctx context.Context, threadID, name string) error {
	var response json.RawMessage
	return c.Request(ctx, "thread/name/set", map[string]any{"threadId": threadID, "name": name}, &response)
}

func (c *Client) UnsubscribeThread(ctx context.Context, threadID string) (string, error) {
	var response struct {
		Status string `json:"status"`
	}
	err := c.Request(ctx, "thread/unsubscribe", map[string]any{"threadId": threadID}, &response)
	return response.Status, err
}

func (c *Client) Request(ctx context.Context, method string, params any, target any) error {
	id := c.nextID.Add(1)
	idKey := fmt.Sprintf("%d", id)
	response := make(chan rpcMessage, 1)
	c.mu.Lock()
	c.pending[idKey] = response
	c.mu.Unlock()
	defer func() {
		c.mu.Lock()
		delete(c.pending, idKey)
		c.mu.Unlock()
	}()

	if err := c.write(map[string]any{"id": id, "method": method, "params": params}); err != nil {
		return err
	}
	select {
	case msg := <-response:
		if msg.Error != nil {
			return msg.Error
		}
		if target == nil || len(msg.Result) == 0 {
			return nil
		}
		if err := json.Unmarshal(msg.Result, target); err != nil {
			return fmt.Errorf("decode %s response: %w", method, err)
		}
		return nil
	case <-ctx.Done():
		return ctx.Err()
	case <-c.done:
		return c.Err()
	}
}

func (c *Client) Notify(method string, params any) error {
	return c.write(map[string]any{"method": method, "params": params})
}

func (c *Client) Respond(id json.RawMessage, result any) error {
	var decoded any
	if err := json.Unmarshal(id, &decoded); err != nil {
		return err
	}
	return c.write(map[string]any{"id": decoded, "result": result})
}

func (c *Client) write(value any) error {
	data, err := json.Marshal(value)
	if err != nil {
		return err
	}
	c.writeMu.Lock()
	defer c.writeMu.Unlock()
	return c.conn.WriteMessage(websocket.TextMessage, data)
}

func (c *Client) readLoop() {
	defer close(c.done)
	defer close(c.events)
	for {
		messageType, payload, err := c.conn.ReadMessage()
		if err != nil {
			c.errMu.Lock()
			c.err = err
			c.errMu.Unlock()
			return
		}
		if messageType != websocket.TextMessage {
			continue
		}
		var msg rpcMessage
		if err := json.Unmarshal(payload, &msg); err != nil {
			continue
		}
		if len(msg.ID) > 0 && msg.Method == "" {
			key := string(msg.ID)
			c.mu.Lock()
			ch := c.pending[key]
			c.mu.Unlock()
			if ch != nil {
				ch <- msg
			}
			continue
		}
		if msg.Method != "" {
			select {
			case c.events <- Event{Method: msg.Method, Params: msg.Params, ID: msg.ID}:
			default: // Never let token deltas block protocol responses.
			}
		}
	}
}

func (c *Client) Err() error {
	c.errMu.Lock()
	defer c.errMu.Unlock()
	if c.err != nil {
		return c.err
	}
	return errors.New("app-server stopped")
}

func (c *Client) Close() error {
	return c.conn.Close()
}
