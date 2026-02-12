use anyhow::Result;
use chrono::{DateTime, Datelike, Local, Timelike, Utc};
use clap::{Parser, Subcommand};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, ListState, Paragraph},
    Frame, Terminal,
};
use serde::{Deserialize, Serialize};
use std::{
    collections::HashMap,
    fs,
    io,
    path::PathBuf,
    process::Command,
    sync::mpsc,
    thread,
    time::Duration,
};

// ============================================================================
// Asana Tasks Cache
// ============================================================================

#[derive(Debug, Deserialize)]
struct AsanaTasksCache {
    tasks: Vec<AsanaCachedTask>,
}

#[derive(Debug, Deserialize)]
struct AsanaCachedTask {
    gid: String,
    name: String,
    assignee: String,
    completed: bool,
    #[serde(default = "default_priority")]
    priority: i32,
    #[serde(default)]
    due_on: Option<String>,
}

fn default_priority() -> i32 {
    3
}

fn get_asana_cache_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".config/ambient-task-agent")
}

fn get_asana_cache_path() -> PathBuf {
    get_asana_cache_dir().join("tasks-cache.json")
}

fn load_asana_tasks() -> Vec<GlobalTask> {
    let path = get_asana_cache_path();
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let cache: AsanaTasksCache = match serde_json::from_str(&content) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let user_name = std::env::var("ASANA_USER_NAME").unwrap_or_default();

    // キャッシュは優先度順にソート済み。フィルタのみ行う。
    cache
        .tasks
        .into_iter()
        .filter(|t| {
            if t.completed {
                return false;
            }
            if t.assignee.contains("田澤") {
                return true;
            }
            if !user_name.is_empty() && t.assignee.contains(&user_name) {
                return true;
            }
            false
        })
        .map(|t| GlobalTask {
            id: t.gid,
            title: t.name,
            status: "pending".to_string(),
            priority: t.priority,
            due_on: t.due_on,
            created_at: String::new(),
            updated_at: String::new(),
        })
        .collect()
}

// ============================================================================
// CLI
// ============================================================================

#[derive(Parser)]
#[command(name = "wez-sidebar")]
#[command(about = "WezTerm sidebar with Claude Code monitoring and task management")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Handle Claude Code hook event
    Hook {
        /// Event name (PreToolUse, PostToolUse, Notification, Stop, UserPromptSubmit)
        event: String,
    },
}

