# Codex CLI interaction parity

Audited against OpenAI Codex commit
`1dac3d9ca04a347632056f752b15ddfa4d7cd757` and local `codex-cli 0.147.0`.
The upstream sources of truth are `codex-rs/tui/src/keymap.rs`,
`bottom_pane/chat_composer.rs`, `slash_command.rs`, `markdown_render.rs`, and
`terminal_hyperlinks.rs`.

The agent view is an App Server client, not an embedded Codex TUI. Code that is
pure editor behavior is ported. Features owned by Codex's Rust `ChatWidget` are
only enabled when the App Server exposes an equivalent operation.

| Interaction | Agent view | Decision |
|---|---:|---|
| Enter submit/open | Yes | Same context-sensitive behavior |
| Shift/Alt+Enter, Ctrl+J/M newline | Yes | Bubble Tea v2 keyboard disambiguation plus Codex-compatible terminal mode negotiation preserves modified Enter |
| Arrow and Ctrl+B/F character movement | Yes | Ported editor behavior |
| Alt+Arrow and Alt+B/F word movement | Yes | Ported editor behavior |
| Home/End and Ctrl+A/E | Yes | Ported editor behavior |
| Backspace/Delete and Ctrl+H/D | Partial | Backspace/Delete supported; terminals do not all distinguish every modified variant through Bubble Tea |
| Alt/Ctrl+Backspace, Ctrl+W | Yes | Delete previous word |
| Alt/Ctrl+Delete, Alt+D | Yes | Delete next word |
| Ctrl+U/K/Y | Yes | Kill to start/end and yank |
| Shift+Arrow selection | Yes | Selection uses Codex-style reverse emphasis |
| Up/Down and Ctrl+P/N | Yes | Popup navigation and local prompt history |
| Transcript scrolling | Yes | Empty-composer arrows, PageUp/PageDown, Home/End, and mouse wheel; active user prompt is sticky |
| Native transcript selection | Yes | Terminal-owned drag selection works across visible virtualized rows; composer padding is non-textual |
| `?` shortcut overlay | Yes | Agent-view-specific actions plus ported editor keys |
| Ctrl+C | Yes | Clear a draft; otherwise interrupt; double empty Ctrl+C exits overview |
| Ctrl+D | Yes | Exit when the draft is empty; forward-delete otherwise |
| `/` command autocomplete | Yes | Upstream names/order; Tab completes; executable scope is labelled |
| `/new`, `/resume`, `/status`, `/stop`, `/clear`, `/help`, `/quit`, `/exit` | Yes | Implemented locally or through App Server |
| Other native slash commands | Discoverable | App Server does not expose the TUI panels/dispatchers; guarded rather than sent as prompts |
| URL highlighting and OSC 8 links | Yes | Highlighted in composer; clickable in assistant transcript |
| Existing path highlighting and OSC 8 links | Yes | Relative to the session working directory |
| Bottom-pinned padded multiline composer | Yes | Long strings and explicit newlines wrap without terminal overflow |
| Ctrl+R/S reverse history search | No | Ctrl+R refreshes the overview; a search UI needs separate state |
| Tab queue while a turn runs | No | App Server ownership supports `turn/start`, but this MVP has no durable queued-turn supervisor |
| Esc interrupt/backtrack | Yes | Esc interrupts a live turn; Left returns to the overview; Ctrl+C also interrupts the exact active `(threadId, turnId)` |
| Ctrl+T transcript overlay | No | The session view is already the transcript |
| Ctrl+G external editor | No | Would create a subprocess/editor lifecycle outside the thin view |
| Ctrl+L clear terminal | No | Alternate-screen redraw makes terminal clearing redundant; `/clear` clears rendered messages |
| Shift+Tab collaboration mode | No | Requires native Codex configuration and mode UI |
| Alt+,/. reasoning effort | No | Requires model/config mutation APIs and feedback UI |
| `@` file search | Not yet | Feasible with a bounded, cached project file index |
| `$` skills/apps/plugins mentions | No | Requires native mention bindings and attachment payloads |
| `!` shell mode | No | Native Codex owns sandboxing, approval, output streaming, and command history; executing locally here would bypass those controls |
| Image paste/attachments | No | Requires App Server attachment handling and terminal clipboard/image support |
| Vim mode and `/keymap` | No | Upstream keymap engine is Rust-internal and not a reusable library |
| Markdown and fenced-code syntax highlighting | Partial | Codex-style Markdown, code, URLs, and paths are styled; full language grammars and diff cells remain native-only |
| Command/tool activity | Yes | Live started/completed command, MCP, dynamic-tool, file-change, and bounded output rows; timed final-answer separator |
| Working indicator | Yes | Bottom-pinned elapsed status with Esc interrupt hint |
| Cross-process session status | Yes | Cached rollout-tail task state overlays App Server's `notLoaded` status |
| ANSI command-output rendering | Partial | Output is bounded and safely rendered; arbitrary command ANSI is not replayed |
| Approval/input widgets | Status only | The overview marks **Needs input**; responses still require native Codex |

## Next high-value parity slice

The cleanest next increment is a cached `@` file picker and native approval/input
widgets. Shell `!` mode should continue to be delegated to native Codex unless
App Server offers the same sandbox and approval semantics.

The keyboard layer uses Bubble Tea v2 progressive keyboard enhancements. This
is important: legacy terminal input encodes Enter, Ctrl+M, and often
Shift+Enter identically, so a handler alone cannot provide Codex parity. The
enhanced protocol preserves modifiers on supported terminals and retains
Alt+Enter as the portable newline fallback.
