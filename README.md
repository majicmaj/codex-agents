# codex-agents

A deliberately small session overview for Codex CLI.

It runs one Codex App Server for every session in the overview, so sessions keep
running when you move in and out of them. No tmux panes or transcript scraping
are involved.

## Requirements

- Codex CLI with `codex app-server`
- Go 1.25+ to build from source

## Run

```sh
go run .
```

Or build a single binary:

```sh
go build -o codex-agents .
./codex-agents
```

Verify connectivity without opening the TUI:

```sh
./codex-agents --doctor
```

New sessions use the directory where `codex-agents` was launched.
Sessions beneath `~/Projects/<name>` are grouped under `<name>`; nested working
directories remain part of the same project. Set `CODEX_AGENTS_PROJECTS_DIR` if
your projects live somewhere else.

Overview rows keep session names to a 34-character column, followed by the live
state (`Done`, `Needs Input`, `Working`, `Idle`, or `Failed`) and a one-line recap
derived incrementally from the latest session activity or message.

## Keys

Overview:

- Sessions are grouped by project by default
- `↑` / `↓` selects a session
- The mouse wheel moves only within the bounded session list; the header and
  composer stay pinned and terminal scrollback is never exposed
- `Enter` or `→` opens it
- `g` toggles between project and status grouping
- Type a prompt and press `Enter` to start a new session
- Type `/` for the command palette; `Tab` completes and `↑` / `↓` selects
- `?` opens the complete shortcut reference
- `Shift+←` / `Shift+→` selects input text; `Backspace` deletes it
- `Alt+Backspace` (Option+Backspace on macOS) deletes the previous word
- `Ctrl+C` clears a draft; press it twice on an empty input to exit
- `Ctrl+R` refreshes session history
- New Codex sessions started after this view opens are discovered automatically
- `Esc` exits

Session:

- `Enter` sends the prompt
- `Shift+Enter` or `Alt+Enter` inserts a newline
- `↑` / `↓`, `PageUp` / `PageDown`, or the mouse wheel scrolls conversation
  history while the composer is empty; `Home` / `End` jumps to top/bottom
- Native mouse drag selects visible transcript text; composer background padding
  is painted without adding trailing spaces to copied text
- `←` returns to the overview; `Esc` interrupts a working turn and otherwise returns
- `Ctrl+C` clears a draft, or interrupts the active turn when input is empty
- Press `Ctrl+X` twice within three seconds to close/unsubscribe this view from
  the session. The first press temporarily replaces the session title with a
  red confirmation; persisted history remains available for a later resume
- `/rename <name>` updates the persisted session name without starting a turn

The conversation follows Codex's visual hierarchy: user messages use the
subtle composer background, assistant text is unlabelled normal transcript
text, and the user prompt associated with the visible turn stays pinned beneath
the session header as history scrolls. Live commands and tools appear step by
step, `Working (… • esc to interrupt)` stays above the composer, and completed
work is separated from the final answer by Codex's timed `Worked for …` rule.

The composer follows Codex's default editing conventions: `Ctrl+B/F` moves by
character, `Alt+B/F` by word, `Ctrl+A/E` to the line edges, `Ctrl+U/K` kills to
the start/end, `Ctrl+Y` yanks, and `Ctrl+P/N` recalls history or navigates a
completion popup. Long input wraps inside a padded, bottom-pinned composer.

## Codex interaction compatibility

The palette uses the upstream Codex command names and presentation order. The
commands this overview can execute through its current App Server ownership are:
`/new`, `/resume`, `/rename`, `/status`, `/stop`, `/clear`, `/help`, `/quit`, and `/exit`.
Other Codex commands remain discoverable and are labelled **native Codex**;
selecting one explains that it is not exposed here instead of sending it as an
ordinary model prompt.

URLs are underlined, clickable terminal hyperlinks in assistant messages.
Existing local paths are highlighted and clickable; URLs and path-like tokens
are highlighted while composing. Session activity includes commands, bounded
command output, MCP/dynamic tools, and file-change steps from App Server events.
Cross-process working, approval, and input states are inferred incrementally
from Codex rollout task events because a separate App Server reports those
threads as `notLoaded`.

## MVP limitations

- Approval and structured-input requests are shown as **Needs input**, but must
  currently be answered in the native Codex client.
- Closing `codex-agents` stops turns owned by its App Server. A durable detached
  supervisor is intentionally deferred until after the interaction loop is
  proven.
- Unread/Ready state is kept only for the current UI process.
- Full diffs, native slash-command panels, image attachments, `@` file search,
  `$` mentions, shell `!` mode, queued prompts, Vim mode, and configurable
  keymaps remain available in native Codex.

The protocol integration follows the official OpenAI App Server documentation:
https://developers.openai.com/codex/app-server
