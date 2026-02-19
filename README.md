# wez-sidebar

WezTerm sidebar / dock for monitoring [Claude Code](https://docs.anthropic.com/en/docs/claude-code) sessions, usage limits, and tasks.

| Sidebar mode | Dock mode | WezTerm overlay |
|:---:|:---:|:---:|
| ![Sidebar](docs/images/sidebar-mode.png) | ![Dock](docs/images/dock-mode.png) | ![Overlay](docs/images/wezterm-overlay.png) |

## Features

- **Session monitoring** - Track active Claude Code sessions with status (running / waiting input / stopped), uptime, and task progress
- **Usage limits** - Real-time display of Anthropic API usage (5-hour and weekly limits) with color-coded indicators
- **Task panel** - Optional display of tasks from an external JSON cache file (e.g. Asana)
- **Built-in hook handler** - Manages `sessions.json` autonomously; no external dependencies required
- **Two display modes** - Vertical sidebar or horizontal dock (bottom bar with 3-column layout)
- **Pane integration** - Switch to any session's WezTerm pane with Enter or number keys

## Requirements

- [WezTerm](https://wezfurlong.org/wezterm/)
- [Claude Code](https://docs.anthropic.com/en/docs/claude-code)
- Rust toolchain (for building)

## Installation

```bash
cargo install --path .
```

## Setup

### 1. Register hooks

Add the following to `~/.claude/settings.json`:

```json
{
  "hooks": {
    "PreToolUse": [
      { "type": "command", "command": "wez-sidebar hook PreToolUse" }
    ],
    "PostToolUse": [
      { "type": "command", "command": "wez-sidebar hook PostToolUse" }
    ],
    "Notification": [
      { "type": "command", "command": "wez-sidebar hook Notification" }
    ],
    "Stop": [
      { "type": "command", "command": "wez-sidebar hook Stop" }
    ],
    "UserPromptSubmit": [
      { "type": "command", "command": "wez-sidebar hook UserPromptSubmit" }
    ]
  }
}
```

### 2. Configure WezTerm

Add a sidebar or dock pane that runs `wez-sidebar` (or `wez-sidebar dock`).

Example WezTerm keybinding to toggle a right sidebar:

```lua
{
  key = "b",
  mods = "LEADER",
  action = wezterm.action_callback(function(window, pane)
    local tab = window:active_tab()
    -- Toggle logic: split right with wez-sidebar
    tab:active_pane():split({ direction = "Right", args = { "wez-sidebar" } })
  end),
}
```

### 3. (Optional) Create config file

```bash
cp config.example.toml ~/.config/wez-sidebar/config.toml
```

See [`config.example.toml`](config.example.toml) for all available options.

## Configuration

| Key | Default | Description |
|-----|---------|-------------|
| `wezterm_path` | auto-detect | Path to WezTerm binary |
| `stale_threshold_mins` | `30` | Minutes before a session is considered stale |
| `data_dir` | `~/.config/wez-sidebar` | Directory for `sessions.json` |
| `hook_command` | *(built-in)* | External command to delegate hook handling |
| `tasks_file` | *(none)* | Path to tasks cache JSON file |
| `task_filter_name` | *(none)* | Only show tasks where assignee contains this name |

## Usage

### Sidebar mode (default)

```bash
wez-sidebar
```

### Dock mode (horizontal bottom bar)

```bash
wez-sidebar dock
```

### Keybindings

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

## License

MIT
