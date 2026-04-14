use anyhow::Result;
use chrono::{DateTime, Utc};
use std::{collections::HashMap, fs, path::PathBuf};

use crate::config::{expand_tilde, AppConfig};
use crate::terminal::{TerminalBackend, TerminalPane};
use crate::types::{SessionItem, SessionsFile};

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

    // Atomic write: write to PID-unique temp file then rename to avoid corruption from concurrent hooks
    let tmp_path = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&tmp_path, data)?;
    fs::rename(&tmp_path, &path)?;
    Ok(())
}

pub fn load_sessions_data(config: &AppConfig, backend: &dyn TerminalBackend) -> Vec<SessionItem> {
    let panes = backend.list_panes();
    if panes.is_empty() {
        return Vec::new();
    }

    // Build TTY to pane map
    let mut tty_to_pane: HashMap<String, &TerminalPane> = HashMap::new();
    let own_pane_id = backend.current_pane_id();
    let mut current_window_id = -1;
    let mut current_pane_id = -1;

    for pane in &panes {
        tty_to_pane.insert(pane.tty_name.clone(), pane);
        if own_pane_id >= 0 && pane.pane_id == own_pane_id {
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

    for sess in store.sessions.values() {
        // Skip subagent sessions (no TTY) — they never fire Stop hooks
        if sess.tty.is_empty() {
            continue;
        }
        // Match by TTY, then verify pane_id (prevents TTY reuse false match)
        let pane = tty_to_pane.get(&sess.tty).filter(|p| {
            match sess.pane_id {
                Some(recorded_pane_id) => p.pane_id == recorded_pane_id,
                // No pane_id recorded: trust TTY only for non-stopped sessions
                // (stopped sessions won't fire hooks to record pane_id, so TTY reuse is likely)
                None => sess.status != "stopped",
            }
        });
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

        // Count subagents active within the last 60 seconds
        let active_subagents = sess.subagents.iter().filter(|e| {
            DateTime::parse_from_rfc3339(&e.last_seen)
                .map(|dt| now.signed_duration_since(dt.with_timezone(&Utc)) < chrono::Duration::seconds(60))
                .unwrap_or(false)
        }).count();

        if let Some(pane) = pane {
            if pane.window_id != current_window_id {
                continue;
            }
            let last_user_message_at = sess.last_user_message_at.as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                .map(|dt| dt.with_timezone(&Utc));
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
                permission_mode: if !sess.permission_mode.is_empty() {
                    sess.permission_mode.clone()
                } else if sess.is_yolo {
                    "yolo".to_string()
                } else {
                    "normal".to_string()
                },
                last_activity: sess.last_activity.clone(),
                is_dangerous: sess.is_dangerous,
                git_branch: sess.git_branch.clone(),
                home_cwd: sess.home_cwd.clone(),
                last_user_message: sess.last_user_message.clone(),
                last_user_message_at,
                tasks: sess.tasks.clone(),
                active_subagents,
                context_percent: sess.context_percent,
            });
        } else {
            // Disconnected session
            let age = now.signed_duration_since(updated_at);
            if age <= chrono::Duration::hours(24) {
                let last_user_message_at = sess.last_user_message_at.as_deref()
                    .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
                    .map(|dt| dt.with_timezone(&Utc));
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
                    permission_mode: if !sess.permission_mode.is_empty() {
                        sess.permission_mode.clone()
                    } else if sess.is_yolo {
                        "yolo".to_string()
                    } else {
                        "normal".to_string()
                    },
                    last_activity: sess.last_activity.clone(),
                    is_dangerous: sess.is_dangerous,
                    git_branch: sess.git_branch.clone(),
                    home_cwd: sess.home_cwd.clone(),
                    last_user_message: sess.last_user_message.clone(),
                    last_user_message_at,
                    tasks: sess.tasks.clone(),
                    active_subagents,
                    context_percent: sess.context_percent,
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

pub fn activate_pane(session: &SessionItem, backend: &dyn TerminalBackend) {
    if session.is_disconnected {
        return;
    }
    backend.activate_pane(session.tab_id, session.pane_id);
}

pub fn delete_session(session: &SessionItem, data_dir: &str) {
    let mut store = read_session_store(data_dir);
    store.sessions.remove(&session.session_id);
    let _ = write_session_store(&store, data_dir);
}
