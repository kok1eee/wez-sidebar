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
    io::{self, Read},
    path::PathBuf,
    process::Command,
    sync::mpsc,
    thread,
    time::Duration,
};

// ============================================================================
// Kintone Configuration
// ============================================================================

#[derive(Debug, Clone)]
struct KintoneConfig {
    domain: String,
    app_id: String,
    api_token: String,
}

fn load_credentials_env() -> HashMap<String, String> {
    let path = dirs::home_dir()
        .unwrap_or_default()
        .join(".credentials/common.env");
    let mut map = HashMap::new();
    if let Ok(content) = fs::read_to_string(&path) {
        for line in content.lines() {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                continue;
            }
            if let Some((key, value)) = line.split_once('=') {
                let value = value.trim().trim_matches('"').trim_matches('\'');
                map.insert(key.trim().to_string(), value.to_string());
            }
        }
    }
    map
}

fn load_kintone_config() -> Option<KintoneConfig> {
    let env = load_credentials_env();
    let domain = env.get("KINTONE_DOMAIN")?.clone();
    let app_id = env.get("KINTONE_APP_ID_TASKS")?.clone();
    let api_token = env.get("KINTONE_API_TOKEN")?.clone();
    if domain.is_empty() || app_id.is_empty() || api_token.is_empty() {
        return None;
    }
    Some(KintoneConfig {
        domain,
        app_id,
        api_token,
    })
}

// ============================================================================
// Kintone API
// ============================================================================

#[derive(Debug, Deserialize)]
struct KintoneRecordsResponse {
    records: Vec<KintoneRecord>,
}

#[derive(Debug, Deserialize)]
struct KintoneRecord {
    #[serde(rename = "$id")]
    id: KintoneFieldValue,
    #[serde(rename = "タイトル")]
    title: KintoneFieldValue,
    status: KintoneFieldValue,
    #[serde(rename = "優先度")]
    priority: KintoneFieldValue,
    #[serde(rename = "期限")]
    deadline: KintoneFieldValue,
    #[serde(rename = "作成日時")]
    created_at: KintoneFieldValue,
}

#[derive(Debug, Deserialize)]
struct KintoneFieldValue {
    value: Option<String>,
}

fn fetch_kintone_tasks(config: &KintoneConfig) -> Vec<GlobalTask> {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let query = "status in (\"todo\", \"in_progress\") order by 作成日時 desc limit 20";
    let url = format!(
        "https://{}/k/v1/records.json?app={}&query={}",
        config.domain,
        config.app_id,
        urlencoded(query)
    );

    let resp = client
        .get(&url)
        .header("X-Cybozu-API-Token", &config.api_token)
        .send();

    let records: Vec<KintoneRecord> = match resp {
        Ok(r) => match r.json::<KintoneRecordsResponse>() {
            Ok(data) => data.records,
            Err(_) => return Vec::new(),
        },
        Err(_) => return Vec::new(),
    };

    let now = Utc::now();
    let mut tasks: Vec<(f64, GlobalTask)> = records
        .into_iter()
        .map(|r| {
            let id = r.id.value.unwrap_or_default();
            let title = r.title.value.unwrap_or_default();
            let status_raw = r.status.value.unwrap_or_default();
            let priority_str = r.priority.value.unwrap_or_default();
            let deadline_str = r.deadline.value.unwrap_or_default();
            let created_str = r.created_at.value.unwrap_or_default();

            let status = match status_raw.as_str() {
                "in_progress" => "in_progress",
                _ => "pending",
            }
            .to_string();

            // Priority mapping
            let priority = match priority_str.as_str() {
                "urgent" => 1,
                "this_week" => 2,
                _ => 3, // someday
            };

            // Priority score calculation
            let base_score: f64 = match priority_str.as_str() {
                "urgent" => 100.0,
                "this_week" => 50.0,
                _ => 10.0,
            };

            // Deadline bonus
            let deadline_score = if !deadline_str.is_empty() {
                if let Ok(dl) = DateTime::parse_from_rfc3339(&deadline_str) {
                    let days_left = dl.signed_duration_since(now).num_days();
                    if days_left < 0 {
                        30.0 // overdue
                    } else if days_left <= 1 {
                        20.0
                    } else if days_left <= 3 {
                        10.0
                    } else {
                        0.0
                    }
                } else {
                    // Try YYYY-MM-DD format
                    if let Ok(dl) = chrono::NaiveDate::parse_from_str(&deadline_str, "%Y-%m-%d") {
                        let today = now.date_naive();
                        let days_left = (dl - today).num_days();
                        if days_left < 0 {
                            30.0
                        } else if days_left <= 1 {
                            20.0
                        } else if days_left <= 3 {
                            10.0
                        } else {
                            0.0
                        }
                    } else {
                        0.0
                    }
                }
            } else {
                0.0
            };

            // Staleness penalty (older = higher priority bump)
            let stale_score = if !created_str.is_empty() {
                if let Ok(ct) = DateTime::parse_from_rfc3339(&created_str) {
                    let days_old = now.signed_duration_since(ct).num_days();
                    (days_old as f64 * 0.5).min(15.0)
                } else {
                    0.0
                }
            } else {
                0.0
            };

            let score = base_score + deadline_score + stale_score;

            let task = GlobalTask {
                id,
                title,
                status,
                priority,
                created_at: created_str,
                updated_at: String::new(),
            };

            (score, task)
        })
        .collect();

    // Sort by score descending
    tasks.sort_by(|a, b| b.0.partial_cmp(&a.0).unwrap_or(std::cmp::Ordering::Equal));
    tasks.into_iter().map(|(_, t)| t).collect()
}

