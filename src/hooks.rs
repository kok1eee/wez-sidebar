use anyhow::Result;
use chrono::Utc;
use std::{
    io::{self, Read as _},
    process::Command,
};

use crate::config::AppConfig;
use crate::session::{read_session_store, send_permission_notification, write_session_store};
use crate::types::{HookPayload, Session, SessionTask};
use crate::usage::cache_usage_if_stale;

pub fn handle_hook(event_name: &str, config: &AppConfig) -> Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;

    handle_hook_inner(event_name, config, &input)?;

    println!("{{}}");
    Ok(())
}

fn handle_hook_inner(event_name: &str, config: &AppConfig, input: &str) -> Result<()> {
    let payload: HookPayload = match serde_json::from_str(input) {
        Ok(p) => p,
        Err(_) => return Ok(()),
    };

    if payload.session_id.is_empty() {
        return Ok(());
    }

    // Detect TTY and yolo mode from ancestors (single walk)
    let (tty, is_yolo) = get_tty_and_yolo_from_ancestors();

    // Extract activity and danger flag from hook payload
    let activity = extract_activity(event_name, &payload);
    let is_dangerous = event_name == "PreToolUse" && detect_dangerous(&payload);

    // Extract user message from UserPromptSubmit
    let user_message = if event_name == "UserPromptSubmit" {
        payload.prompt.as_deref().map(|p| {
            let trimmed = p.trim();
            if trimmed.chars().count() > 200 {
                let end = trimmed.char_indices().nth(200).map(|(i, _)| i).unwrap_or(trimmed.len());
                format!("{}…", &trimmed[..end])
            } else {
                trimmed.to_string()
            }
        }).filter(|s| !s.is_empty())
    } else {
        None
    };

    // Extract tasks from TaskCreate/TaskUpdate
    let existing_tasks = {
        let store = read_session_store(&config.data_dir);
        store.sessions.get(&payload.session_id)
            .map(|s| s.tasks.clone())
            .unwrap_or_default()
    };
    let tasks = extract_tasks(event_name, &payload, &existing_tasks);

    // Update session
    let cwd = payload.cwd.unwrap_or_default();
    let git_branch = resolve_git_branch(&cwd);
    let notification_type = payload.notification_type.as_deref();
    let new_status = update_session(
        event_name,
        &payload.session_id,
        &cwd,
        &tty,
        notification_type,
        is_yolo,
        activity,
        is_dangerous,
        git_branch,
        user_message,
        tasks,
        &config.data_dir,
    )?;

    // Desktop notification on permission prompt
    if new_status == "waiting_input" {
        send_permission_notification(&cwd, &tty, &config.wezterm_path);
    }

    // Usage cache: 10分クールダウンで API 取得 → キャッシュファイル書き出し
    cache_usage_if_stale(&config.data_dir);

    Ok(())
}

/// Show last 2 path components: "src/config.rs" instead of just "config.rs"
fn short_path(p: &str) -> String {
    let path = std::path::Path::new(p);
    let components: Vec<_> = path.components().rev().take(2).collect();
    let parts: Vec<&str> = components.iter().rev().filter_map(|c| c.as_os_str().to_str()).collect();
    parts.join("/")
}

fn extract_activity(event_name: &str, payload: &HookPayload) -> Option<String> {
    if event_name != "PreToolUse" {
        return None;
    }

    let tool = payload.tool_name.as_deref()?;
    let input = payload.tool_input.as_ref();

    let detail = match tool {
        "Bash" => input
            .and_then(|v| v.get("command"))
            .and_then(|v| v.as_str())
            .map(|cmd| {
                let short = cmd.split_whitespace().take(6).collect::<Vec<_>>().join(" ");
                if short.chars().count() > 60 {
                    let end = short.char_indices().nth(60).map(|(i, _)| i).unwrap_or(short.len());
                    format!("{}…", &short[..end])
                } else {
                    short
                }
            }),
        "Read" | "Write" | "Edit" => input
            .and_then(|v| v.get("file_path"))
            .and_then(|v| v.as_str())
            .map(short_path),
        "Grep" => input
            .and_then(|v| v.get("pattern"))
            .and_then(|v| v.as_str())
            .map(|p| format!("/{}/", p)),
        "Glob" => input
            .and_then(|v| v.get("pattern"))
            .and_then(|v| v.as_str())
            .map(String::from),
        "Agent" => input
            .and_then(|v| v.get("description"))
            .and_then(|v| v.as_str())
            .map(String::from),
        _ => None,
    };

    match detail {
        Some(d) => Some(format!("{} {}", tool, d)),
        None => Some(tool.to_string()),
    }
}

