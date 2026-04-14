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
- **Subagent tracking** — Active subagent count displayed on parent session card
- **Disconnected sessions** — Sessions whose WezTerm pane was closed shown with ⚫ marker (24h retention)
- **Yolo mode detection** — Detects `--dangerously-skip-permissions` via process tree inspection
- **Usage limits** — Anthropic API usage (5-hour / weekly) with color-coded indicators
- **Two display modes** — Sidebar (right pane for MacBook) or Dock (bottom pane for external monitors)
- **Pane switching** — Jump to any session's WezTerm pane with Enter or number keys
- **Desktop notifications** — macOS notification on permission prompts (via `terminal-notifier`)
- **Orphan reaper** — Automatically detects and kills orphaned Claude Code processes not attached to any WezTerm pane (opt-in)
- **Zero polling** — All data flows through hooks → file watcher; no CPU wasted on polling
- **Spawn new sessions** — `wez-sidebar new <dir>` opens a new tab with Claude Code running in the given directory
- **Kanban / task management** — Stack tasks in the backlog, link dependencies, see progress across `Active / Review / Done` columns. Approving a task auto-spawns the next dependent task
- **Block-alert notifications** — Desktop notification when a task sits in `review` too long (anti-neglect)

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

## Spawning New Sessions

`wez-sidebar new` opens a new WezTerm tab (or window) and starts `claude` in the given directory. Works with both WezTerm and tmux backends.

```bash
# Open a new tab in the current directory with claude
wez-sidebar new

# Open a new tab in the specified directory
wez-sidebar new ~/Documents/personal-dev/wez-sidebar

# Open in a new window instead of a tab
wez-sidebar new -w ~/Documents/personal-dev/wez-sidebar

# Pass an initial prompt to claude (everything after `--` is forwarded)
wez-sidebar new ~/path/to/repo -- "Fix X in src/foo.rs"

# Pass claude options through
wez-sidebar new ~/path -- -r

# Spawn with a task (sets claude -n "<title>" and sends an initial prompt)
wez-sidebar new --task "Design DB schema" --prompt "SQLite + Prisma..."
```

The tab title is automatically set to the directory basename. When `--task` is supplied, a kanban task is also created and shown in the `Active` column.

## Kanban / Task Management

For running multiple Claude Code sessions in parallel with visible dependency / status tracking. Inspired by Cline Kanban.

### Workflow

```
[backlog] ── spawn ──▶ [running] ◀─── UserPromptSubmit ───┐
                          │ Stop hook                      │
                          ▼                                │
                       [review] ───────────────────────────┘
                          │ a (approve) / auto_approve
                          ▼
                       [done]  ── auto-spawns dependent next task
```

### Task CLI

```bash
# Add a task to the backlog
wez-sidebar tasks add "Design DB schema" --cwd ~/repo --prompt "..."

# Link dependency: B can start only after A is done
wez-sidebar tasks link <A_id> <B_id>

# List (table or JSON)
wez-sidebar tasks list
wez-sidebar tasks list --status review --format json

# backlog → running (spawn when dependencies clear)
wez-sidebar tasks start <id>

# review → done (and auto-spawn dependents)
wez-sidebar tasks approve <id>

# review → running (send back for more input)
wez-sidebar tasks reject <id>

# any → trash / trash → backlog
wez-sidebar tasks trash <id>
wez-sidebar tasks restore <id>

# Resume a done task via `claude --resume "<title>"`
wez-sidebar tasks resume <id>
```

### TUI Operations (kanban mode)

| Key | Action |
|-----|--------|
| `v` | Toggle view (auto ↔ kanban ↔ flat) |
| `a` | Approve selected task (review → done) |
| `R` | Reject selected task (review → running) |
| `T` | Trash selected task |
| `Tab`/`h`/`l` | Move between columns (Active / Review / Done) |
| `Space` | Toggle section collapse (sidebar) |

### Block-alert Notifications

When a task sits in `review` for more than `block_alert_minutes` (default 5), a macOS notification fires via `terminal-notifier` (sound: Basso). Click to jump to the pane.

### Persistence

Tasks live in `~/.config/wez-sidebar/tasks.json`. The task title equals the Claude Code session name (`claude -n "<title>"`), so renames via `/rename` stay tracked.

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

### Orphan Reaper

Disabled by default. Add to `config.toml`:

```toml
[reaper]
enabled = true
threshold_hours = 3  # Kill orphans older than this
```

When enabled, the TUI checks every 5 minutes for Claude Code processes not attached to any WezTerm pane. You can also run it manually:

```bash
wez-sidebar reap --dry  # Preview orphans without killing
wez-sidebar reap        # Kill orphaned processes (SIGTERM)
```

### Kanban / Notifications

```toml
[kanban]
auto_flat_threshold = 3         # Stay flat when session count is below this
block_alert_minutes = 5         # Threshold for review-staleness alerts (minutes). 0 to disable.
auto_approve = false            # true = skip review; Stop hook moves task straight to done
block_alert_sound = "Basso"     # terminal-notifier -sound value
block_alert_cooldown_secs = 0   # 0 = once per review stint, >0 = re-alert every N seconds
```

## Keybindings

| Key | Sidebar | Dock |
|-----|---------|------|
| `j`/`k` | Move up/down | Move up/down |
| `Enter` | Switch to pane | Switch to pane |
| `1`-`9` | Switch by number | Switch by number |
| `Tab`/`h`/`l` | Switch column (kanban) | Switch column |
| `Space` | Toggle section (kanban) | - |
| `v` | Toggle view (auto/kanban/flat) | Toggle view |
| `a` | Approve task (review → done) | Approve task |
| `R` | Reject task (review → running) | Reject task |
| `T` | Trash task | Trash task |
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
                    │ subagent tracking    │
                    │ git branch           │
                    │ yolo mode detection  │
                    └─────────┬───────────┘
                              │
                    sessions.json + usage-cache.json
                              │
                         file watcher
                              │
                    wez-sidebar TUI (zero polling)
                              │
                    reaper (opt-in, every 5 min)
                    └→ ps + wezterm cli list → kill orphans
```

All data flows through hooks. The TUI only reacts to file changes — no polling, no subprocesses.
The reaper periodically compares running `claude` processes against WezTerm panes to detect orphans.

## License

MIT