fn update_kintone_task_status(config: &KintoneConfig, record_id: &str, new_status: &str) {
    let client = match reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(5))
        .build()
    {
        Ok(c) => c,
        Err(_) => return,
    };

    let url = format!("https://{}/k/v1/record.json", config.domain);

    let mut record_fields = serde_json::Map::new();
    record_fields.insert(
        "status".to_string(),
        serde_json::json!({"value": new_status}),
    );
    if new_status == "done" {
        record_fields.insert(
            "completed_at".to_string(),
            serde_json::json!({"value": Utc::now().to_rfc3339()}),
        );
    }

    let body = serde_json::json!({
        "app": config.app_id,
        "id": record_id,
        "record": record_fields,
    });

    let _ = client
        .put(&url)
        .header("X-Cybozu-API-Token", &config.api_token)
        .header("Content-Type", "application/json")
        .json(&body)
        .send();
}

fn urlencoded(s: &str) -> String {
    let mut result = String::new();
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                result.push(b as char);
            }
            _ => {
                result.push_str(&format!("%{:02X}", b));
            }
        }
    }
    result
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

#[derive(Debug, Clone, Serialize, Deserialize)]
struct HookPayload {
    session_id: String,
    cwd: Option<String>,
    notification_type: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
struct WezTermPane {
    window_id: i32,
    tab_id: i32,
    pane_id: i32,
    tty_name: String,
    #[allow(dead_code)]
    title: String,
    cwd: String,
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
    resets_at: String,
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
    window_id: i32,
    tab_id: i32,
    pane_id: i32,
    name: String,
    #[allow(dead_code)]
    cwd: String,
    status: String,
    is_current: bool,
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
    kintone_config: Option<KintoneConfig>,
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
            kintone_config: load_kintone_config(),
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

fn get_data_dir() -> PathBuf {
    dirs::home_dir()
        .unwrap_or_default()
        .join(".config/wez-sidebar")
}

fn get_sessions_file_path() -> PathBuf {
    get_data_dir().join("sessions.json")
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

        let updated_at = DateTime::parse_from_rfc3339(&sess.updated_at)
            .map(|dt| dt.with_timezone(&Utc))
            .unwrap_or(now);
        let is_stale = now.signed_duration_since(updated_at) > stale_threshold;

        if let Some(pane) = pane {
            if pane.window_id != current_window_id {
                continue;
            }
            let (task_active, task_done, task_total) = read_claude_tasks(&sess.session_id);
            sessions.push(SessionItem {
                window_id: pane.window_id,
                tab_id: pane.tab_id,
                pane_id: pane.pane_id,
                name,
                cwd: sess.home_cwd.clone(),
                status: sess.status.clone(),
                is_current: pane.pane_id == current_pane_id,
                updated_at,
                is_stale,
                session_id: sess.session_id.clone(),
                is_disconnected: false,
                active_task: task_active,
                tasks_completed: task_done,
                tasks_total: task_total,
            });
        } else {
            // Disconnected session
            let age = now.signed_duration_since(updated_at);
            if age <= chrono::Duration::hours(24) {
                let (task_active, task_done, task_total) = read_claude_tasks(&sess.session_id);
                sessions.push(SessionItem {
                    window_id: -1,
                    tab_id: -1,
                    pane_id: -1,
                    name,
                    cwd: sess.home_cwd.clone(),
                    status: sess.status.clone(),
                    is_current: false,
                    updated_at,
                    is_stale,
                    session_id: sess.session_id.clone(),
                    is_disconnected: true,
                    active_task: task_active,
                    tasks_completed: task_done,
                    tasks_total: task_total,
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
            result.five_hour_reset = calculate_reset_time(&usage.five_hour.resets_at);
            result.weekly = usage.seven_day.utilization as i32;
            result.weekly_reset = format_reset_day(&usage.seven_day.resets_at);
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
// Hook Handling
// ============================================================================

const VALID_HOOK_EVENTS: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
    "UserPromptSubmit",
];

fn read_stdin_with_timeout(timeout: Duration) -> String {
    let (tx, rx) = mpsc::channel();
    thread::spawn(move || {
        let mut input = String::new();
        let _ = io::stdin().read_to_string(&mut input);
        let _ = tx.send(input);
    });

    rx.recv_timeout(timeout).unwrap_or_default()
}

fn handle_hook(event_name: &str) -> Result<()> {
    if !VALID_HOOK_EVENTS.contains(&event_name) {
        anyhow::bail!("Invalid event name: {}", event_name);
    }

    let tty = get_tty_from_ancestors();

    let input = read_stdin_with_timeout(Duration::from_secs(2));

    if input.is_empty() {
        println!("{{}}");
        return Ok(());
    }

    let payload: HookPayload = serde_json::from_str(&input)?;

    if payload.session_id.is_empty() {
        anyhow::bail!("Missing session_id");
    }

    let cwd = payload
        .cwd
        .unwrap_or_else(|| std::env::current_dir().unwrap().to_string_lossy().to_string());

    update_session_store(
        event_name,
        &payload.session_id,
        &cwd,
        &tty,
        payload.notification_type.as_deref(),
    )?;

    println!("{{}}");
    Ok(())
}

fn get_tty_from_ancestors() -> String {
    let mut ppid = std::os::unix::process::parent_id() as i32;

    for _ in 0..5 {
        let output = Command::new("ps")
            .args(["-o", "tty=", "-p", &ppid.to_string()])
            .output();

        if let Ok(out) = output {
            let tty = String::from_utf8_lossy(&out.stdout).trim().to_string();
            if !tty.is_empty() && tty != "??" {
                return format!("/dev/{}", tty);
            }
        }

        let output = Command::new("ps")
            .args(["-o", "ppid=", "-p", &ppid.to_string()])
            .output();

        if let Ok(out) = output {
            if let Ok(new_ppid) = String::from_utf8_lossy(&out.stdout).trim().parse::<i32>() {
                ppid = new_ppid;
            } else {
                break;
            }
        } else {
            break;
        }
    }

    String::new()
}

// Read tasks from ~/.claude/tasks/<session_id>/*.json
fn read_claude_tasks(session_id: &str) -> (Option<String>, i32, i32) {
    let tasks_dir = dirs::home_dir()
        .unwrap_or_default()
        .join(".claude/tasks")
        .join(session_id);

    let entries = match fs::read_dir(&tasks_dir) {
        Ok(e) => e,
        Err(_) => return (None, 0, 0),
    };

    #[derive(Deserialize)]
    struct TaskItem {
        subject: String,
        status: String,
    }

    let mut items: Vec<TaskItem> = Vec::new();
    for entry in entries.flatten() {
        if entry.path().extension().map(|e| e == "json").unwrap_or(false) {
            if let Ok(content) = fs::read_to_string(entry.path()) {
                if let Ok(item) = serde_json::from_str::<TaskItem>(&content) {
                    items.push(item);
                }
            }
        }
    }

    if items.is_empty() {
        return (None, 0, 0);
    }

    let total = items.len() as i32;
    let completed = items.iter().filter(|t| t.status == "completed").count() as i32;

    let active = items
        .iter()
        .find(|t| t.status == "in_progress")
        .or_else(|| items.iter().find(|t| t.status == "pending"))
        .map(|t| t.subject.clone());

    (active, completed, total)
}

fn determine_status(event_name: &str, notification_type: Option<&str>, current_status: &str) -> String {
    if event_name == "Stop" {
        return "stopped".to_string();
    }
    if event_name == "UserPromptSubmit" {
        return "running".to_string();
    }
    if current_status == "stopped" {
        return "stopped".to_string();
    }
    if event_name == "PreToolUse" {
        return "running".to_string();
    }
    if event_name == "Notification" && notification_type == Some("permission_prompt") {
        return "waiting_input".to_string();
    }
    "running".to_string()
}

fn update_session_store(
    event_name: &str,
    session_id: &str,
    cwd: &str,
    tty: &str,
    notification_type: Option<&str>,
) -> Result<()> {
    let mut store = read_session_store();
    let now = Utc::now().to_rfc3339();

    // Clean up sessions with same TTY but different session_id
    if !tty.is_empty() {
        store.sessions.retain(|k, s| s.tty != tty || k == session_id);
    }

    let existing = store.sessions.get(session_id);
    let current_status = existing.map(|s| s.status.as_str()).unwrap_or("");
    let created_at = existing
        .map(|s| s.created_at.clone())
        .unwrap_or_else(|| now.clone());
    // Always use the hook's cwd as home_cwd (= Claude Code's working directory)
    let home_cwd = cwd.to_string();
    let final_tty = existing
        .and_then(|s| if s.tty.is_empty() { None } else { Some(s.tty.clone()) })
        .unwrap_or_else(|| tty.to_string());

    // Read tasks from ~/.claude/tasks/<session_id>/
    let (active_task, tasks_completed, tasks_total) = read_claude_tasks(session_id);

    store.sessions.insert(
        session_id.to_string(),
        Session {
            session_id: session_id.to_string(),
            home_cwd,
            tty: final_tty,
            status: determine_status(event_name, notification_type, current_status),
            created_at,
            updated_at: now.clone(),
            active_task,
            tasks_completed,
            tasks_total,
        },
    );

    store.updated_at = now;
    write_session_store(&store)?;
    Ok(())
}

// ============================================================================
// TUI Rendering
// ============================================================================

fn ui(frame: &mut Frame, app: &mut App) {
    let chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(5),  // Usage (time + 5h + weekly)
            Constraint::Length(7),  // Tasks (5 items)
            Constraint::Min(10),    // Sessions
            Constraint::Length(1),  // Status bar
        ])
        .split(frame.area());

    render_usage(frame, app, chunks[0]);
    render_tasks(frame, app, chunks[1]);
    render_sessions(frame, app, chunks[2]);
    render_status_bar(frame, app, chunks[3]);

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

    let items: Vec<ListItem> = if app.kintone_config.is_none() {
        vec![ListItem::new(Span::styled(
            "kintone未設定",
            Style::default().fg(Color::DarkGray),
        ))]
    } else if app.global_tasks.is_empty() {
        vec![ListItem::new(Span::styled(
            "タスクなし",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
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
                ListItem::new(format!("{} {}{}", priority_icon, title, status_text))
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

            if sess.tasks_total > 0 {
                let progress_bar = render_progress_bar(sess.tasks_completed, sess.tasks_total, 10);
                lines.push(Line::from(Span::styled(
                    format!("  {} {} {}/{}", status_icon, progress_bar, sess.tasks_completed, sess.tasks_total),
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
                    format!("  {} (disconnected)", status_icon),
                    Style::default().fg(Color::DarkGray),
                )));
            } else if sess.is_stale {
                lines.push(Line::from(Span::styled(
                    format!("  {} (stale)", status_icon),
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    format!("  {}", status_icon),
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

fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
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
            Line::from(" 📋 Tasks Mode (kintone)"),
            Line::from(""),
            Line::from(" s     タスク開始"),
            Line::from(" d     タスク完了"),
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
    KintoneTasksUpdated(Vec<GlobalTask>),
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

    // Kintone tasks: initial load only (further updates triggered by user actions)
    if let Some(kintone_cfg) = app.kintone_config.clone() {
        let tx_kintone = tx.clone();
        thread::spawn(move || {
            let tasks = fetch_kintone_tasks(&kintone_cfg);
            let _ = tx_kintone.send(AppEvent::KintoneTasksUpdated(tasks));
        });
    }

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
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(AppEvent::Tick) => {
                // Just redraw for clock update
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
            Ok(AppEvent::KintoneTasksUpdated(tasks)) => {
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
        KeyCode::Char('r') => {
            app.sessions = load_sessions_data(30);
            if let Some(ref config) = app.kintone_config {
                app.global_tasks = fetch_kintone_tasks(config);
            }
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
        KeyCode::Up | KeyCode::Char('k') => app.previous_session(),
        KeyCode::Down | KeyCode::Char('j') => app.next_session(),
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
        KeyCode::Char('s') => {
            if let (Some(idx), Some(ref config)) = (app.task_state.selected(), &app.kintone_config) {
                if idx < app.global_tasks.len() {
                    let id = app.global_tasks[idx].id.clone();
                    let config = config.clone();
                    update_kintone_task_status(&config, &id, "in_progress");
                    app.global_tasks = fetch_kintone_tasks(&config);
                }
            }
        }
        KeyCode::Char('d') => {
            if let (Some(idx), Some(ref config)) = (app.task_state.selected(), &app.kintone_config) {
                if idx < app.global_tasks.len() {
                    let id = app.global_tasks[idx].id.clone();
                    let config = config.clone();
                    update_kintone_task_status(&config, &id, "done");
                    app.global_tasks = fetch_kintone_tasks(&config);
                    if !app.global_tasks.is_empty() {
                        let new_idx = idx.min(app.global_tasks.len() - 1);
                        app.task_state.select(Some(new_idx));
                    }
                }
            }
        }
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
