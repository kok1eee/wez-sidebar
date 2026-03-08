# wez-sidebar

WezTerm sidebar / dock for monitoring [Claude Code](https://docs.anthropic.com/en/docs/claude-code) sessions, usage limits, and tasks.

[日本語](README_JA.md)

| Sidebar (MacBook) | Dock (external monitor) |
|:---:|:---:|
| ![Sidebar](docs/images/sidebar-with-panes.png) | ![Dock](docs/images/dock-mode.png) |

| Mode select | Overlay |
|:---:|:---:|
| ![Select](docs/images/mode-select.png) | ![Overlay](docs/images/wezterm-overlay.png) |

## Features

- **Session monitoring** - Track active Claude Code sessions with status (running / waiting input / stopped), uptime, and task progress
- **Yolo mode detection** - Automatically detects `--dangerously-skip-permissions` sessions via process tree inspection
- **Usage limits** - Anthropic API usage (5-hour and weekly limits) with color-coded indicators, updated via hook with 10-min cooldown
- **Task management** - Built-in CLI (`wez-sidebar task add/done/list`) and optional JSON file for external integrations
- **Built-in hook handler** - Manages `sessions.json` autonomously; no external dependencies required
- **Two display modes** - Sidebar (right bar for MacBook) or Dock (bottom bar for external monitors)
- **Pane integration** - Switch to any session's WezTerm pane with Enter or number keys
- **Desktop notifications** - macOS notification on permission prompts (via `terminal-notifier`)

## Requirements

- [WezTerm](https://wezfurlong.org/wezterm/)
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code)
- Rust toolchain (for building)

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
2. Guide you through task management setup (optional)
3. Show WezTerm keybinding examples

### Manual setup

<details>
<summary>If you prefer manual configuration</summary>

#### 1. Register hooks

Add the following to `~/.claude/settings.json`:

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

#### 2. Configure WezTerm

Add a keybinding to open the sidebar:

```lua
{
  key = "b",
  mods = "LEADER",
  action = wezterm.action_callback(function(window, pane)
    local tab = window:active_tab()
    tab:active_pane():split({ direction = "Right", args = { "wez-sidebar" } })
  end),
}
```

</details>

That's it. No config file needed — it works out of the box.

## Session Markers

| Marker | Meaning |
|--------|---------|
| 🟢 | Current pane |
| 🔵 | Other pane |
| 🤖 | Yolo mode (`--dangerously-skip-permissions`) |
| ⚫ | Disconnected |

| Status | Meaning |
|--------|---------|
| ▶ | Running |
| ? | Waiting for input (permission prompt) |
| ■ | Stopped |

## Task Management (Optional)

wez-sidebar includes a built-in task CLI. Tasks are stored in `~/.config/wez-sidebar/tasks.json`.

```bash
# Add a task
wez-sidebar task add "Implement auth" -p 1 -d 2026-03-10

# List tasks
wez-sidebar task list

# Complete a task
wez-sidebar task done <id>
```

To show tasks in the TUI panel, set `tasks_file` in your config:

```toml
# ~/.config/wez-sidebar/config.toml
tasks_file = "~/.config/wez-sidebar/tasks.json"
```

The TUI watches the file for changes and updates in real-time.

You can also write the same JSON format from external tools (Asana sync scripts, GitHub Actions, etc.):

```json
{
  "tasks": [
    { "id": "1", "title": "Task name", "status": "pending", "priority": 1, "due_on": "2026-03-10" }
  ]
}
```

## Configuration

All settings are optional. Create `~/.config/wez-sidebar/config.toml` only if you need to customize.

| Key | Default | Description |
|-----|---------|-------------|
| `wezterm_path` | auto-detect | Full path to WezTerm binary |
| `stale_threshold_mins` | `30` | Minutes before a session is considered stale |
| `data_dir` | `~/.config/wez-sidebar` | Directory for `sessions.json` and `usage-cache.json` |
| `tasks_file` | *(none)* | Path to tasks JSON file (enables TUI task panel) |
| `hook_command` | *(built-in)* | External command to delegate hook handling |
| `api_url` | *(none)* | REST API base URL for remote task fetching |

See [`config.example.toml`](config.example.toml) for details.

### Hook Delegation

By default, `wez-sidebar hook` handles everything internally. To delegate to an external tool (while keeping built-in session management):

```toml
hook_command = "my-custom-tool hook"
```

The external command receives the hook payload on stdin after wez-sidebar updates `sessions.json`.

## Display Modes

### Sidebar (default)

```bash
wez-sidebar
```

### Dock (horizontal bottom bar)

```bash
wez-sidebar dock
```

## Keybindings

| Key | Sidebar | Dock |
|-----|---------|------|
| `j`/`k` | Move up/down | Move up/down |
| `Enter` | Switch to pane | Switch to pane |
| `t` | Tasks mode | - |
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
                        sessions.json (session state)
                        usage-cache.json (API usage, 10-min cooldown)
                              │
                        file watcher
                              │
                        wez-sidebar TUI
```

## License

MIT
