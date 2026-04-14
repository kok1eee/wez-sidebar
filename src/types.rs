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
    pub is_yolo: bool, // Deprecated: kept for backward compat with old sessions.json
    #[serde(default)]
    pub permission_mode: String, // "normal", "yolo", "auto"
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
    #[serde(default)]
    pub subagents: Vec<SubagentEntry>,
    #[serde(default)]
    pub pane_id: Option<i32>,
    #[serde(default)]
    pub context_percent: Option<u8>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SubagentEntry {
    pub session_id: String,
    pub last_seen: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SessionTask {
    #[serde(default)]
    pub id: String,
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
    pub permission_mode: String,
    pub last_activity: Option<String>,
    pub is_dangerous: bool,
    pub git_branch: Option<String>,
    pub home_cwd: String,
    pub last_user_message: Option<String>,
    pub last_user_message_at: Option<DateTime<Utc>>,
    pub tasks: Vec<SessionTask>,
    pub active_subagents: usize,
    pub context_percent: Option<u8>,
}

// ============================================================================
// Kanban Task
// ============================================================================

/// Kanban task status.
///
/// Lifecycle: backlog → running → review → done
/// Any state can transition to trash (soft delete).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum TaskStatus {
    Backlog,
    Running,
    Review,
    Done,
    Trash,
}

impl TaskStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Backlog => "backlog",
            TaskStatus::Running => "running",
            TaskStatus::Review => "review",
            TaskStatus::Done => "done",
            TaskStatus::Trash => "trash",
        }
    }

    pub fn parse(s: &str) -> Option<Self> {
        match s {
            "backlog" => Some(TaskStatus::Backlog),
            "running" => Some(TaskStatus::Running),
            "review" => Some(TaskStatus::Review),
            "done" => Some(TaskStatus::Done),
            "trash" => Some(TaskStatus::Trash),
            _ => None,
        }
    }
}

/// Kanban task record (persisted to tasks.json).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KanbanTask {
    /// Short UUID (8 hex chars)
    pub id: String,
    /// Display name / `claude -n <title>` value
    pub title: String,
    /// Initial prompt to send on spawn (optional)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub prompt: Option<String>,
    pub status: TaskStatus,
    /// Working directory for spawn
    pub cwd: String,
    /// Claude Code session_id (set when bound to a running session)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<String>,
    /// Creation timestamp (RFC 3339)
    pub created_at: String,
    /// When task moved to running (RFC 3339)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at: Option<String>,
    /// When task moved to review (for block-detection timing)
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_started_at: Option<String>,
    /// When task moved to done
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at: Option<String>,
    /// Block-alert dedupe: timestamp of last block notification fired for this task
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub block_alerted_at: Option<String>,
}

/// Task dependency edge: `from` must be done before `to` can start.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskDependency {
    pub from: String,
    pub to: String,
}

/// tasks.json root schema.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct TasksFile {
    #[serde(default)]
    pub tasks: Vec<KanbanTask>,
    #[serde(default)]
    pub dependencies: Vec<TaskDependency>,
    #[serde(default)]
    pub updated_at: String,
}

// ============================================================================
// Kanban UI state (Phase 6-7)
// ============================================================================

/// User-visible kanban view selection.
///
/// `Auto` defers the kanban/flat decision to `kanban.auto_flat_threshold`.
/// `Kanban` and `Flat` force the respective layout until the user toggles
/// again with the `v` key.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewMode {
    Auto,
    Kanban,
    Flat,
}

impl ViewMode {
    /// Cycle: Auto → Kanban → Flat → Auto.
    pub fn next(self) -> Self {
        match self {
            ViewMode::Auto => ViewMode::Kanban,
            ViewMode::Kanban => ViewMode::Flat,
            ViewMode::Flat => ViewMode::Auto,
        }
    }

    #[allow(dead_code)]
    pub fn label(self) -> &'static str {
        match self {
            ViewMode::Auto => "auto",
            ViewMode::Kanban => "kanban",
            ViewMode::Flat => "flat",
        }
    }
}

/// Resolved (effective) layout after `ViewMode::Auto` is collapsed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EffectiveView {
    Kanban,
    Flat,
}

/// Kanban column.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KanbanColumn {
    Active,
    Review,
    Done,
}

impl KanbanColumn {
    pub const ALL: [KanbanColumn; 3] =
        [KanbanColumn::Active, KanbanColumn::Review, KanbanColumn::Done];

    pub fn label(self) -> &'static str {
        match self {
            KanbanColumn::Active => "Active",
            KanbanColumn::Review => "Review",
            KanbanColumn::Done => "Done",
        }
    }

    pub fn index(self) -> usize {
        match self {
            KanbanColumn::Active => 0,
            KanbanColumn::Review => 1,
            KanbanColumn::Done => 2,
        }
    }
}

// ============================================================================
// Usage (cache read only — data written by statusline script)
// ============================================================================

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct UsageLimits {
    pub five_hour: i32,
    pub five_hour_reset: String,
    pub weekly: i32,
    pub weekly_reset: String,
    pub sonnet: i32,
    /// cache file の mtime からの経過秒数 (TUI表示用、シリアライズしない)
    #[serde(skip)]
    pub cache_age_secs: Option<u64>,
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
    pub tool_response: Option<serde_json::Value>,
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
