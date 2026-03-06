use anyhow::Result;
use chrono::Utc;
use serde::Deserialize;
use std::{
    fs,
    io::{self, Read as _},
    path::PathBuf,
    process::Command,
};

use crate::config::AppConfig;
use crate::session::{read_session_store, send_permission_notification, write_session_store};
use crate::types::{HookPayload, Session};

pub fn handle_hook(event_name: &str, config: &AppConfig) -> Result<()> {
    // 1. Read stdin once
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;

    // 2. Always run built-in handler (updates sessions.json)
    builtin_handle_hook(event_name, config, &input)?;

    // 3. Optionally delegate to external command
    if let Some(ref cmd) = config.hook_command {
        let mut child = Command::new("sh")
            .args(["-c", &format!("{} {}", cmd, event_name)])
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .spawn()?;

        if let Some(ref mut stdin) = child.stdin {
            use std::io::Write;
            let _ = stdin.write_all(input.as_bytes());
        }

        let output = child.wait_with_output()?;
        let stdout = String::from_utf8_lossy(&output.stdout);
        print!("{}", stdout);
    } else {
        println!("{{}}");
    }

    Ok(())
}

fn builtin_handle_hook(event_name: &str, config: &AppConfig, input: &str) -> Result<()> {
    let payload: HookPayload = match serde_json::from_str(input) {
        Ok(p) => p,
        Err(_) => return Ok(()),
    };

    if payload.session_id.is_empty() {
        return Ok(());
    }

    // Detect TTY and yolo mode from ancestors
    let tty = get_tty_from_ancestors();
    let is_yolo = detect_yolo_mode();

    // Update session
    let cwd = payload.cwd.unwrap_or_default();
    let notification_type = payload.notification_type.as_deref();
    let new_status = update_session(
        event_name,
        &payload.session_id,
        &cwd,
        &tty,
        notification_type,
        is_yolo,
        &config.data_dir,
    )?;

    // Desktop notification on permission prompt
    // Skip when hook_command is set (external command handles its own notifications)
    if new_status == "waiting_input" && config.hook_command.is_none() {
        send_permission_notification(&cwd, &tty, &config.wezterm_path);
    }

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

fn detect_yolo_mode() -> bool {
    let mut ppid = std::os::unix::process::parent_id() as i32;

    for _ in 0..5 {
        let output = Command::new("ps")
            .args(["-o", "args=", "-p", &ppid.to_string()])
            .output();

        if let Ok(out) = output {
            let args = String::from_utf8_lossy(&out.stdout);
            if args.contains("--dangerously-skip-permissions") {
                return true;
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

    false
}

pub fn determine_status(
    event_name: &str,
    notification_type: Option<&str>,
    current_status: &str,
) -> String {
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

fn read_claude_tasks(session_id: &str) -> (Option<String>, i32, i32) {
    let tasks_dir = dirs::home_dir()
        .unwrap_or_else(|| PathBuf::from(std::env::var("HOME").unwrap_or_default()))
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
        if entry
            .path()
            .extension()
            .map(|e| e == "json")
            .unwrap_or(false)
        {
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

pub fn update_session(
    event_name: &str,
    session_id: &str,
    cwd: &str,
    tty: &str,
    notification_type: Option<&str>,
    is_yolo: bool,
    data_dir: &str,
) -> Result<String> {
    let mut store = read_session_store(data_dir);
    let now_utc = Utc::now();
    let now = now_utc.to_rfc3339();

    // TTY deduplication: remove entries with same TTY but different session_id
    if !tty.is_empty() {
        store
            .sessions
            .retain(|k, s| s.tty != tty || k == session_id);
    }

    // Auto-cleanup: remove stopped sessions older than 24h
    store.sessions.retain(|_, s| {
        if s.status != "stopped" {
            return true;
        }
        if let Ok(dt) = chrono::DateTime::parse_from_rfc3339(&s.updated_at) {
            let age = now_utc.signed_duration_since(dt.with_timezone(&Utc));
            age < chrono::Duration::hours(24)
        } else {
            true
        }
    });

    let existing = store.sessions.get(session_id);
    let current_status = existing.map(|s| s.status.as_str()).unwrap_or("");
    let created_at = existing
        .map(|s| s.created_at.clone())
        .unwrap_or_else(|| now.clone());
    let home_cwd = cwd.to_string();
    let final_tty = existing
        .and_then(|s| {
            if s.tty.is_empty() {
                None
            } else {
                Some(s.tty.clone())
            }
        })
        .unwrap_or_else(|| tty.to_string());

    let (active_task, tasks_completed, tasks_total) = read_claude_tasks(session_id);
    let new_status = determine_status(event_name, notification_type, current_status);

    store.sessions.insert(
        session_id.to_string(),
        Session {
            session_id: session_id.to_string(),
            home_cwd,
            tty: final_tty,
            status: new_status.clone(),
            created_at,
            updated_at: now.clone(),
            active_task,
            tasks_completed,
            tasks_total,
            is_yolo,
        },
    );

    store.updated_at = now;
    write_session_store(&store, data_dir)?;
    Ok(new_status)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{Session, SessionsFile};
    use std::collections::HashMap;

    /// Helper: create a Session with given fields (defaults for the rest)
    fn make_session(
        session_id: &str,
        tty: &str,
        status: &str,
        updated_at: &str,
    ) -> Session {
        Session {
            session_id: session_id.to_string(),
            home_cwd: "/tmp/test".to_string(),
            tty: tty.to_string(),
            status: status.to_string(),
            created_at: updated_at.to_string(),
            updated_at: updated_at.to_string(),
            active_task: None,
            tasks_completed: 0,
            tasks_total: 0,
            is_yolo: false,
        }
    }

    /// Helper: write a SessionsFile to a temp dir and return the dir path
    fn setup_store(sessions: Vec<(&str, Session)>) -> tempfile::TempDir {
        let dir = tempfile::tempdir().unwrap();
        let mut map = HashMap::new();
        for (key, sess) in sessions {
            map.insert(key.to_string(), sess);
        }
        let store = SessionsFile {
            sessions: map,
            updated_at: Utc::now().to_rfc3339(),
        };
        let path = dir.path().join("sessions.json");
        let data = serde_json::to_string_pretty(&store).unwrap();
        std::fs::write(path, data).unwrap();
        dir
    }

    // ====================================================================
    // determine_status tests
    // ====================================================================

    #[test]
    fn test_determine_status_stop_event() {
        assert_eq!(determine_status("Stop", None, "running"), "stopped");
        assert_eq!(determine_status("Stop", None, "waiting_input"), "stopped");
        assert_eq!(determine_status("Stop", None, ""), "stopped");
    }

    #[test]
    fn test_determine_status_user_prompt_submit() {
        assert_eq!(
            determine_status("UserPromptSubmit", None, "stopped"),
            "running"
        );
        assert_eq!(
            determine_status("UserPromptSubmit", None, "running"),
            "running"
        );
        assert_eq!(
            determine_status("UserPromptSubmit", None, "waiting_input"),
            "running"
        );
    }

    #[test]
    fn test_determine_status_pre_tool_use() {
        assert_eq!(determine_status("PreToolUse", None, "running"), "running");
        assert_eq!(
            determine_status("PreToolUse", None, "waiting_input"),
            "running"
        );
    }

    #[test]
    fn test_determine_status_stopped_stays_stopped() {
        assert_eq!(determine_status("PreToolUse", None, "stopped"), "stopped");
        assert_eq!(
            determine_status("Notification", None, "stopped"),
            "stopped"
        );
        assert_eq!(
            determine_status("PostToolUse", None, "stopped"),
            "stopped"
        );
    }

    #[test]
    fn test_determine_status_permission_prompt_notification() {
        assert_eq!(
            determine_status("Notification", Some("permission_prompt"), "running"),
            "waiting_input"
        );
    }

    #[test]
    fn test_determine_status_other_notification() {
        assert_eq!(
            determine_status("Notification", Some("other"), "running"),
            "running"
        );
        assert_eq!(determine_status("Notification", None, "running"), "running");
    }

    #[test]
    fn test_determine_status_unknown_event() {
        assert_eq!(determine_status("PostToolUse", None, "running"), "running");
        assert_eq!(determine_status("Unknown", None, "running"), "running");
    }

    // ====================================================================
    // update_session tests (TTY dedup, 24h cleanup, status)
    // ====================================================================

    #[test]
    fn test_update_session_tty_dedup() {
        let now = Utc::now().to_rfc3339();
        let dir = setup_store(vec![
            ("old-session", make_session("old-session", "/dev/ttys001", "running", &now)),
        ]);

        let result = update_session(
            "UserPromptSubmit",
            "new-session",
            "/tmp/project",
            "/dev/ttys001",
            None,
            false,
            dir.path().to_str().unwrap(),
        )
        .unwrap();

        assert_eq!(result, "running");

        let store = read_session_store(dir.path().to_str().unwrap());
        assert!(!store.sessions.contains_key("old-session"));
        assert!(store.sessions.contains_key("new-session"));
        assert_eq!(store.sessions["new-session"].tty, "/dev/ttys001");
    }

    #[test]
    fn test_update_session_24h_cleanup() {
        let old_time = (Utc::now() - chrono::Duration::hours(25)).to_rfc3339();
        let recent_time = (Utc::now() - chrono::Duration::hours(1)).to_rfc3339();

        let dir = setup_store(vec![
            ("old-stopped", make_session("old-stopped", "/dev/ttys002", "stopped", &old_time)),
            ("recent-stopped", make_session("recent-stopped", "/dev/ttys003", "stopped", &recent_time)),
            ("old-running", make_session("old-running", "/dev/ttys004", "running", &old_time)),
        ]);

        let _ = update_session(
            "UserPromptSubmit",
            "new-session",
            "/tmp/project",
            "/dev/ttys005",
            None,
            false,
            dir.path().to_str().unwrap(),
        )
        .unwrap();

        let store = read_session_store(dir.path().to_str().unwrap());
        assert!(!store.sessions.contains_key("old-stopped"));
        assert!(store.sessions.contains_key("recent-stopped"));
        assert!(store.sessions.contains_key("old-running"));
        assert!(store.sessions.contains_key("new-session"));
    }

    #[test]
    fn test_update_session_returns_correct_status() {
        let dir = setup_store(vec![]);

        let status = update_session(
            "UserPromptSubmit",
            "sess-1",
            "/tmp/proj",
            "/dev/ttys001",
            None,
            false,
            dir.path().to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(status, "running");

        let status = update_session(
            "Stop",
            "sess-1",
            "/tmp/proj",
            "/dev/ttys001",
            None,
            false,
            dir.path().to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(status, "stopped");

        let status = update_session(
            "PreToolUse",
            "sess-1",
            "/tmp/proj",
            "/dev/ttys001",
            None,
            false,
            dir.path().to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(status, "stopped");

        let status = update_session(
            "UserPromptSubmit",
            "sess-1",
            "/tmp/proj",
            "/dev/ttys001",
            None,
            false,
            dir.path().to_str().unwrap(),
        )
        .unwrap();
        assert_eq!(status, "running");
    }

    #[test]
    fn test_update_session_empty_tty_no_dedup() {
        let now = Utc::now().to_rfc3339();
        let dir = setup_store(vec![
            ("existing", make_session("existing", "/dev/ttys001", "running", &now)),
        ]);

        let _ = update_session(
            "UserPromptSubmit",
            "new-session",
            "/tmp/project",
            "",
            None,
            false,
            dir.path().to_str().unwrap(),
        )
        .unwrap();

        let store = read_session_store(dir.path().to_str().unwrap());
        assert!(store.sessions.contains_key("existing"));
        assert!(store.sessions.contains_key("new-session"));
    }
}
