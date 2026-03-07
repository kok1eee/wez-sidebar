use serde::Deserialize;
use std::{fs, path::PathBuf, process::Command};

#[derive(Debug, Clone, Deserialize)]
#[serde(default)]
pub struct AppConfig {
    pub wezterm_path: String,
    pub stale_threshold_mins: i64,
    pub data_dir: String,
    /// External hook command (uses built-in session handler if omitted)
    pub hook_command: Option<String>,
    /// Tasks cache file path (no tasks shown if omitted)
    pub tasks_file: Option<String>,
    /// REST API base URL (e.g., "http://ec2-host:3000")
    pub api_url: Option<String>,
}

impl Default for AppConfig {
    fn default() -> Self {
        let wezterm_path = Command::new("which")
            .arg("wezterm")
            .output()
            .ok()
            .and_then(|o| {
                if o.status.success() {
                    Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| "wezterm".to_string());

        Self {
            wezterm_path,
            stale_threshold_mins: 30,
            data_dir: "~/.config/wez-sidebar".to_string(),
            hook_command: None,
            tasks_file: None,
            api_url: None,
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
