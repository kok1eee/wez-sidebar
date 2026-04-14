use serde::Deserialize;
use std::{fs, path::PathBuf};

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    /// Terminal backend: "wezterm" (default) or "tmux"
    pub backend: String,
    /// Path to terminal CLI binary (auto-detected if empty)
    pub terminal_path: String,
    /// Legacy field: maps to terminal_path for backward compat
    #[serde(default)]
    pub wezterm_path: String,
    pub stale_threshold_mins: i64,
    pub data_dir: String,
    #[serde(default)]
    pub reaper: ReaperConfig,
    #[serde(default)]
    pub kanban: KanbanConfig,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct ReaperConfig {
    pub enabled: bool,
    pub threshold_hours: i64,
}

impl Default for ReaperConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            threshold_hours: 3,
        }
    }
}

/// Kanban mode configuration. Lives under `[kanban]` in config.toml.
#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct KanbanConfig {
    /// Switch to flat view when fewer than this many sessions are active
    /// (set to 0 to always use kanban when tasks exist).
    pub auto_flat_threshold: usize,
    /// Minutes a task may sit in `review` before a block notification fires.
    pub block_alert_minutes: u32,
    /// Skip the review column: move tasks directly from running → done on
    /// Stop hook (Cline Kanban style auto pipeline).
    pub auto_approve: bool,
    /// `terminal-notifier -sound` argument (e.g. "Basso").
    pub block_alert_sound: String,
    /// Cooldown (seconds) between repeated block alerts for the same task.
    /// `0` means alert once per review stint (dedupe via `block_alerted_at`).
    pub block_alert_cooldown_secs: u64,
}

impl Default for KanbanConfig {
    fn default() -> Self {
        Self {
            auto_flat_threshold: 3,
            block_alert_minutes: 5,
            auto_approve: false,
            block_alert_sound: "Basso".to_string(),
            block_alert_cooldown_secs: 0,
        }
    }
}

impl Default for AppConfig {
    fn default() -> Self {
        Self {
            backend: "wezterm".to_string(),
            terminal_path: String::new(),
            wezterm_path: String::new(),
            stale_threshold_mins: 30,
            data_dir: "~/.config/wez-sidebar".to_string(),
            reaper: ReaperConfig::default(),
            kanban: KanbanConfig::default(),
        }
    }
}

impl AppConfig {
    /// Resolve the effective terminal_path (terminal_path > wezterm_path > auto-detect)
    pub fn effective_terminal_path(&self) -> &str {
        if !self.terminal_path.is_empty() {
            &self.terminal_path
        } else if !self.wezterm_path.is_empty() {
            &self.wezterm_path
        } else {
            ""
        }
    }
}

pub fn expand_tilde(path: &str) -> PathBuf {
    if let Some(rest) = path.strip_prefix("~/") {
        if let Some(home) = dirs::home_dir() {
            return home.join(rest);
        }
    }
    PathBuf::from(path)
}

pub fn load_config() -> AppConfig {
    let config_path = dirs::home_dir()
        .unwrap_or_default()
        .join(".config/wez-sidebar/config.toml");

    match fs::read_to_string(&config_path) {
        Ok(content) => toml::from_str(&content).unwrap_or_default(),
        Err(_) => AppConfig::default(),
    }
}
