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
    pub is_yolo: bool,
    #[serde(default)]
    pub last_activity: Option<String>,
    #[serde(default)]
    pub is_dangerous: bool,
    #[serde(default)]
    pub git_branch: Option<String>,
    #[serde(default)]
    pub last_user_message: Option<String>,
    #[serde(default)]
    pub last_user_message_at: Option<String>,
    #[serde(default)]
    pub tasks: Vec<SessionTask>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SessionTask {
    pub content: String,
    pub status: String,
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
    pub is_yolo: bool,
    pub last_activity: Option<String>,
    pub is_dangerous: bool,
    pub git_branch: Option<String>,
    pub home_cwd: String,
    pub last_user_message: Option<String>,
    pub last_user_message_at: Option<DateTime<Utc>>,
    pub tasks: Vec<SessionTask>,
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

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageLimits {
    pub five_hour: i32,
    pub five_hour_reset: String,
    pub weekly: i32,
    pub weekly_reset: String,
    pub sonnet: i32,
}

// ============================================================================
// Hook
// ============================================================================

#[derive(Debug, Deserialize)]
pub struct HookPayload {
    pub session_id: String,
    pub cwd: Option<String>,
    pub notification_type: Option<String>,
    pub tool_name: Option<String>,
    pub tool_input: Option<serde_json::Value>,
    pub prompt: Option<String>,
}

// ============================================================================
// Events
// ============================================================================

pub enum AppEvent {
    Tick,
    Key(event::KeyEvent),
    SessionsUpdated,
    UsageUpdated(UsageLimits),
}
