# codex-agents

A deliberately small session overview for Codex CLI.

It connects to Codex's durable local App Server daemon, so every session has one
writer-owning server while Agents View and native Codex can both subscribe and
write. Sessions keep running when you move in and out of them or close this UI.
No tmux panes or transcript scraping are involved.

## Install

This repository is currently private, so authenticate GitHub CLI once and run
the installer directly from the repository:

```sh
gh auth login
gh api repos/majicmaj/codex-agents/contents/install.sh \
  -H "Accept: application/vnd.github.raw+json" | sh
```

That installs the correct macOS or Linux binary in `~/.local/bin`. If needed,
add it to your shell path:

```sh
export PATH="$HOME/.local/bin:$PATH"
```

Then launch it from any project:

```sh
codex-agents
```

Already cloned the repository? Run `./install.sh`. To use another destination,
set `CODEX_AGENTS_INSTALL_DIR`, for example:

```sh
CODEX_AGENTS_INSTALL_DIR="$HOME/bin" ./install.sh
```

### Updates

Installed release builds check GitHub once per day when they start. When a new
release exists, `codex-agents` downloads the exact OS/architecture binary,
verifies its SHA-256 checksum and GitHub asset digest, atomically replaces
itself, and continues with the new version.

Update immediately or disable automatic checks with:

```sh
codex-agents --update
codex-agents --no-update
CODEX_AGENTS_NO_UPDATE=1 codex-agents
```

Private-repository updates use the token from `GH_TOKEN`, `GITHUB_TOKEN`, or
the existing `gh auth login` session. If the repository becomes public, the
same installer and updater work without authentication.

## Requirements

- Codex CLI with `codex app-server`
- GitHub CLI authenticated with repository access while the repo is private
- macOS or Linux on Apple Silicon/ARM64 or Intel/AMD64

## Build from source

Source builds require Go 1.25+:

```sh
go run .
```

Or build a single binary:

```sh
go build -o codex-agents .
./codex-agents
```

Maintainers publish a version by pushing a semantic-version tag:

```sh
git tag v0.14.0
git push origin main v0.14.0
```

The release workflow runs tests, builds all supported platforms, generates
`SHA256SUMS`, and publishes the GitHub release. The binary version is derived
from the tag, so there is no separate version file to keep synchronized.

Verify connectivity without opening the TUI:

```sh
codex-agents --doctor
```

To use a native Codex TUI and Agents View on the same live session, launch the
native client through the same local server:

```sh
codex --remote unix://
codex resume --remote unix:// <session-id-or-name>
```

Both clients then receive live events and can send input. Input sent while a
turn is already working uses Codex's `turn/steer` protocol.

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
- `↑` / `↓` moves through multiline input, then cycles through past prompts at
  the top/bottom boundary. The newest slot restores the unsent draft exactly,
  including edits made while cycling
- `PageUp` / `PageDown` or the mouse wheel scrolls conversation history;
  `Home` / `End` jumps to top/bottom. Wheel events are captured and clamped, so
  they cannot escape into shell scrollback
- Drag selects and copies either composer input or visible transcript text
  without background padding; OSC 52 plus native clipboard fallback keeps this
  reliable across terminals, and selection remains virtualized while scrolling
- `←` returns to the overview; `Esc` interrupts a working turn and otherwise returns
- `Ctrl+C` clears a draft, or interrupts the active turn when input is empty
- Press `Ctrl+X` twice within three seconds to close/unsubscribe this view from
  the session. The first press temporarily replaces the session title with a
  red confirmation; persisted history remains available for a later resume
- `/rename <name>` updates the persisted session name without starting a turn

Opening a stored session attempts `thread/resume` immediately on the shared
daemon. A legacy Codex process launched without `--remote unix://` still owns a
separate writer that cannot be safely taken over in place. Agents View shows the
exact remote resume command for that exceptional case and keeps the draft intact.

The conversation follows Codex's visual hierarchy: user messages use the
subtle composer background, assistant text is unlabelled normal transcript
text, and the user prompt associated with the visible turn stays pinned beneath
the session header as history scrolls. The sticky prompt and bottom composer are
both framed by full-width rules. Live commands and tools appear step by step,
`Working (… • esc to interrupt)` stays above the composer, and Codex's timed
`Worked for …` rule begins the completed work block.

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
- Unread/Ready state is kept only for the current UI process.
- Full diffs, native slash-command panels, image attachments, `@` file search,
  `$` mentions, shell `!` mode, queued prompts, Vim mode, and configurable
  keymaps remain available in native Codex.

The protocol integration follows the official OpenAI App Server documentation:
https://developers.openai.com/codex/app-server