fn extract_tasks(
    event_name: &str,
    payload: &HookPayload,
    existing_tasks: &[SessionTask],
) -> Option<Vec<SessionTask>> {
    let tool = payload.tool_name.as_deref()?;

    match (event_name, tool) {
        // PreToolUse TaskCreate: add with placeholder ID (will be replaced by PostToolUse)
        ("PreToolUse", "TaskCreate") => {
            let input = payload.tool_input.as_ref()?;
            let subject = input.get("subject")?.as_str()?;
            let mut tasks = existing_tasks.to_vec();
            tasks.push(SessionTask {
                id: String::new(), // placeholder — PostToolUse will fill the real ID
                content: subject.to_string(),
                status: "pending".to_string(),
            });
            Some(tasks)
        }
        // PostToolUse TaskCreate: fill in real ID from tool_response
        ("PostToolUse", "TaskCreate") => {
            let resp = payload.tool_response.as_ref()?;
            let real_id = resp.get("task")?.get("id")?.as_str()?;
            let mut tasks = existing_tasks.to_vec();
            // Update the last task with empty ID
            if let Some(task) = tasks.iter_mut().rev().find(|t| t.id.is_empty()) {
                task.id = real_id.to_string();
                Some(tasks)
            } else {
                None
            }
        }
        // PreToolUse TaskUpdate: update status/subject by ID
        ("PreToolUse", "TaskUpdate") => {
            let input = payload.tool_input.as_ref()?;
            let task_id = input.get("taskId")?.as_str()?;
            let new_status = input.get("status").and_then(|v| v.as_str());
            let new_subject = input.get("subject").and_then(|v| v.as_str());

            if new_status.is_none() && new_subject.is_none() {
                return None;
            }

            let mut tasks = existing_tasks.to_vec();

            // Handle deletion
            if new_status == Some("deleted") {
                let before = tasks.len();
                tasks.retain(|t| t.id != task_id);
                return if tasks.len() != before { Some(tasks) } else { None };
            }

            if let Some(task) = tasks.iter_mut().find(|t| t.id == task_id) {
                if let Some(s) = new_status {
                    task.status = s.to_string();
                }
                if let Some(s) = new_subject {
                    task.content = s.to_string();
                }
                Some(tasks)
            } else {
                None
            }
        }
        _ => None,
    }
}

fn detect_dangerous(payload: &HookPayload) -> bool {
    let tool = match payload.tool_name.as_deref() {
        Some(t) => t,
        None => return false,
    };
    let input = match payload.tool_input.as_ref() {
        Some(v) => v,
        None => return false,
    };

    match tool {
        "Bash" => {
            if let Some(cmd) = input.get("command").and_then(|v| v.as_str()) {
                let cmd_lower = cmd.to_lowercase();
                cmd_lower.contains("rm -rf")
                    || cmd_lower.contains("git push --force")
                    || cmd_lower.contains("git push -f")
                    || cmd_lower.contains("git reset --hard")
                    || cmd_lower.contains("git clean -f")
                    || cmd_lower.contains("git checkout .")
                    || cmd_lower.contains("drop table")
                    || cmd_lower.contains("drop database")
                    || cmd_lower.contains("truncate table")
                    || cmd_lower.contains("> /dev/")
                    || cmd_lower.contains("mkfs")
                    || cmd_lower.contains("dd if=")
            } else {
                false
            }
        }
        _ => false,
    }
}

