use anyhow::Result;
use chrono::Utc;
use std::{
    io::{self, Read as _},
    process::{Command, Stdio},
};

use crate::config::AppConfig;
use crate::notify::send_permission_notification;
use crate::session::{read_session_store, write_session_store};
use crate::terminal::create_backend;
use crate::types::{HookPayload, Session, SessionTask};

pub fn handle_hook(event_name: &str, config: &AppConfig) -> Result<()> {
    let mut input = String::new();
    io::stdin().read_to_string(&mut input)?;

    // Swallow errors so we always output {} (prevents Claude Code "hook error")
    let _ = handle_hook_inner(event_name, config, &input);

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

    // Detect TTY and permission mode from ancestors (single walk)
    let (tty, permission_mode) = get_tty_and_permission_from_ancestors();

    // Subagent detection:
    // 1. No TTY → headless subagent (e.g. background agent)
    // 2. TTY exists but already belongs to a different running session → teammate/subagent
    let is_subagent = if tty.is_empty() {
        true
    } else {
        let store = read_session_store(&config.data_dir);
        store.sessions.values().any(|s| {
            s.tty == tty && s.session_id != payload.session_id && s.status == "running"
        })
    };

    if is_subagent {
        return track_subagent(&payload, &config.data_dir);
    }

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

    // Resolve pane_id from TTY
    let backend = create_backend(&config.backend, config.effective_terminal_path());
    let pane_id = backend.find_pane_by_tty(&tty).map(|(_, pid)| pid);

    // Update session
    let cwd = payload.cwd.clone().unwrap_or_default();
    let git_branch = resolve_git_branch(&cwd);
    let notification_type = payload.notification_type.as_deref();
    let new_status = update_session(
        event_name,
        &payload.session_id,
        &cwd,
        &tty,
        pane_id,
        notification_type,
        &permission_mode,
        activity,
        is_dangerous,
        git_branch,
        user_message,
        &payload,
        &config.data_dir,
    )?;

    // Desktop notification on permission prompt
    if new_status == "waiting_input" {
        send_permission_notification(&cwd, &tty, backend.as_ref());
    }

    // Context window usage: read from Claude Code JSONL
    if let Some(pct) = read_context_percent(&payload.session_id, &cwd) {
        let mut store = read_session_store(&config.data_dir);
        if let Some(sess) = store.sessions.get_mut(&payload.session_id) {
            sess.context_percent = Some(pct);
            let _ = write_session_store(&store, &config.data_dir);
        }
    }

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

/// Apply task mutations from TaskCreate (PostToolUse) or TaskUpdate (PreToolUse).
/// Called inside update_session with existing tasks from the store — no extra file read needed.
fn apply_task_event(
    event_name: &str,
    payload: &HookPayload,
    existing_tasks: &mut Vec<SessionTask>,
) -> bool {
    let tool = match payload.tool_name.as_deref() {
        Some(t) => t,
        None => return false,
    };

    match (event_name, tool) {
        // PostToolUse TaskCreate: input has subject, response has real ID
        ("PostToolUse", "TaskCreate") => {
            let input = match payload.tool_input.as_ref() {
                Some(v) => v,
                None => return false,
            };
            let resp = match payload.tool_response.as_ref() {
                Some(v) => v,
                None => return false,
            };
            let subject = match input.get("subject").and_then(|v| v.as_str()) {
                Some(s) => s,
                None => return false,
            };
            let id = resp
                .get("task")
                .and_then(|t| t.get("id"))
                .and_then(|v| v.as_str())
                .unwrap_or("");
            existing_tasks.push(SessionTask {
                id: id.to_string(),
                content: subject.to_string(),
                status: "pending".to_string(),
            });
            true
        }
        // PreToolUse TaskUpdate: update status/subject by ID
        ("PreToolUse", "TaskUpdate") => {
            let input = match payload.tool_input.as_ref() {
                Some(v) => v,
                None => return false,
            };
            let task_id = match input.get("taskId").and_then(|v| v.as_str()) {
                Some(id) => id,
                None => return false,
            };
            let new_status = input.get("status").and_then(|v| v.as_str());
            let new_subject = input.get("subject").and_then(|v| v.as_str());

            if new_status.is_none() && new_subject.is_none() {
                return false;
            }

            // Handle deletion
            if new_status == Some("deleted") {
                let before = existing_tasks.len();
                existing_tasks.retain(|t| t.id != task_id);
                return existing_tasks.len() != before;
            }

            if let Some(task) = existing_tasks.iter_mut().find(|t| t.id == task_id) {
                if let Some(s) = new_status {
                    task.status = s.to_string();
                }
                if let Some(s) = new_subject {
                    task.content = s.to_string();
                }
                true
            } else {
                false
            }
        }
        _ => false,
    }
}

/// Read context window usage percent from Claude Code's session JSONL.
/// Reads only the tail of the file for efficiency (even if the file is many MB).
fn read_context_percent(session_id: &str, cwd: &str) -> Option<u8> {
    use std::io::{Read, Seek, SeekFrom};

    // Build JSONL path: ~/.claude/projects/{encoded_cwd}/{session_id}.jsonl
    let home = dirs::home_dir()?;
    let encoded_cwd = cwd.replace('/', "-");
    let jsonl_path = home
        .join(".claude/projects")
        .join(&encoded_cwd)
        .join(format!("{}.jsonl", session_id));

    let mut file = std::fs::File::open(&jsonl_path).ok()?;
    let file_len = file.metadata().ok()?.len();

    // Read last 16KB — enough to contain the last assistant message
    let read_from = file_len.saturating_sub(16 * 1024);
    file.seek(SeekFrom::Start(read_from)).ok()?;
    let mut buf = String::new();
    file.read_to_string(&mut buf).ok()?;

    // Find the last assistant message with usage data (search from end)
    let mut model_name = None;
    let mut total_tokens: Option<u64> = None;

    for line in buf.lines().rev() {
        if !line.contains("\"type\":\"assistant\"") && !line.contains("\"type\": \"assistant\"") {
            continue;
        }
        let Ok(v) = serde_json::from_str::<serde_json::Value>(line) else {
            continue;
        };
        if v.get("type").and_then(|t| t.as_str()) != Some("assistant") {
            continue;
        }
        let msg = v.get("message")?;
        let usage = msg.get("usage")?;

        let input = usage.get("input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let cache_create = usage.get("cache_creation_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let cache_read = usage.get("cache_read_input_tokens").and_then(|v| v.as_u64()).unwrap_or(0);
        let total = input + cache_create + cache_read;
        if total == 0 {
            continue;
        }

        total_tokens = Some(total);
        model_name = msg.get("model").and_then(|m| m.as_str()).map(|s| s.to_string());
        break;
    }

    let total = total_tokens?;
    let max_tokens = match model_name.as_deref() {
        Some(m) if m.contains("opus") => 1_000_000u64,
        _ => 200_000u64,
    };

    let pct = ((total * 100) / max_tokens).min(100) as u8;
    Some(pct)
}

/// Track subagent activity by recording its session_id in the parent's subagents list.
/// Only updates the count — no task merging.
fn track_subagent(payload: &HookPayload, data_dir: &str) -> Result<()> {
    let mut store = read_session_store(data_dir);
    let now = chrono::Utc::now().to_rfc3339();

    // Find parent session: prefer CWD match, fallback to most recently updated running session
    let subagent_cwd = payload.cwd.as_deref().unwrap_or("");
    let running_sessions: Vec<_> = store
        .sessions
        .values()
        .filter(|s| !s.tty.is_empty() && s.status == "running")
        .collect();

    let parent_id = if !subagent_cwd.is_empty() {
        running_sessions
            .iter()
            .filter(|s| subagent_cwd.starts_with(&s.home_cwd))
            .max_by_key(|s| s.home_cwd.len())
            .or_else(|| running_sessions.iter().max_by_key(|s| s.updated_at.as_str()))
            .map(|s| s.session_id.clone())
    } else {
        running_sessions
            .iter()
            .max_by_key(|s| s.updated_at.as_str())
            .map(|s| s.session_id.clone())
    };

    let parent_id = match parent_id {
        Some(id) => id,
        None => return Ok(()),
    };

    if let Some(parent) = store.sessions.get_mut(&parent_id) {
        if let Some(entry) = parent.subagents.iter_mut().find(|e| e.session_id == payload.session_id) {
            entry.last_seen = now.clone();
        } else {
            parent.subagents.push(crate::types::SubagentEntry {
                session_id: payload.session_id.clone(),
                last_seen: now.clone(),
            });
        }

        parent.updated_at = now;
        store.updated_at = parent.updated_at.clone();
        write_session_store(&store, data_dir)?;
    }
    Ok(())
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
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

/// Walk the ancestor process chain once, collecting TTY and permission mode in a single pass.
/// Returns `(tty, permission_mode)` where permission_mode is "yolo", "auto", or "normal".
fn get_tty_and_permission_from_ancestors() -> (String, String) {
    let mut ppid = std::os::unix::process::parent_id() as i32;
    let mut found_tty = String::new();
    let mut permission_mode = String::new();

    for _ in 0..5 {
        let Ok(out) = Command::new("ps")
            .args(["-o", "tty=,ppid=,args=", "-p", &ppid.to_string()])
            .stderr(Stdio::null())
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

        if permission_mode.is_empty() {
            if args_field.contains("--dangerously-skip-permissions") {
                permission_mode = "yolo".to_string();
            } else if args_field.contains("--permission-mode auto")
                || args_field.contains("--permission-mode=auto")
            {
                permission_mode = "auto".to_string();
            }
        }

        if !found_tty.is_empty() && !permission_mode.is_empty() {
            break;
        }

        match ppid_field.parse::<i32>() {
            Ok(new_ppid) => ppid = new_ppid,
            Err(_) => break,
        }
    }

    if permission_mode.is_empty() {
        permission_mode = "normal".to_string();
    }

    (found_tty, permission_mode)
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
    pane_id: Option<i32>,
    notification_type: Option<&str>,
    permission_mode: &str,
    activity: Option<String>,
    is_dangerous: bool,
    git_branch: Option<String>,
    user_message: Option<String>,
    payload: &HookPayload,
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
    // Use the new TTY if provided; fall back to existing TTY only when new is empty
    // (e.g. subagent hooks have empty TTY and should not overwrite parent's TTY)
    let final_tty = if tty.is_empty() {
        existing
            .map(|s| s.tty.clone())
            .unwrap_or_default()
    } else {
        tty.to_string()
    };

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

    // Tasks: apply TaskCreate/TaskUpdate mutations in-place (no extra file read)
    let mut final_tasks = existing.map(|s| s.tasks.clone()).unwrap_or_default();
    apply_task_event(event_name, payload, &mut final_tasks);

    // pane_id: update if resolved, otherwise preserve existing
    let final_pane_id = pane_id.or_else(|| existing.and_then(|s| s.pane_id));

    store.sessions.insert(
        session_id.to_string(),
        Session {
            session_id: session_id.to_string(),
            home_cwd,
            tty: final_tty,
            status: new_status.clone(),
            created_at,
            updated_at: now.clone(),
            is_yolo: permission_mode == "yolo",
            permission_mode: permission_mode.to_string(),
            last_activity,
            is_dangerous: final_dangerous,
            git_branch: final_branch,
            last_user_message: final_user_message,
            last_user_message_at: final_user_message_at,
            tasks: final_tasks,
            subagents: existing.map(|s| s.subagents.clone()).unwrap_or_default(),
            pane_id: final_pane_id,
            context_percent: existing.and_then(|s| s.context_percent),
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
            permission_mode: "normal".to_string(),
            last_activity: None,
            is_dangerous: false,
            git_branch: None,
            last_user_message: None,
            last_user_message_at: None,
            tasks: Vec::new(),
            subagents: Vec::new(),
            pane_id: None,
            context_percent: None,
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

    /// Helper: create a dummy HookPayload for tests
    fn empty_payload(session_id: &str) -> HookPayload {
        HookPayload {
            session_id: session_id.to_string(),
            cwd: None,
            notification_type: None,
            tool_name: None,
            tool_input: None,
            tool_response: None,
            prompt: None,
        }
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
            None, // pane_id
            None,
            "normal",
            None,
            false,
            None,
            None,
            &empty_payload("new-session"),
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
            None, // pane_id
            None,
            "normal",
            None,
            false,
            None,
            None,
            &empty_payload("new-session"),
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
        let p = empty_payload("sess-1");

        let status = update_session(
            "UserPromptSubmit", "sess-1", "/tmp/proj", "/dev/ttys001",
            None, None, "normal", None, false, None, None, &p,
            dir.path().to_str().unwrap(),
        ).unwrap();
        assert_eq!(status, "running");

        let status = update_session(
            "Stop", "sess-1", "/tmp/proj", "/dev/ttys001",
            None, None, "normal", None, false, None, None, &p,
            dir.path().to_str().unwrap(),
        ).unwrap();
        assert_eq!(status, "stopped");

        let status = update_session(
            "PreToolUse", "sess-1", "/tmp/proj", "/dev/ttys001",
            None, None, "normal", None, false, None, None, &p,
            dir.path().to_str().unwrap(),
        ).unwrap();
        assert_eq!(status, "stopped");

        let status = update_session(
            "UserPromptSubmit", "sess-1", "/tmp/proj", "/dev/ttys001",
            None, None, "normal", None, false, None, None, &p,
            dir.path().to_str().unwrap(),
        ).unwrap();
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
            None, // pane_id
            None,
            "normal",
            None,
            false,
            None,
            None,
            &empty_payload("new-session"),
            dir.path().to_str().unwrap(),
        )
        .unwrap();

        let store = read_session_store(dir.path().to_str().unwrap());
        assert!(store.sessions.contains_key("existing"));
        assert!(store.sessions.contains_key("new-session"));
    }
}
