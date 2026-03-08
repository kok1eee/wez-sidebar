# wez-sidebar

A WezTerm sidebar / dock for monitoring [Claude Code](https://docs.anthropic.com/en/docs/claude-code) sessions in real-time.

[日本語](README_JA.md)

## Why WezTerm?

WezTerm has built-in pane splitting and session management — it replaces tmux entirely. wez-sidebar is designed to run **inside WezTerm as a pane**, reading session data via the WezTerm CLI (`wezterm cli list`, `wezterm cli get-text`, `wezterm cli activate-pane`).

This is an intentional scope decision: wez-sidebar is for WezTerm users who run multiple Claude Code sessions in split panes. If you use a different terminal, this tool is not for you — and that's OK.

## Features

- **Session monitoring** — Status (running / waiting input / stopped), uptime, git branch, real-time activity
- **Activity display** — Shows what each session is doing right now (`Edit src/config.rs`, `Bash cargo test`, etc.)
- **Dangerous command warning** — `rm -rf`, `git push --force`, etc. highlighted in red with ⚠ marker
- **User message display** — Last user prompt with elapsed time (`fix the bug (3m ago)`)
- **Task progress (dock)** — Claude's `TodoWrite` tasks shown in dock mode (✓ done, ● in progress, ○ pending)
- **Yolo mode detection** — Detects `--dangerously-skip-permissions` via process tree inspection
- **Usage limits** — Anthropic API usage (5-hour / weekly) with color-coded indicators
- **Two display modes** — Sidebar (right pane for MacBook) or Dock (bottom pane for external monitors)
- **Pane switching** — Jump to any session's WezTerm pane with Enter or number keys
- **Desktop notifications** — macOS notification on permission prompts (via `terminal-notifier`)
- **Zero polling** — All data flows through hooks → file watcher; no CPU wasted on polling

## Requirements

- [WezTerm](https://wezfurlong.org/wezterm/)
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code)
- Rust toolchain (for building from source)

## Install

### Binary (no Rust required)

```bash
# macOS (Apple Silicon)
curl -L https://github.com/kok1eee/wez-sidebar/releases/latest/download/wez-sidebar-aarch64-apple-darwin \
  -o ~/.local/bin/wez-sidebar && chmod +x ~/.local/bin/wez-sidebar

# macOS (Intel)
curl -L https://github.com/kok1eee/wez-sidebar/releases/latest/download/wez-sidebar-x86_64-apple-darwin \
  -o ~/.local/bin/wez-sidebar && chmod +x ~/.local/bin/wez-sidebar

# Linux (x86_64)
curl -L https://github.com/kok1eee/wez-sidebar/releases/latest/download/wez-sidebar-x86_64-linux \
  -o ~/.local/bin/wez-sidebar && chmod +x ~/.local/bin/wez-sidebar
```

### Cargo

```bash
cargo install wez-sidebar
```

### From source

```bash
git clone https://github.com/kok1eee/wez-sidebar.git
cd wez-sidebar
cargo install --path .
```

## Quick Start

Run the setup wizard:

```bash
wez-sidebar init
```

This will:
1. Register Claude Code hooks in `~/.claude/settings.json`
2. Show WezTerm keybinding examples

<details>
<summary>Manual setup</summary>

#### 1. Register hooks

Add to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      { "type": "command", "command": "~/.cargo/bin/wez-sidebar hook PreToolUse" }
    ],
    "PostToolUse": [
      { "type": "command", "command": "~/.cargo/bin/wez-sidebar hook PostToolUse" }
    ],
    "Notification": [
      { "type": "command", "command": "~/.cargo/bin/wez-sidebar hook Notification" }
    ],
    "Stop": [
      { "type": "command", "command": "~/.cargo/bin/wez-sidebar hook Stop" }
    ],
    "UserPromptSubmit": [
      { "type": "command", "command": "~/.cargo/bin/wez-sidebar hook UserPromptSubmit" }
    ]
  }
}
```

#### 2. WezTerm keybinding

```lua
-- Sidebar (MacBook)
{
  key = "b",
  mods = "LEADER",
  action = wezterm.action_callback(function(window, pane)
    pane:split({ direction = "Right", size = 0.2, args = { "wez-sidebar" } })
  end),
}

-- Dock (external monitor)
{
  key = "d",
  mods = "LEADER",
  action = wezterm.action_callback(function(window, pane)
    pane:split({ direction = "Bottom", size = 0.25, args = { "wez-sidebar", "dock" } })
  end),
}
```

</details>

That's it. No config file needed.

## Card Display

### Sidebar (compact, 3 content lines)

```
╭─ 🟢 my-project ⠋ ────╮
│ 2h30m  main           │
│ Edit src/config.rs     │
│ fix the bug (3m ago)   │
╰───────────────────────╯
```

### Dock (with task progress)

```
╭─ 🟢 my-project ⠋ ─────────────╮
│ 2h30m  main                    │
│ Edit src/hooks.rs              │
│ implement auth (5m ago)        │
│ ✓ Add types                    │
│ ● Edit hooks                   │
│ ○ Add tests                    │
╰────────────────────────────────╯
```

The same `render_session_card` function adapts to the available height — no mode branching needed.

## Session Markers

| Marker | Meaning |
|--------|---------|
| 🟢 | Current pane |
| 🔵 | Other pane |
| 🤖 | Yolo mode (`--dangerously-skip-permissions`) |
| ⚫ | Disconnected |

| Status | Meaning |
|--------|---------|
| ⠋ (spinner) | Running |
| ? | Waiting for input (permission prompt) |
| ■ | Stopped |

## Configuration

All settings are optional. Create `~/.config/wez-sidebar/config.toml` only if needed.

| Key | Default | Description |
|-----|---------|-------------|
| `wezterm_path` | auto-detect | Full path to WezTerm binary (recommended if PATH issues occur) |
| `stale_threshold_mins` | `30` | Minutes before a session is considered stale |
| `data_dir` | `~/.config/wez-sidebar` | Directory for `sessions.json` and `usage-cache.json` |

## Keybindings

| Key | Sidebar | Dock |
|-----|---------|------|
| `j`/`k` | Move up/down | Move up/down |
| `Enter` | Switch to pane | Switch to pane |
| `1`-`9` | Switch by number | Switch by number |
| `Tab`/`h`/`l` | - | Switch column |
| `p` | Toggle preview | - |
| `f` | Toggle stale sessions | Toggle stale sessions |
| `d` | Delete session | Delete session |
| `r` | Refresh all | Refresh all |
| `?` | Help | Help |
| `q`/`Esc` | Quit | Quit |

## Architecture

```
Claude Code ──hook──→ wez-sidebar hook <event>
                              │
                    ┌─────────┴───────────┐
                    │ session update       │
                    │ activity extraction  │
                    │ danger detection     │
                    │ user message capture │
                    │ TodoWrite tasks      │
                    │ git branch           │
                    │ yolo mode detection  │
                    └─────────┬───────────┘
                              │
                    sessions.json + usage-cache.json
                              │
                         file watcher
                              │
                    wez-sidebar TUI (zero polling)
```

All data flows through hooks. The TUI only reacts to file changes — no polling, no subprocesses.

## License

MIT