fn resolve_git_branch(cwd: &str) -> Option<String> {
    if cwd.is_empty() {
        return None;
    }
    Command::new("git")
        .args(["-C", cwd, "branch", "--show-current"])
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Walk the ancestor process chain once, collecting TTY and yolo mode in a single pass.
/// Returns `(tty, is_yolo)`. Uses a single `ps -o tty=,ppid=,args=` per level: max 5 calls total.
fn get_tty_and_yolo_from_ancestors() -> (String, bool) {
    let mut ppid = std::os::unix::process::parent_id() as i32;
    let mut found_tty = String::new();
    let mut is_yolo = false;

    for _ in 0..5 {
        let Ok(out) = Command::new("ps")
            .args(["-o", "tty=,ppid=,args=", "-p", &ppid.to_string()])
            .output()
        else {
            break;
        };

        let line = String::from_utf8_lossy(&out.stdout);
        let line = line.trim();
        if line.is_empty() {
            break;
        }

        // Format: "tty ppid rest-of-args..."
        let mut parts = line.splitn(3, char::is_whitespace);
        let tty_field = parts.next().unwrap_or("").trim();
        let ppid_field = parts.next().unwrap_or("").trim();
        let args_field = parts.next().unwrap_or("");

        if found_tty.is_empty() && !tty_field.is_empty() && tty_field != "??" {
            found_tty = format!("/dev/{}", tty_field);
        }

        if !is_yolo && args_field.contains("--dangerously-skip-permissions") {
            is_yolo = true;
        }

        if !found_tty.is_empty() && is_yolo {
            break;
        }

        match ppid_field.parse::<i32>() {
            Ok(new_ppid) => ppid = new_ppid,
            Err(_) => break,
        }
    }

    (found_tty, is_yolo)
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

#[allow(clippy::too_many_arguments)]
pub fn update_session(
    event_name: &str,
    session_id: &str,
    cwd: &str,
    tty: &str,
    notification_type: Option<&str>,
    is_yolo: bool,
    activity: Option<String>,
    is_dangerous: bool,
    git_branch: Option<String>,
    user_message: Option<String>,
    tasks: Option<Vec<SessionTask>>,
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

    let new_status = determine_status(event_name, notification_type, current_status);

    // Preserve previous activity if this event doesn't have one
    let last_activity = activity.or_else(|| {
        existing.and_then(|s| s.last_activity.clone())
    });

    // Danger flag: set on dangerous PreToolUse, clear on UserPromptSubmit (user approved)
    let final_dangerous = if event_name == "UserPromptSubmit" {
        false
    } else if is_dangerous {
        true
    } else {
        existing.map(|s| s.is_dangerous).unwrap_or(false)
    };

    // Git branch: preserve previous if not resolved this time
    let final_branch = git_branch.or_else(|| {
        existing.and_then(|s| s.git_branch.clone())
    });

    // User message: update on UserPromptSubmit, preserve otherwise
    let final_user_message = user_message.or_else(|| {
        existing.and_then(|s| s.last_user_message.clone())
    });

    // User message timestamp: set on UserPromptSubmit, preserve otherwise
    let final_user_message_at = if event_name == "UserPromptSubmit" {
        Some(now.clone())
    } else {
        existing.and_then(|s| s.last_user_message_at.clone())
    };

    // Tasks: update on TodoWrite, preserve otherwise
    let final_tasks = tasks.unwrap_or_else(|| {
        existing.map(|s| s.tasks.clone()).unwrap_or_default()
    });

    store.sessions.insert(
        session_id.to_string(),
        Session {
            session_id: session_id.to_string(),
            home_cwd,
            tty: final_tty,
            status: new_status.clone(),
            created_at,
            updated_at: now.clone(),
            is_yolo,
            last_activity,
            is_dangerous: final_dangerous,
            git_branch: final_branch,
            last_user_message: final_user_message,
            last_user_message_at: final_user_message_at,
            tasks: final_tasks,
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
            is_yolo: false,
            last_activity: None,
            is_dangerous: false,
            git_branch: None,
            last_user_message: None,
            last_user_message_at: None,
            tasks: Vec::new(),
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
            None,
            false,
            None,
            None,
            None,
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
            None,
            false,
            None,
            None,
            None,
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
            None,
            false,
            None,
            None,
            None,
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
            None,
            false,
            None,
            None,
            None,
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
            None,
            false,
            None,
            None,
            None,
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
            None,
            false,
            None,
            None,
            None,
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
            None,
            false,
            None,
            None,
            None,
            dir.path().to_str().unwrap(),
        )
        .unwrap();

        let store = read_session_store(dir.path().to_str().unwrap());
        assert!(store.sessions.contains_key("existing"));
        assert!(store.sessions.contains_key("new-session"));
    }
}
