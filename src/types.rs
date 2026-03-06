use chrono::{DateTime, Utc};
use crossterm::event;
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

// ============================================================================
// Session Data
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
pub struct SessionsFile {
    pub sessions: HashMap<String, Session>,
    pub updated_at: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Session {
    pub session_id: String,
    pub home_cwd: String,
    pub tty: String,
    pub status: String,
    pub created_at: String,
    pub updated_at: String,
    #[serde(default)]
    pub active_task: Option<String>,
    #[serde(default)]
    pub tasks_completed: i32,
    #[serde(default)]
    pub tasks_total: i32,
    #[serde(default)]
    pub is_yolo: bool,
}

#[derive(Debug, Clone)]
pub struct SessionItem {
    pub tab_id: i32,
    pub pane_id: i32,
    pub name: String,
    pub status: String,
    pub is_current: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub is_stale: bool,
    pub session_id: String,
    pub is_disconnected: bool,
    pub active_task: Option<String>,
    pub tasks_completed: i32,
    pub tasks_total: i32,
    pub is_yolo: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct WezTermPane {
    pub window_id: i32,
    pub tab_id: i32,
    pub pane_id: i32,
    pub tty_name: String,
    #[allow(dead_code)]
    pub title: String,
    pub is_active: bool,
}

// ============================================================================
// Usage API
// ============================================================================

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageResponse {
    pub five_hour: UsageData,
    pub seven_day: UsageData,
    pub seven_day_sonnet: Option<UsageData>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct UsageData {
    pub utilization: f64,
    #[serde(default)]
    pub resets_at: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct KeychainCreds {
    #[serde(rename = "claudeAiOauth")]
    pub claude_ai_oauth: OAuthData,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct OAuthData {
    #[serde(rename = "accessToken")]
    pub access_token: String,
}

#[derive(Debug, Clone, Default)]
pub struct UsageLimits {
    pub five_hour: i32,
    pub five_hour_reset: String,
    pub weekly: i32,
    pub weekly_reset: String,
    pub sonnet: i32,
}

// ============================================================================
// Tasks
// ============================================================================

#[derive(Debug, Serialize, Deserialize)]
pub struct TasksFile {
    pub tasks: Vec<Task>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Task {
    pub id: String,
    pub title: String,
    #[serde(default = "default_status")]
    pub status: String, // "pending", "in_progress", "completed"
    #[serde(default = "default_priority")]
    pub priority: i32, // 1=high, 2=medium, 3=low
    #[serde(default)]
    pub due_on: Option<String>, // "2026-03-10"
}

fn default_status() -> String {
    "pending".to_string()
}

pub fn default_priority() -> i32 {
    3
}

// ============================================================================
// Hook
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct HookPayload {
    pub session_id: String,
    pub cwd: Option<String>,
    pub notification_type: Option<String>,
}

// ============================================================================
// Events
// ============================================================================

pub enum AppEvent {
    Tick,
    Key(event::KeyEvent),
    SessionsUpdated,
    TasksUpdated(Vec<Task>),
    UsageUpdated(UsageLimits),
    ApiStatusChanged(bool),
}
