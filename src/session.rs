use anyhow::Result;
use chrono::{DateTime, Utc};
use std::{collections::HashMap, fs, path::PathBuf, process::Command};

use crate::config::{expand_tilde, AppConfig};
use crate::types::{SessionItem, SessionsFile, WezTermPane};

pub fn get_sessions_file_path(data_dir: &str) -> PathBuf {
    expand_tilde(data_dir).join("sessions.json")
}

pub fn read_session_store(data_dir: &str) -> SessionsFile {
    let path = get_sessions_file_path(data_dir);
    match fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => SessionsFile::default(),
    }
}

pub fn write_session_store(store: &SessionsFile, data_dir: &str) -> Result<()> {
    let path = get_sessions_file_path(data_dir);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let data = serde_json::to_string_pretty(store)?;
    fs::write(path, data)?;
    Ok(())
}

pub fn get_wezterm_panes(wezterm_path: &str) -> Vec<WezTermPane> {
    let output = Command::new(wezterm_path)
        .args(["cli", "list", "--format", "json"])
        .output();

    match output {
        Ok(out) => serde_json::from_slice(&out.stdout).unwrap_or_default(),
        Err(_) => Vec::new(),
    }
}

pub fn find_wezterm_pane_by_tty(tty: &str, wezterm_path: &str) -> Option<(i32, i32)> {
    if tty.is_empty() {
        return None;
    }
    let panes = get_wezterm_panes(wezterm_path);
    panes
        .iter()
        .find(|p| p.tty_name == tty)
        .map(|p| (p.tab_id, p.pane_id))
}

pub fn send_permission_notification(cwd: &str, tty: &str, wezterm_path: &str) {
    // terminal-notifier が存在しなければスキップ
    let notifier = match Command::new("which")
        .arg("terminal-notifier")
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
    {
        Some(path) if !path.is_empty() => path,
        _ => return,
    };

    let dir_name = std::path::Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let (activate_cmd, approve_cmd) = match find_wezterm_pane_by_tty(tty, wezterm_path) {
        Some((tab_id, pane_id)) => {
            let activate = format!(
                "{} cli activate-tab --tab-id {} && {} cli activate-pane --pane-id {}",
                wezterm_path, tab_id, wezterm_path, pane_id
            );
            let approve = format!(
                "{} && {} cli send-text --pane-id {} --no-paste $'\\n'",
                activate, wezterm_path, pane_id
            );
            (activate, approve)
        }
        None => (
            "open -a WezTerm".to_string(),
            "open -a WezTerm".to_string(),
        ),
    };

    let script = format!(
        r#"result=$({} -title 'Claude Code' -message '許可待ち: {}' -sound Tink -actions '承認' -sender com.github.wez.wezterm); if [ "$result" = "@ACTIONCLICKED" ]; then {}; elif [ "$result" = "@CONTENTCLICKED" ]; then {}; fi"#,
        notifier, dir_name, approve_cmd, activate_cmd
    );

    let _ = Command::new("bash").args(["-c", &script]).spawn();
}

pub fn load_sessions_data(config: &AppConfig) -> Vec<SessionItem> {
    let panes = get_wezterm_panes(&config.wezterm_path);
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

    let store = read_session_store(&config.data_dir);
    let now = Utc::now();
    let stale_threshold = chrono::Duration::minutes(config.stale_threshold_mins);

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
                is_yolo: sess.is_yolo,
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
                    is_yolo: sess.is_yolo,
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

pub fn get_pane_text(pane_id: i32, wezterm_path: &str) -> Vec<String> {
    if pane_id < 0 {
        return vec!["(disconnected)".to_string()];
    }

    let output = Command::new(wezterm_path)
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

pub fn activate_pane(session: &SessionItem, wezterm_path: &str) {
    if session.is_disconnected {
        return;
    }

    let _ = Command::new(wezterm_path)
        .args(["cli", "activate-tab", "--tab-id", &session.tab_id.to_string()])
        .output();

    let _ = Command::new(wezterm_path)
        .args([
            "cli",
            "activate-pane",
            "--pane-id",
            &session.pane_id.to_string(),
        ])
        .output();
}

pub fn delete_session(session: &SessionItem, data_dir: &str) {
    let mut store = read_session_store(data_dir);
    store.sessions.remove(&session.session_id);
    let _ = write_session_store(&store, data_dir);
}
