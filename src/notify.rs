//! Desktop notification helpers (macOS terminal-notifier).
//!
//! Two alert types live here:
//!   - `send_permission_notification`: fires when a session enters `waiting_input`.
//!   - `send_block_notification`: fires when a kanban task has been sitting in
//!     `review` for longer than `block_alert_minutes`.
//!
//! Both are best-effort; if `terminal-notifier` is missing (or any subshell
//! errors), they silently no-op so hooks never fail loudly.

use chrono::{DateTime, Utc};
use std::process::{Command, Stdio};

use crate::config::{AppConfig, KanbanConfig};
use crate::session::read_session_store;
use crate::tasks::{load_tasks, write_tasks};
use crate::terminal::TerminalBackend;
use crate::types::TaskStatus;

/// Resolve terminal-notifier binary path, returning `None` if not installed.
fn resolve_notifier() -> Option<String> {
    Command::new("which")
        .arg("terminal-notifier")
        .stderr(Stdio::null())
        .output()
        .ok()
        .filter(|o| o.status.success())
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .filter(|s| !s.is_empty())
}

fn capitalize(s: &str) -> String {
    let mut c = s.chars();
    match c.next() {
        None => String::new(),
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
    }
}

fn shell_escape(s: &str) -> String {
    // Wrap in single quotes and escape embedded single quotes for bash -c.
    let escaped = s.replace('\'', "'\\''");
    format!("'{}'", escaped)
}

/// Desktop notification fired when a Claude Code session prompts for a
/// permission confirmation. Clicking the notification focuses the pane;
/// the "承認" action sends Enter on the recipient's behalf.
pub fn send_permission_notification(cwd: &str, tty: &str, backend: &dyn TerminalBackend) {
    let notifier = match resolve_notifier() {
        Some(p) => p,
        None => return,
    };

    let dir_name = std::path::Path::new(cwd)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("unknown")
        .to_string();

    let (activate_cmd, approve_cmd) = match backend.find_pane_by_tty(tty) {
        Some((tab_id, pane_id)) => (
            backend.build_activate_command(tab_id, pane_id),
            backend.build_approve_command(tab_id, pane_id),
        ),
        None => (
            format!("open -a {}", capitalize(backend.name())),
            format!("open -a {}", capitalize(backend.name())),
        ),
    };

    let script = format!(
        r#"result=$({} -title 'Claude Code' -message '許可待ち: {}' -sound Tink -actions '承認' -sender com.github.wez.wezterm); if [ "$result" = "@ACTIONCLICKED" ]; then {}; elif [ "$result" = "@CONTENTCLICKED" ]; then {}; fi"#,
        notifier, dir_name, approve_cmd, activate_cmd
    );

    let _ = Command::new("bash")
        .args(["-c", &script])
        .stderr(Stdio::null())
        .spawn();
}

/// Desktop notification fired when a kanban task has idled in `review` for
/// longer than the configured threshold. Clicking focuses the pane (no
/// approve action — blocked tasks usually need human attention first).
pub fn send_block_notification(
    task_title: &str,
    minutes: u32,
    sound: &str,
    tty: Option<&str>,
    backend: &dyn TerminalBackend,
) {
    let notifier = match resolve_notifier() {
        Some(p) => p,
        None => return,
    };

    let activate_cmd = tty
        .and_then(|t| backend.find_pane_by_tty(t))
        .map(|(tab_id, pane_id)| backend.build_activate_command(tab_id, pane_id))
        .unwrap_or_else(|| format!("open -a {}", capitalize(backend.name())));

    let sound_arg = if sound.is_empty() { "Basso" } else { sound };
    let title_q = shell_escape(task_title);
    let message = format!("{}分以上放置されています", minutes);
    let message_q = shell_escape(&message);

    let script = format!(
        "result=$({notifier} -title 'Claude Code (ブロック)' -subtitle {title} -message {msg} -sound {sound} -sender com.github.wez.wezterm); if [ \"$result\" = \"@CONTENTCLICKED\" ]; then {activate}; fi",
        notifier = notifier,
        title = title_q,
        msg = message_q,
        sound = sound_arg,
        activate = activate_cmd,
    );

    let _ = Command::new("bash")
        .args(["-c", &script])
        .stderr(Stdio::null())
        .spawn();
}