// ============================================================================
// Data Structures
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
struct SessionsFile {
    sessions: HashMap<String, Session>,
    updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Session {
    session_id: String,
    home_cwd: String,
    tty: String,
    status: String,
    created_at: String,
    updated_at: String,
    // Claude Code task progress
    #[serde(default)]
    active_task: Option<String>,
    #[serde(default)]
    tasks_completed: i32,
    #[serde(default)]
    tasks_total: i32,
}

#[derive(Debug, Clone, Deserialize)]
struct WezTermPane {
    window_id: i32,
    tab_id: i32,
    pane_id: i32,
    tty_name: String,
    #[allow(dead_code)]
    title: String,
    is_active: bool,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UsageResponse {
    five_hour: UsageData,
    seven_day: UsageData,
    seven_day_sonnet: Option<UsageData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct UsageData {
    utilization: f64,
    #[serde(default)]
    resets_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct KeychainCreds {
    #[serde(rename = "claudeAiOauth")]
    claude_ai_oauth: OAuthData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct OAuthData {
    #[serde(rename = "accessToken")]
    access_token: String,
}

#[derive(Debug, Clone)]
struct SessionItem {
    tab_id: i32,
    pane_id: i32,
    name: String,
    status: String,
    is_current: bool,
    created_at: DateTime<Utc>,
    updated_at: DateTime<Utc>,
    is_stale: bool,
    session_id: String,
    is_disconnected: bool,
    // Task progress
    active_task: Option<String>,
    tasks_completed: i32,
    tasks_total: i32,
}

#[derive(Debug, Clone, Default)]
struct UsageLimits {
    five_hour: i32,
    five_hour_reset: String,
    weekly: i32,
    weekly_reset: String,
    sonnet: i32,
}

// Global Tasks
#[derive(Debug, Clone, Serialize, Deserialize)]
struct GlobalTask {
    id: String,
    title: String,
    status: String, // pending, in_progress, completed
    priority: i32,  // 1=high, 2=medium, 3=low
    due_on: Option<String>,
    created_at: String,
    updated_at: String,
}


// ============================================================================
// App State
// ============================================================================

#[derive(Debug, Clone, Copy, PartialEq)]
enum FocusMode {
    Sessions,
    Tasks,
}

struct App {
    sessions: Vec<SessionItem>,
    session_state: ListState,
    global_tasks: Vec<GlobalTask>,
    task_state: ListState,
    usage: UsageLimits,
    show_stale: bool,
    focus_mode: FocusMode,
    should_quit: bool,
    show_help: bool,
    show_preview: bool,
    pane_preview: Vec<String>,
    preview_scroll: u16,
}

impl App {
    fn new() -> Self {
        let mut session_state = ListState::default();
        session_state.select(Some(0));
        let mut task_state = ListState::default();
        task_state.select(Some(0));

        Self {
            sessions: Vec::new(),
            session_state,
            global_tasks: Vec::new(),
            task_state,
            usage: UsageLimits {
                five_hour: -1,
                weekly: -1,
                sonnet: -1,
                ..Default::default()
            },
            show_stale: false,
            focus_mode: FocusMode::Sessions,
            should_quit: false,
            show_help: false,
            show_preview: false,
            pane_preview: Vec::new(),
            preview_scroll: 0,
        }
    }

    fn visible_sessions(&self) -> Vec<&SessionItem> {
        if self.show_stale {
            self.sessions.iter().collect()
        } else {
            self.sessions
                .iter()
                .filter(|s| s.is_disconnected || !s.is_stale)
                .collect()
        }
    }

    fn next_session(&mut self) {
        let visible = self.visible_sessions();
        if visible.is_empty() {
            return;
        }
        let i = match self.session_state.selected() {
            Some(i) => (i + 1) % visible.len(),
            None => 0,
        };
        self.session_state.select(Some(i));
    }

    fn previous_session(&mut self) {
        let visible = self.visible_sessions();
        if visible.is_empty() {
            return;
        }
        let i = match self.session_state.selected() {
            Some(i) => {
                if i == 0 {
                    visible.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.session_state.select(Some(i));
    }

    fn next_task(&mut self) {
        if self.global_tasks.is_empty() {
            return;
        }
        let i = match self.task_state.selected() {
            Some(i) => (i + 1) % self.global_tasks.len(),
            None => 0,
        };
        self.task_state.select(Some(i));
    }

    fn previous_task(&mut self) {
        if self.global_tasks.is_empty() {
            return;
        }
        let i = match self.task_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.global_tasks.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.task_state.select(Some(i));
    }
}

// ============================================================================
// File Paths
// ============================================================================

fn get_sessions_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".config/ambient-task-agent")
}

fn get_sessions_file_path() -> PathBuf {
    get_sessions_dir().join("sessions.json")
}

// ============================================================================
// Session Management
// ============================================================================

fn read_session_store() -> SessionsFile {
    let path = get_sessions_file_path();
    match fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => SessionsFile::default(),
    }
}

fn write_session_store(store: &SessionsFile) -> Result<()> {
    let path = get_sessions_file_path();
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let data = serde_json::to_string_pretty(store)?;
    fs::write(path, data)?;
    Ok(())
}

fn get_wezterm_panes() -> Vec<WezTermPane> {
    let output = Command::new("/opt/homebrew/bin/wezterm")
        .args(["cli", "list", "--format", "json"])
        .output();

    match output {
        Ok(out) => serde_json::from_slice(&out.stdout).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

fn load_sessions_data(stale_threshold_mins: i64) -> Vec<SessionItem> {
    let panes = get_wezterm_panes();
    if panes.is_empty() {
        return Vec::new();
    }

    // Build TTY to pane map
    let mut tty_to_pane: HashMap<String, &WezTermPane> = HashMap::new();
    let own_pane_env = std::env::var("WEZTERM_PANE").unwrap_or_default();
    let mut current_window_id = -1;
    let mut current_pane_id = -1;

    for pane in &panes {
        tty_to_pane.insert(pane.tty_name.clone(), pane);
        if !own_pane_env.is_empty() && pane.pane_id.to_string() == own_pane_env {
            current_window_id = pane.window_id;
        }
    }

    if current_window_id == -1 {
        for pane in &panes {
            if pane.is_active {
                current_window_id = pane.window_id;
                break;
            }
        }
    }

    for pane in &panes {
        if pane.is_active && pane.window_id == current_window_id {
            current_pane_id = pane.pane_id;
            break;
        }
    }

    let store = read_session_store();
    let now = Utc::now();
    let stale_threshold = chrono::Duration::minutes(stale_threshold_mins);

    let mut sessions: Vec<SessionItem> = Vec::new();

    for (_, sess) in &store.sessions {
        let pane = tty_to_pane.get(&sess.tty);
        let name = std::path::Path::new(&sess.home_cwd)
            .file_name()
            .map(|s| s.to_string_lossy().to_string())
            .unwrap_or_else(|| sess.home_cwd.clone());

        let created_at = DateTime::parse_from_rfc3339(&sess.created_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(now);
        let updated_at = DateTime::parse_from_rfc3339(&sess.updated_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(now);
        let is_stale = now.signed_duration_since(updated_at) > stale_threshold;

        if let Some(pane) = pane {
            if pane.window_id != current_window_id {
                continue;
            }
            sessions.push(SessionItem {
                tab_id: pane.tab_id,
                pane_id: pane.pane_id,
                name,
                status: sess.status.clone(),
                is_current: pane.pane_id == current_pane_id,
                created_at,
                updated_at,
                is_stale,
                session_id: sess.session_id.clone(),
                is_disconnected: false,
                active_task: sess.active_task.clone(),
                tasks_completed: sess.tasks_completed,
                tasks_total: sess.tasks_total,
            });
        } else {
            // Disconnected session
            let age = now.signed_duration_since(updated_at);
            if age <= chrono::Duration::hours(24) {
                sessions.push(SessionItem {
                    tab_id: -1,
                    pane_id: -1,
                    name,
                    status: sess.status.clone(),
                    is_current: false,
                    created_at,
                    updated_at,
                    is_stale,
                    session_id: sess.session_id.clone(),
                    is_disconnected: true,
                    active_task: sess.active_task.clone(),
                    tasks_completed: sess.tasks_completed,
                    tasks_total: sess.tasks_total,
                });
            }
        }
    }

    // Sort: connected first, then non-stale, then by updated_at
    sessions.sort_by(|a, b| {
        if a.is_disconnected != b.is_disconnected {
            return a.is_disconnected.cmp(&b.is_disconnected);
        }
        if a.is_stale != b.is_stale {
            return a.is_stale.cmp(&b.is_stale);
        }
        b.updated_at.cmp(&a.updated_at)
    });

    sessions
}

fn get_pane_text(pane_id: i32) -> Vec<String> {
    if pane_id < 0 {
        return vec!["(disconnected)".to_string()];
    }

    let output = Command::new("/opt/homebrew/bin/wezterm")
        .args(["cli", "get-text", "--pane-id", &pane_id.to_string()])
        .output();

    match output {
        Ok(out) if out.status.success() => {
            let text = String::from_utf8_lossy(&out.stdout);
            // Trim trailing empty lines, keep last meaningful lines
            let lines: Vec<String> = text.lines().map(|l| l.to_string()).collect();
            // Find last non-empty line
            let last_non_empty = lines.iter().rposition(|l| !l.trim().is_empty()).unwrap_or(0);
            lines[..=last_non_empty].to_vec()
        }
        _ => vec!["(取得失敗)".to_string()],
    }
}

fn update_preview(app: &mut App) {
    let visible = app.visible_sessions();
    if let Some(idx) = app.session_state.selected() {
        if idx < visible.len() {
            let pane_id = visible[idx].pane_id;
            app.pane_preview = get_pane_text(pane_id);
            // Auto-scroll to bottom
            app.preview_scroll = app.pane_preview.len().saturating_sub(1) as u16;
        } else {
            app.pane_preview.clear();
        }
    } else {
        app.pane_preview.clear();
    }
}

fn activate_pane(session: &SessionItem) {
    if session.is_disconnected {
        return;
    }

    let _ = Command::new("/opt/homebrew/bin/wezterm")
        .args(["cli", "activate-tab", "--tab-id", &session.tab_id.to_string()])
        .output();

    let _ = Command::new("/opt/homebrew/bin/wezterm")
        .args([
            "cli",
            "activate-pane",
            "--pane-id",
            &session.pane_id.to_string(),
        ])
        .output();
}

fn delete_session(session: &SessionItem) {
    let mut store = read_session_store();
    store.sessions.remove(&session.session_id);
    let _ = write_session_store(&store);
}

// ============================================================================
// Usage Data
// ============================================================================

fn get_keychain_credentials() -> Option<String> {
    // Try keyring crate first
    if let Ok(entry) = keyring::Entry::new("Claude Code-credentials", "credentials") {
        if let Ok(password) = entry.get_password() {
            return Some(password);
        }
    }

    // Fallback to security command on macOS
    if cfg!(target_os = "macos") {
        let output = Command::new("security")
            .args([
                "find-generic-password",
                "-s",
                "Claude Code-credentials",
                "-w",
            ])
            .output()
            .ok()?;

        if output.status.success() {
            return Some(String::from_utf8_lossy(&output.stdout).trim().to_string());
        }
    }

    None
}

fn load_usage_data() -> UsageLimits {
    let mut result = UsageLimits {
        five_hour: -1,
        weekly: -1,
        sonnet: -1,
        ..Default::default()
    };

    let creds = match get_keychain_credentials() {
        Some(c) => c,
        None => return result,
    };

    let keychain_data: KeychainCreds = match serde_json::from_str(&creds) {
        Ok(d) => d,
        Err(_) => return result,
    };

    let token = &keychain_data.claude_ai_oauth.access_token;
    if token.is_empty() {
        return result;
    }

    let client = reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(3))
        .build()
        .unwrap();

    let response = client
        .get("https://api.anthropic.com/api/oauth/usage")
        .header("Authorization", format!("Bearer {}", token))
        .header("anthropic-beta", "oauth-2025-04-20")
        .send();

    if let Ok(resp) = response {
        if let Ok(usage) = resp.json::<UsageResponse>() {
            result.five_hour = usage.five_hour.utilization as i32;
            if let Some(ref r) = usage.five_hour.resets_at {
                result.five_hour_reset = calculate_reset_time(r);
            }
            result.weekly = usage.seven_day.utilization as i32;
            if let Some(ref r) = usage.seven_day.resets_at {
                result.weekly_reset = format_reset_day(r);
            }
            if let Some(sonnet) = usage.seven_day_sonnet {
                result.sonnet = sonnet.utilization as i32;
            }
        }
    }

    result
}

fn calculate_reset_time(resets_at: &str) -> String {
    let reset_time = match DateTime::parse_from_rfc3339(resets_at) {
        Ok(dt) => dt.with_timezone(&Utc),
        Err(_) => return String::new(),
    };

    let now = Utc::now();
    let diff = reset_time.signed_duration_since(now);

    if diff <= chrono::Duration::zero() {
        return "soon".to_string();
    }

    let hours = diff.num_hours();
    let mins = diff.num_minutes() % 60;

    if hours > 0 {
        format!("{}h{}m", hours, mins)
    } else {
        format!("{}m", mins)
    }
}

fn format_reset_day(resets_at: &str) -> String {
    let reset_time = match DateTime::parse_from_rfc3339(resets_at) {
        Ok(dt) => dt.with_timezone(&Local),
        Err(_) => return String::new(),
    };

    let weekdays = ["日", "月", "火", "水", "木", "金", "土"];
    let weekday_num = reset_time.weekday().num_days_from_sunday() as usize;
    let weekday = weekdays[weekday_num];

    format!("{}{}:{:02}", weekday, reset_time.hour(), reset_time.minute())
}

// ============================================================================
// Hook Handling (stub - session management moved to ambient-task-agent)
// ============================================================================

fn handle_hook(_event_name: &str) -> Result<()> {
    // セッション管理はambient-task-agentに移行済み
    // 互換性のため空のJSONを返すだけ
    println!("{{}}");
    Ok(())
}

// ============================================================================
// TUI Rendering
// ============================================================================

fn ui(frame: &mut Frame, app: &mut App) {
    if app.show_preview {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),  // Usage
                Constraint::Length(12), // Tasks (10 items)
                Constraint::Min(5),     // Sessions (smaller)
                Constraint::Length(12), // Preview
                Constraint::Length(1),  // Status bar
            ])
            .split(frame.area());

        render_usage(frame, app, chunks[0]);
        render_tasks(frame, app, chunks[1]);
        render_sessions(frame, app, chunks[2]);
        render_preview(frame, app, chunks[3]);
        render_status_bar(frame, chunks[4]);
    } else {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),  // Usage
                Constraint::Length(12), // Tasks (10 items)
                Constraint::Min(10),    // Sessions
                Constraint::Length(1),  // Status bar
            ])
            .split(frame.area());

        render_usage(frame, app, chunks[0]);
        render_tasks(frame, app, chunks[1]);
        render_sessions(frame, app, chunks[2]);
        render_status_bar(frame, chunks[3]);
    }

    if app.show_help {
        render_help_popup(frame, app);
    }
}

fn render_usage(frame: &mut Frame, app: &App, area: Rect) {
    let now = Local::now();
    let time_str = now.format("%H:%M:%S").to_string();

    let mut lines = vec![Line::from(format!(" 🕐 {}", time_str))];

    if app.usage.five_hour >= 0 {
        let color = if app.usage.five_hour >= 80 {
            Color::Red
        } else if app.usage.five_hour >= 50 {
            Color::Yellow
        } else {
            Color::Green
        };
        let mut text = format!(" ⏳ 5h: {}%", app.usage.five_hour);
        if !app.usage.five_hour_reset.is_empty() {
            text.push_str(&format!(" ({})", app.usage.five_hour_reset));
        }
        lines.push(Line::from(Span::styled(text, Style::default().fg(color))));
    }

    if app.usage.weekly >= 0 {
        let color = if app.usage.weekly >= 80 {
            Color::Red
        } else if app.usage.weekly >= 50 {
            Color::Yellow
        } else {
            Color::Green
        };
        let mut text = format!(" 📅 All: {}%", app.usage.weekly);
        if !app.usage.weekly_reset.is_empty() {
            text.push_str(&format!(" ({})", app.usage.weekly_reset));
        }
        lines.push(Line::from(Span::styled(text, Style::default().fg(color))));
    }


    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 📊 Usage ");
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_tasks(frame: &mut Frame, app: &mut App, area: Rect) {
    let active_count = app.global_tasks.iter().filter(|t| t.status != "completed").count();
    let total_count = app.global_tasks.len();

    let cache_exists = get_asana_cache_path().exists();

    let items: Vec<ListItem> = if !cache_exists && app.global_tasks.is_empty() {
        vec![ListItem::new(Span::styled(
            "キャッシュなし (sync必要)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else if app.global_tasks.is_empty() {
        vec![ListItem::new(Span::styled(
            "タスクなし",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        let today = Local::now().date_naive();
        app.global_tasks
            .iter()
            .map(|task| {
                let priority_icon = match task.priority {
                    1 => "🔴",
                    3 => "🟢",
                    _ => "🟡",
                };
                let status_text = if task.status == "in_progress" { " ▶" } else { "" };
                let title = truncate_name(&task.title, 24);
                let text = format!("{} {}{}", priority_icon, title, status_text);

                // Color based on deadline
                let color = if let Some(ref due) = task.due_on {
                    if let Ok(due_date) = chrono::NaiveDate::parse_from_str(due, "%Y-%m-%d") {
                        let days_left = (due_date - today).num_days();
                        if days_left < 0 {
                            Color::Red        // overdue
                        } else if days_left <= 3 {
                            Color::Yellow     // due soon
                        } else {
                            Color::Reset
                        }
                    } else {
                        Color::Reset
                    }
                } else {
                    Color::Reset
                };

                ListItem::new(Span::styled(text, Style::default().fg(color)))
            })
            .collect()
    };

    let border_color = if app.focus_mode == FocusMode::Tasks {
        Color::Yellow
    } else {
        Color::Reset
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(format!(" 📋 Tasks ({}/{}) ", active_count, total_count));

    let highlight_style = if app.focus_mode == FocusMode::Tasks {
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style);

    frame.render_stateful_widget(list, area, &mut app.task_state);
}

fn format_duration(created_at: &DateTime<Utc>) -> String {
    let elapsed = Utc::now().signed_duration_since(*created_at);
    let mins = elapsed.num_minutes();
    if mins < 60 {
        format!("{}m", mins)
    } else {
        format!("{}h{}m", mins / 60, mins % 60)
    }
}

fn render_sessions(frame: &mut Frame, app: &mut App, area: Rect) {
    let visible = app.visible_sessions();

    let items: Vec<ListItem> = visible
        .iter()
        .map(|sess| {
            // Line 1: marker + directory name
            let marker = if sess.is_disconnected {
                "⚫"
            } else if sess.is_current {
                "🟢"
            } else {
                "🔵"
            };

            let name = truncate_name(&sess.name, 18);
            let main_text = format!("{} {}", marker, name);
            let mut lines = vec![Line::from(main_text)];

            // Line 2: status + progress or task info
            let status_icon = match sess.status.as_str() {
                "running" => "▶",
                "waiting_input" => "?",
                "stopped" => "■",
                _ => " ",
            };

            let duration = format_duration(&sess.created_at);

            if sess.tasks_total > 0 {
                let progress_bar = render_progress_bar(sess.tasks_completed, sess.tasks_total, 10);
                lines.push(Line::from(Span::styled(
                    format!("  {} {} {} {}/{}", status_icon, duration, progress_bar, sess.tasks_completed, sess.tasks_total),
                    Style::default().fg(Color::Cyan),
                )));
                // Line 3: Active task name
                if let Some(ref task) = sess.active_task {
                    lines.push(Line::from(Span::styled(
                        format!("  ⤷ {}", truncate_name(task, 20)),
                        Style::default().fg(Color::Yellow),
                    )));
                } else if sess.tasks_completed == sess.tasks_total {
                    lines.push(Line::from(Span::styled(
                        "  ✓ 完了".to_string(),
                        Style::default().fg(Color::Green),
                    )));
                }
            } else if sess.is_disconnected {
                lines.push(Line::from(Span::styled(
                    format!("  {} {} (disconnected)", status_icon, duration),
                    Style::default().fg(Color::DarkGray),
                )));
            } else if sess.is_stale {
                lines.push(Line::from(Span::styled(
                    format!("  {} {} (stale)", status_icon, duration),
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    format!("  {} {}", status_icon, duration),
                    Style::default().fg(Color::DarkGray),
                )));
            }

            ListItem::new(lines)
        })
        .collect();

    // Helper function for progress bar
    fn render_progress_bar(completed: i32, total: i32, width: usize) -> String {
        if total == 0 {
            return format!("[{}]", "░".repeat(width));
        }
        let filled = ((completed as f64 / total as f64) * width as f64) as usize;
        let empty = width - filled;
        format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
    }

    let title = if app.show_stale {
        " 🖥 Sessions [All] "
    } else {
        " 🖥 Sessions [Active] "
    };

    let block = Block::default().borders(Borders::ALL).title(title);

    let highlight_style = if app.focus_mode == FocusMode::Sessions {
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style);

    frame.render_stateful_widget(list, area, &mut app.session_state);
}

fn render_preview(frame: &mut Frame, app: &App, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize; // border top+bottom

    // Show last N lines (scroll from bottom)
    let total_lines = app.pane_preview.len();
    let max_scroll = total_lines.saturating_sub(inner_height) as u16;
    let scroll = app.preview_scroll.min(max_scroll);

    let start = scroll as usize;
    let end = (start + inner_height).min(total_lines);

    let lines: Vec<Line> = if app.pane_preview.is_empty() {
        vec![Line::from(Span::styled(
            "(no data)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.pane_preview[start..end]
            .iter()
            .map(|l| Line::from(l.as_str()))
            .collect()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 👁 Preview ");
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_status_bar(frame: &mut Frame, area: Rect) {
    // Minimal status bar for narrow sidebar
    let text = "?:help q:quit";
    let style = Style::default().fg(Color::DarkGray);
    let paragraph = Paragraph::new(text).style(style);
    frame.render_widget(paragraph, area);
}

fn render_help_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(36, 14, frame.area());

    let lines = if app.focus_mode == FocusMode::Tasks {
        vec![
            Line::from(" 📋 Tasks Mode (Asana)"),
            Line::from(""),
            Line::from(" j/k   上下移動"),
            Line::from(" Esc   セッションに戻る"),
            Line::from(" q     終了"),
            Line::from(""),
            Line::from(" Press any key to close"),
        ]
    } else {
        vec![
            Line::from(" 🖥 Sessions Mode"),
            Line::from(""),
            Line::from(" t       タスクモード"),
            Line::from(" p       プレビュー切替"),
            Line::from(" Enter   ペイン切替"),
            Line::from(" 1-9     番号でペイン切替"),
            Line::from(" d       セッション削除"),
            Line::from(" f       全表示切替"),
            Line::from(" r       更新"),
            Line::from(" q/Esc   終了"),
            Line::from(""),
            Line::from(" Press any key to close"),
        ]
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ");

    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

fn truncate_name(name: &str, max_len: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= max_len {
        name.to_string()
    } else {
        format!("{}…", chars[..max_len - 1].iter().collect::<String>())
    }
}

// ============================================================================
// Event Handling
// ============================================================================

enum AppEvent {
    Tick,
    Key(event::KeyEvent),
    SessionsUpdated,
    AsanaTasksUpdated(Vec<GlobalTask>),
    UsageUpdated(UsageLimits),
}

fn run_tui() -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new();

    // Load initial data (usage and kintone tasks are loaded async)
    app.sessions = load_sessions_data(30);

    // Setup channels for events
    let (tx, rx) = mpsc::channel::<AppEvent>();

    // Tick thread
    let tx_tick = tx.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        let _ = tx_tick.send(AppEvent::Tick);
    });

    // File watcher for sessions
    let tx_sessions = tx.clone();
    let sessions_path = get_sessions_file_path();
    let sessions_dir = sessions_path.parent().unwrap().to_path_buf();
    thread::spawn(move || {
        let (watcher_tx, watcher_rx) = mpsc::channel();
        let mut watcher: RecommendedWatcher =
            Watcher::new(watcher_tx, NotifyConfig::default()).unwrap();
        let _ = fs::create_dir_all(&sessions_dir);
        let _ = watcher.watch(&sessions_dir, RecursiveMode::NonRecursive);

        loop {
            if let Ok(Ok(event)) = watcher_rx.recv() {
                if event
                    .paths
                    .iter()
                    .any(|p| p.file_name().map(|n| n == "sessions.json").unwrap_or(false))
                {
                    thread::sleep(Duration::from_millis(150));
                    let _ = tx_sessions.send(AppEvent::SessionsUpdated);
                }
            }
        }
    });

    // Asana tasks: initial load from cache
    app.global_tasks = load_asana_tasks();

    // File watcher for Asana tasks cache
    let tx_asana = tx.clone();
    let asana_cache_dir = get_asana_cache_dir();
    thread::spawn(move || {
        let (watcher_tx, watcher_rx) = mpsc::channel();
        let mut watcher: RecommendedWatcher =
            Watcher::new(watcher_tx, NotifyConfig::default()).unwrap();
        let _ = fs::create_dir_all(&asana_cache_dir);
        let _ = watcher.watch(&asana_cache_dir, RecursiveMode::NonRecursive);

        loop {
            if let Ok(Ok(event)) = watcher_rx.recv() {
                if event
                    .paths
                    .iter()
                    .any(|p| p.file_name().map(|n| n == "tasks-cache.json").unwrap_or(false))
                {
                    thread::sleep(Duration::from_millis(200));
                    let tasks = load_asana_tasks();
                    let _ = tx_asana.send(AppEvent::AsanaTasksUpdated(tasks));
                }
            }
        }
    });

    // Usage refresh thread (also handles initial load)
    let tx_usage = tx.clone();
    thread::spawn(move || {
        // Initial load immediately
        let usage = load_usage_data();
        let _ = tx_usage.send(AppEvent::UsageUpdated(usage));
        // Then refresh every 60s
        loop {
            thread::sleep(Duration::from_secs(60));
            let usage = load_usage_data();
            let _ = tx_usage.send(AppEvent::UsageUpdated(usage));
        }
    });

    // Key event thread
    let tx_key = tx.clone();
    thread::spawn(move || loop {
        if event::poll(Duration::from_millis(100)).unwrap() {
            if let Event::Key(key) = event::read().unwrap() {
                if key.kind == KeyEventKind::Press {
                    let _ = tx_key.send(AppEvent::Key(key));
                }
            }
        }
    });

    // Main loop
    let mut tick_count: u32 = 0;
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(AppEvent::Tick) => {
                tick_count = tick_count.wrapping_add(1);
                // Refresh preview every 3 seconds
                if app.show_preview && tick_count % 3 == 0 {
                    update_preview(&mut app);
                }
            }
            Ok(AppEvent::Key(key)) => {
                if app.show_help {
                    app.show_help = false;
                } else {
                    handle_key(&mut app, key);
                }
            }
            Ok(AppEvent::SessionsUpdated) => {
                app.sessions = load_sessions_data(30);
            }
            Ok(AppEvent::AsanaTasksUpdated(tasks)) => {
                app.global_tasks = tasks;
            }
            Ok(AppEvent::UsageUpdated(usage)) => {
                app.usage = usage;
            }
            Err(_) => {}
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}

fn handle_key(app: &mut App, key: event::KeyEvent) {
    // Common keys for all modes
    if key.code == KeyCode::Char('?') {
        app.show_help = true;
        return;
    }

    match app.focus_mode {
        FocusMode::Tasks => handle_tasks_key(app, key),
        FocusMode::Sessions => handle_sessions_key(app, key),
    }
}

fn handle_sessions_key(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('t') => app.focus_mode = FocusMode::Tasks,
        KeyCode::Char('f') => app.show_stale = !app.show_stale,
        KeyCode::Char('p') => {
            app.show_preview = !app.show_preview;
            if app.show_preview {
                update_preview(app);
            }
        }
        KeyCode::Char('r') => {
            app.sessions = load_sessions_data(30);
            app.global_tasks = load_asana_tasks();
            app.usage = load_usage_data();
        }
        KeyCode::Char('d') => {
            let visible = app.visible_sessions();
            if let Some(idx) = app.session_state.selected() {
                if idx < visible.len() {
                    delete_session(visible[idx]);
                    app.sessions = load_sessions_data(30);
                }
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.previous_session();
            if app.show_preview {
                update_preview(app);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.next_session();
            if app.show_preview {
                update_preview(app);
            }
        }
        KeyCode::Enter => {
            let visible = app.visible_sessions();
            if let Some(idx) = app.session_state.selected() {
                if idx < visible.len() {
                    activate_pane(visible[idx]);
                }
            }
        }
        KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
            let idx = (c as usize) - ('1' as usize);
            let visible: Vec<SessionItem> = app.visible_sessions().into_iter().cloned().collect();
            if idx < visible.len() {
                app.session_state.select(Some(idx));
                activate_pane(&visible[idx]);
            }
        }
        _ => {}
    }
}

fn handle_tasks_key(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Esc => app.focus_mode = FocusMode::Sessions,
        KeyCode::Up | KeyCode::Char('k') => app.previous_task(),
        KeyCode::Down | KeyCode::Char('j') => app.next_task(),
        _ => {}
    }
}

// ============================================================================
// Main
// ============================================================================

fn main() -> Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Some(Commands::Hook { event }) => {
            handle_hook(&event)?;
        }
        None => {
            run_tui()?;
        }
    }

    Ok(())
}
