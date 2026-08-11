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

### Native Codex sessions

Agents View owns only the lightweight project/session overview. Opening a
session—or typing a new prompt in the overview—suspends that view and gives the
terminal to the installed, unmodified Codex TUI. This preserves Codex rendering,
syntax highlighting, slash commands, approvals, input behavior, and future CLI
updates without maintaining a second transcript renderer.

The native TUI connects to the durable local App Server with `--remote
unix://`. `/quit` returns to Agents View and refreshes its session list. Other
Codex clients can join the same daemon explicitly:

```sh
codex --remote unix://
codex resume --remote unix:// <session-id-or-name>
```

Sessions already open in a separately launched Codex process have an active
writer and cannot be taken over. Close that TUI first, then open the session
from Agents View. Sessions launched from Agents View already use the shared
daemon and resume normally.

For rollback and comparison, `codex-agents --legacy-sessions` restores the old
built-in Go session renderer. `CODEX_AGENTS_NATIVE_SESSIONS=0` does the same.

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

Native session:

- Codex owns its complete keymap, composer, scrolling, selection, rendering,
  slash-command palette, approvals, diffs, tools, and attachments
- Plain `←` immediately exits the native session and returns to Agents View
- `Shift+←`, `Ctrl+←`, and `Alt+←` remain available for native text selection
  and word movement; use `Ctrl+B` when character-left editing is needed
- `/quit` also exits the native session and returns to Agents View
- The overview refreshes after Codex exits and keeps the same session selected

## Codex interaction compatibility

The session is the installed Codex CLI itself, so interaction compatibility
tracks that installed version rather than a reimplementation in this project.
The overview continues to infer cross-process `Working`, `Needs Input`, `Done`,
and related states from App Server and rollout events.

## MVP limitations

- Unread/Ready state is kept only for the current UI process.
- Plain left-arrow is reserved by the Agents View PTY bridge while native Codex
  owns the terminal. This intentionally trades Codex's character-left shortcut
  for one-key navigation; `Ctrl+B` retains character-left editing.

The protocol integration follows the official OpenAI App Server documentation:
https://developers.openai.com/codex/app-server