/// Scan tasks.json for review-state tasks whose `review_started_at` exceeds
/// the configured threshold; for each match that hasn't been alerted (or whose
/// cooldown has expired), fire a block notification and stamp
/// `block_alerted_at` back to tasks.json.
///
/// Returns the list of task IDs newly alerted this call (for logging / tests).
pub fn process_block_alerts(config: &AppConfig, backend: &dyn TerminalBackend) -> Vec<String> {
    let KanbanConfig {
        block_alert_minutes,
        block_alert_sound,
        block_alert_cooldown_secs,
        ..
    } = config.kanban.clone();

    if block_alert_minutes == 0 {
        return Vec::new();
    }

    let threshold = chrono::Duration::minutes(block_alert_minutes as i64);
    let now = Utc::now();
    let cooldown = chrono::Duration::seconds(block_alert_cooldown_secs as i64);

    let mut store = load_tasks(&config.data_dir);
    let session_store = read_session_store(&config.data_dir);

    // Decide per-task whether to alert. Collect targets first, then mutate
    // so the inner loop can borrow immutably.
    struct Alert {
        id: String,
        title: String,
        tty: Option<String>,
    }

    let targets: Vec<Alert> = store
        .tasks
        .iter()
        .filter_map(|task| {
            if task.status != TaskStatus::Review {
                return None;
            }
            let started = task
                .review_started_at
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())?
                .with_timezone(&Utc);
            if now.signed_duration_since(started) < threshold {
                return None;
            }
            // Dedupe: skip if already alerted within cooldown.
            if let Some(last) = task
                .block_alerted_at
                .as_deref()
                .and_then(|s| DateTime::parse_from_rfc3339(s).ok())
            {
                if block_alert_cooldown_secs == 0 {
                    // 0 = alert once per review stint
                    return None;
                }
                if now.signed_duration_since(last.with_timezone(&Utc)) < cooldown {
                    return None;
                }
            }

            // Best-effort tty lookup via sessions.json
            let tty = task
                .session_id
                .as_deref()
                .and_then(|sid| session_store.sessions.get(sid))
                .map(|sess| sess.tty.clone())
                .filter(|s| !s.is_empty());

            Some(Alert {
                id: task.id.clone(),
                title: task.title.clone(),
                tty,
            })
        })
        .collect();

    if targets.is_empty() {
        return Vec::new();
    }

    let stamp = now.to_rfc3339();
    let mut fired = Vec::new();

    for alert in targets {
        send_block_notification(
            &alert.title,
            block_alert_minutes,
            &block_alert_sound,
            alert.tty.as_deref(),
            backend,
        );
        if let Some(task) = store.tasks.iter_mut().find(|t| t.id == alert.id) {
            task.block_alerted_at = Some(stamp.clone());
        }
        fired.push(alert.id);
    }

    store.updated_at = stamp;
    let _ = write_tasks(&store, &config.data_dir);
    fired
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tasks::create_task;
    use crate::terminal::{TerminalPane, WezTermBackend};
    use crate::types::TasksFile;

    #[test]
    fn test_capitalize() {
        assert_eq!(capitalize("wezterm"), "Wezterm");
        assert_eq!(capitalize("tmux"), "Tmux");
        assert_eq!(capitalize(""), "");
    }

    #[test]
    fn test_shell_escape_plain() {
        assert_eq!(shell_escape("hello"), "'hello'");
        assert_eq!(shell_escape("a b c"), "'a b c'");
    }

    #[test]
    fn test_shell_escape_embedded_quote() {
        assert_eq!(shell_escape("it's"), r#"'it'\''s'"#);
    }

    /// A fake backend so tests don't need a real terminal.
    struct NullBackend;

    impl TerminalBackend for NullBackend {
        fn list_panes(&self) -> Vec<TerminalPane> { Vec::new() }
        fn activate_pane(&self, _: i32, _: i32) {}
        fn get_pane_text(&self, _: i32) -> Vec<String> { Vec::new() }
        fn current_pane_id(&self) -> i32 { -1 }
        fn build_activate_command(&self, _: i32, _: i32) -> String { String::new() }
        fn build_approve_command(&self, _: i32, _: i32) -> String { String::new() }
        fn spawn_pane(&self, _: &str, _: &[&str], _: bool) -> Option<i32> { None }
        fn set_tab_title(&self, _: i32, _: &str) {}
        fn send_text(&self, _: i32, _: &str, _: bool) {}
        fn name(&self) -> &str { "null" }
    }

    #[test]
    fn test_process_block_alerts_skips_non_review() {
        let dir = tempfile::tempdir().unwrap();
        let dpath = dir.path().to_str().unwrap();
        let mut store = TasksFile::default();
        create_task(&mut store, "task A", None, "/tmp");
        crate::tasks::write_tasks(&store, dpath).unwrap();

        let mut cfg = AppConfig::default();
        cfg.data_dir = dpath.to_string();

        let fired = process_block_alerts(&cfg, &NullBackend);
        assert!(fired.is_empty());
    }

    #[test]
    fn test_process_block_alerts_fires_for_stale_review() {
        let dir = tempfile::tempdir().unwrap();
        let dpath = dir.path().to_str().unwrap();
        let mut store = TasksFile::default();
        let task = create_task(&mut store, "stale task", None, "/tmp");
        let id = task.id.clone();
        // Manually transition to review with a review_started_at 10 min ago
        {
            let t = store.tasks.iter_mut().find(|t| t.id == id).unwrap();
            t.status = TaskStatus::Review;
            t.review_started_at = Some(
                (Utc::now() - chrono::Duration::minutes(10)).to_rfc3339(),
            );
        }
        crate::tasks::write_tasks(&store, dpath).unwrap();

        let mut cfg = AppConfig::default();
        cfg.data_dir = dpath.to_string();
        // Default threshold is 5 minutes, so 10 min > threshold
        let fired = process_block_alerts(&cfg, &NullBackend);
        assert_eq!(fired, vec![id.clone()]);

        // Second call should not re-fire (block_alerted_at now stamped, cooldown 0)
        let fired2 = process_block_alerts(&cfg, &NullBackend);
        assert!(fired2.is_empty());
    }

    #[test]
    fn test_process_block_alerts_respects_threshold() {
        let dir = tempfile::tempdir().unwrap();
        let dpath = dir.path().to_str().unwrap();
        let mut store = TasksFile::default();
        let task = create_task(&mut store, "fresh task", None, "/tmp");
        let id = task.id.clone();
        // Review started 1 min ago — threshold is 5 min default.
        {
            let t = store.tasks.iter_mut().find(|t| t.id == id).unwrap();
            t.status = TaskStatus::Review;
            t.review_started_at = Some(
                (Utc::now() - chrono::Duration::minutes(1)).to_rfc3339(),
            );
        }
        crate::tasks::write_tasks(&store, dpath).unwrap();

        let mut cfg = AppConfig::default();
        cfg.data_dir = dpath.to_string();
        assert!(process_block_alerts(&cfg, &NullBackend).is_empty());
    }

    #[test]
    fn test_process_block_alerts_disabled_when_minutes_zero() {
        let dir = tempfile::tempdir().unwrap();
        let dpath = dir.path().to_str().unwrap();
        let mut store = TasksFile::default();
        let task = create_task(&mut store, "task", None, "/tmp");
        let id = task.id.clone();
        {
            let t = store.tasks.iter_mut().find(|t| t.id == id).unwrap();
            t.status = TaskStatus::Review;
            t.review_started_at = Some(
                (Utc::now() - chrono::Duration::minutes(100)).to_rfc3339(),
            );
        }
        crate::tasks::write_tasks(&store, dpath).unwrap();

        let mut cfg = AppConfig::default();
        cfg.data_dir = dpath.to_string();
        cfg.kanban.block_alert_minutes = 0;
        assert!(process_block_alerts(&cfg, &NullBackend).is_empty());
    }

    // Silence unused-import warnings inside the test module
    #[allow(dead_code)]
    fn _assert_wezterm_backend_constructable() -> WezTermBackend {
        WezTermBackend::new("wezterm".into())
    }
}
