//! Kanban task model: tasks.json read/write, dependency resolution, ID generation.
//!
//! tasks.json path: `{data_dir}/tasks.json`
//!
//! The store is loaded, mutated, and written back atomically (PID-suffixed temp
//! file + rename). No in-memory cache is kept across calls — every operation
//! reads the latest file to tolerate concurrent hook writes.

use anyhow::{anyhow, Result};
use chrono::Utc;
use std::{collections::HashSet, fs, path::PathBuf};

use crate::config::expand_tilde;
use crate::types::{KanbanTask, TaskDependency, TaskStatus, TasksFile};

/// Filename under `data_dir` that stores kanban tasks.
pub const TASKS_FILENAME: &str = "tasks.json";

pub fn tasks_file_path(data_dir: &str) -> PathBuf {
    expand_tilde(data_dir).join(TASKS_FILENAME)
}

/// Load tasks.json, returning a default (empty) store on any error.
///
/// Errors are swallowed because hook invocations must never fail loudly.
pub fn load_tasks(data_dir: &str) -> TasksFile {
    let path = tasks_file_path(data_dir);
    match fs::read_to_string(&path) {
        Ok(data) => serde_json::from_str(&data).unwrap_or_default(),
        Err(_) => TasksFile::default(),
    }
}

/// Atomically write tasks.json (write to PID-unique temp, rename into place).
pub fn write_tasks(store: &TasksFile, data_dir: &str) -> Result<()> {
    let path = tasks_file_path(data_dir);
    if let Some(dir) = path.parent() {
        fs::create_dir_all(dir)?;
    }
    let data = serde_json::to_string_pretty(store)?;
    let tmp_path = path.with_extension(format!("json.{}.tmp", std::process::id()));
    fs::write(&tmp_path, data)?;
    fs::rename(&tmp_path, &path)?;
    Ok(())
}

/// Generate a short (8 hex chars) task ID from the system clock + process id +
/// a monotonic counter. Collisions within a single process are avoided by the
/// counter; across processes the nanos-granular timestamp makes collisions
/// practically impossible.
pub fn new_task_id() -> String {
    use std::sync::atomic::{AtomicU64, Ordering};
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as u64)
        .unwrap_or(0);
    let pid = std::process::id() as u64;
    let c = COUNTER.fetch_add(1, Ordering::Relaxed);

    // Mix three sources, take bottom 32 bits, encode as 8 hex chars.
    let mixed = nanos
        .wrapping_mul(0x9E37_79B9_7F4A_7C15)
        .wrapping_add(pid.wrapping_mul(0xC2B2_AE3D_27D4_EB4F))
        .wrapping_add(c.wrapping_mul(0x1656_67B1_9E37_79F9));
    format!("{:08x}", (mixed & 0xFFFF_FFFF) as u32)
}

/// Look up a task by ID.
pub fn find_task<'a>(store: &'a TasksFile, id: &str) -> Option<&'a KanbanTask> {
    store.tasks.iter().find(|t| t.id == id)
}

/// Look up a task mutably by ID.
pub fn find_task_mut<'a>(store: &'a mut TasksFile, id: &str) -> Option<&'a mut KanbanTask> {
    store.tasks.iter_mut().find(|t| t.id == id)
}

/// Create a new backlog task and append to the store (does not persist).
pub fn create_task(
    store: &mut TasksFile,
    title: &str,
    prompt: Option<String>,
    cwd: &str,
) -> KanbanTask {
    let now = Utc::now().to_rfc3339();
    let task = KanbanTask {
        id: new_task_id(),
        title: title.to_string(),
        prompt,
        status: TaskStatus::Backlog,
        cwd: cwd.to_string(),
        session_id: None,
        created_at: now.clone(),
        started_at: None,
        review_started_at: None,
        completed_at: None,
        block_alerted_at: None,
    };
    store.tasks.push(task.clone());
    store.updated_at = now;
    task
}

/// Add a dependency edge `from -> to`.
///
/// Returns an error if the edge would create a cycle or references unknown
/// task IDs. Duplicate edges are silently accepted (idempotent).
pub fn add_dependency(store: &mut TasksFile, from: &str, to: &str) -> Result<()> {
    if from == to {
        return Err(anyhow!("self-dependency not allowed: {}", from));
    }
    if find_task(store, from).is_none() {
        return Err(anyhow!("unknown task: {}", from));
    }
    if find_task(store, to).is_none() {
        return Err(anyhow!("unknown task: {}", to));
    }

    // Already present? (idempotent)
    if store
        .dependencies
        .iter()
        .any(|d| d.from == from && d.to == to)
    {
        return Ok(());
    }

    // Cycle check: with the new edge, does `to` transitively reach `from`?
    let mut tentative = store.clone();
    tentative.dependencies.push(TaskDependency {
        from: from.to_string(),
        to: to.to_string(),
    });
    if has_cycle(&tentative) {
        return Err(anyhow!(
            "adding edge {} -> {} would create a dependency cycle",
            from,
            to
        ));
    }

    store.dependencies.push(TaskDependency {
        from: from.to_string(),
        to: to.to_string(),
    });
    store.updated_at = Utc::now().to_rfc3339();
    Ok(())
}

/// Remove a dependency edge `from -> to`. No-op if the edge doesn't exist.
pub fn remove_dependency(store: &mut TasksFile, from: &str, to: &str) {
    let before = store.dependencies.len();
    store
        .dependencies
        .retain(|d| !(d.from == from && d.to == to));
    if store.dependencies.len() != before {
        store.updated_at = Utc::now().to_rfc3339();
    }
}

/// DFS cycle detection on the dependency graph.
fn has_cycle(store: &TasksFile) -> bool {
    let mut visiting: HashSet<&str> = HashSet::new();
    let mut visited: HashSet<&str> = HashSet::new();

    for task in &store.tasks {
        if !visited.contains(task.id.as_str())
            && dfs_cycle(&task.id, store, &mut visiting, &mut visited)
        {
            return true;
        }
    }
    false
}

fn dfs_cycle<'a>(
    node: &'a str,
    store: &'a TasksFile,
    visiting: &mut HashSet<&'a str>,
    visited: &mut HashSet<&'a str>,
) -> bool {
    if visiting.contains(node) {
        return true;
    }
    if visited.contains(node) {
        return false;
    }
    visiting.insert(node);
    for edge in store.dependencies.iter().filter(|d| d.from == node) {
        if dfs_cycle(&edge.to, store, visiting, visited) {
            return true;
        }
    }
    visiting.remove(node);
    visited.insert(node);
    false
}

/// Change a task's status and update the appropriate timestamp fields.
///
/// Does **not** perform cascade side effects (auto-spawn of dependents etc.);
/// that lives in higher layers to keep this module pure.
pub fn set_task_status(store: &mut TasksFile, id: &str, new_status: TaskStatus) -> Result<()> {
    let now = Utc::now().to_rfc3339();
    let task = find_task_mut(store, id).ok_or_else(|| anyhow!("unknown task: {}", id))?;

    // Skip no-op transitions to preserve timestamps.
    if task.status == new_status {
        return Ok(());
    }

    match new_status {
        TaskStatus::Running => {
            if task.started_at.is_none() {
                task.started_at = Some(now.clone());
            }
            // Leaving review → reset block_alerted_at so next review stint can re-alert.
            if task.status == TaskStatus::Review {
                task.block_alerted_at = None;
            }
        }
        TaskStatus::Review => {
            task.review_started_at = Some(now.clone());
            task.block_alerted_at = None;
        }
        TaskStatus::Done => {
            task.completed_at = Some(now.clone());
            task.block_alerted_at = None;
        }
        TaskStatus::Backlog | TaskStatus::Trash => {
            task.block_alerted_at = None;
        }
    }

    task.status = new_status;
    store.updated_at = now;
    Ok(())
}

/// Return IDs of tasks that directly depend on `from` (i.e. edges `from -> ?`).
pub fn downstream(store: &TasksFile, from: &str) -> Vec<String> {
    store
        .dependencies
        .iter()
        .filter(|d| d.from == from)
        .map(|d| d.to.clone())
        .collect()
}

/// Return IDs of tasks that `to` depends on (i.e. edges `? -> to`).
pub fn upstream(store: &TasksFile, to: &str) -> Vec<String> {
    store
        .dependencies
        .iter()
        .filter(|d| d.to == to)
        .map(|d| d.from.clone())
        .collect()
}

/// A task is *ready to spawn* when it is in `backlog` **and** every upstream
/// dependency is `done`. (No upstream → always ready.)
pub fn is_ready(store: &TasksFile, id: &str) -> bool {
    let task = match find_task(store, id) {
        Some(t) => t,
        None => return false,
    };
    if task.status != TaskStatus::Backlog {
        return false;
    }
    upstream(store, id).iter().all(|up_id| {
        find_task(store, up_id)
            .map(|t| t.status == TaskStatus::Done)
            .unwrap_or(false)
    })
}

/// Locate a backlog task by title (case-sensitive, exact match).
/// Used by hook logic for title → task_id reverse lookup.
pub fn find_backlog_by_title<'a>(store: &'a TasksFile, title: &str) -> Option<&'a KanbanTask> {
    store
        .tasks
        .iter()
        .find(|t| t.title == title && t.status == TaskStatus::Backlog)
}

/// Locate a running/review task by session_id.
#[allow(dead_code)] // used in Phase 4/5 hook integration
pub fn find_by_session_id<'a>(
    store: &'a TasksFile,
    session_id: &str,
) -> Option<&'a KanbanTask> {
    store
        .tasks
        .iter()
        .find(|t| t.session_id.as_deref() == Some(session_id))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mk_task(id: &str, status: TaskStatus) -> KanbanTask {
        KanbanTask {
            id: id.to_string(),
            title: format!("task-{}", id),
            prompt: None,
            status,
            cwd: "/tmp".to_string(),
            session_id: None,
            created_at: "2026-01-01T00:00:00Z".to_string(),
            started_at: None,
            review_started_at: None,
            completed_at: None,
            block_alerted_at: None,
        }
    }

    #[test]
    fn test_new_task_id_is_8_hex_chars() {
        for _ in 0..100 {
            let id = new_task_id();
            assert_eq!(id.len(), 8);
            assert!(id.chars().all(|c| c.is_ascii_hexdigit()));
        }
    }

    #[test]
    fn test_new_task_id_unique_within_process() {
        let mut set = HashSet::new();
        for _ in 0..1000 {
            assert!(set.insert(new_task_id()));
        }
    }

    #[test]
    fn test_roundtrip_empty_store() {
        let dir = tempfile::tempdir().unwrap();
        let dpath = dir.path().to_str().unwrap();
        let store = load_tasks(dpath);
        assert!(store.tasks.is_empty());
        assert!(store.dependencies.is_empty());
        write_tasks(&store, dpath).unwrap();
        let reloaded = load_tasks(dpath);
        assert!(reloaded.tasks.is_empty());
    }

    #[test]
    fn test_create_task_appends() {
        let mut store = TasksFile::default();
        let t = create_task(&mut store, "DB設計", Some("sqlite".to_string()), "/repo");
        assert_eq!(store.tasks.len(), 1);
        assert_eq!(t.title, "DB設計");
        assert_eq!(t.status, TaskStatus::Backlog);
        assert_eq!(t.cwd, "/repo");
    }

    #[test]
    fn test_add_dependency_happy_path() {
        let mut store = TasksFile::default();
        store.tasks.push(mk_task("a", TaskStatus::Backlog));
        store.tasks.push(mk_task("b", TaskStatus::Backlog));
        add_dependency(&mut store, "a", "b").unwrap();
        assert_eq!(store.dependencies.len(), 1);
    }

    #[test]
    fn test_add_dependency_self_loop_rejected() {
        let mut store = TasksFile::default();
        store.tasks.push(mk_task("a", TaskStatus::Backlog));
        let err = add_dependency(&mut store, "a", "a").unwrap_err();
        assert!(err.to_string().contains("self-dependency"));
    }

    #[test]
    fn test_add_dependency_unknown_id() {
        let mut store = TasksFile::default();
        store.tasks.push(mk_task("a", TaskStatus::Backlog));
        assert!(add_dependency(&mut store, "a", "ghost").is_err());
        assert!(add_dependency(&mut store, "ghost", "a").is_err());
    }

    #[test]
    fn test_add_dependency_idempotent() {
        let mut store = TasksFile::default();
        store.tasks.push(mk_task("a", TaskStatus::Backlog));
        store.tasks.push(mk_task("b", TaskStatus::Backlog));
        add_dependency(&mut store, "a", "b").unwrap();
        add_dependency(&mut store, "a", "b").unwrap();
        assert_eq!(store.dependencies.len(), 1);
    }

    #[test]
    fn test_add_dependency_cycle_detection() {
        let mut store = TasksFile::default();
        store.tasks.push(mk_task("a", TaskStatus::Backlog));
        store.tasks.push(mk_task("b", TaskStatus::Backlog));
        store.tasks.push(mk_task("c", TaskStatus::Backlog));
        add_dependency(&mut store, "a", "b").unwrap();
        add_dependency(&mut store, "b", "c").unwrap();
        // c -> a would close the triangle
        let err = add_dependency(&mut store, "c", "a").unwrap_err();
        assert!(err.to_string().contains("cycle"));
        assert_eq!(store.dependencies.len(), 2);
    }

    #[test]
    fn test_remove_dependency() {
        let mut store = TasksFile::default();
        store.tasks.push(mk_task("a", TaskStatus::Backlog));
        store.tasks.push(mk_task("b", TaskStatus::Backlog));
        add_dependency(&mut store, "a", "b").unwrap();
        remove_dependency(&mut store, "a", "b");
        assert!(store.dependencies.is_empty());
        // Idempotent: removing again is a no-op
        remove_dependency(&mut store, "a", "b");
    }

    #[test]
    fn test_is_ready_no_dependencies() {
        let mut store = TasksFile::default();
        store.tasks.push(mk_task("a", TaskStatus::Backlog));
        assert!(is_ready(&store, "a"));
    }

    #[test]
    fn test_is_ready_upstream_done() {
        let mut store = TasksFile::default();
        store.tasks.push(mk_task("a", TaskStatus::Done));
        store.tasks.push(mk_task("b", TaskStatus::Backlog));
        add_dependency(&mut store, "a", "b").unwrap();
        assert!(is_ready(&store, "b"));
    }

    #[test]
    fn test_is_ready_upstream_not_done() {
        let mut store = TasksFile::default();
        store.tasks.push(mk_task("a", TaskStatus::Running));
        store.tasks.push(mk_task("b", TaskStatus::Backlog));
        add_dependency(&mut store, "a", "b").unwrap();
        assert!(!is_ready(&store, "b"));
    }

    #[test]
    fn test_is_ready_non_backlog() {
        let mut store = TasksFile::default();
        store.tasks.push(mk_task("a", TaskStatus::Running));
        assert!(!is_ready(&store, "a"));
    }

    #[test]
    fn test_set_task_status_sets_timestamps() {
        let mut store = TasksFile::default();
        store.tasks.push(mk_task("a", TaskStatus::Backlog));
        set_task_status(&mut store, "a", TaskStatus::Running).unwrap();
        let t = find_task(&store, "a").unwrap();
        assert_eq!(t.status, TaskStatus::Running);
        assert!(t.started_at.is_some());

        set_task_status(&mut store, "a", TaskStatus::Review).unwrap();
        let t = find_task(&store, "a").unwrap();
        assert!(t.review_started_at.is_some());

        set_task_status(&mut store, "a", TaskStatus::Done).unwrap();
        let t = find_task(&store, "a").unwrap();
        assert!(t.completed_at.is_some());
    }

    #[test]
    fn test_set_task_status_review_to_running_clears_alert() {
        let mut store = TasksFile::default();
        store.tasks.push(mk_task("a", TaskStatus::Review));
        find_task_mut(&mut store, "a").unwrap().block_alerted_at =
            Some("2026-01-01T00:00:00Z".to_string());
        set_task_status(&mut store, "a", TaskStatus::Running).unwrap();
        assert!(find_task(&store, "a").unwrap().block_alerted_at.is_none());
    }

    #[test]
    fn test_set_task_status_unknown_id() {
        let mut store = TasksFile::default();
        assert!(set_task_status(&mut store, "ghost", TaskStatus::Running).is_err());
    }

    #[test]
    fn test_downstream_and_upstream() {
        let mut store = TasksFile::default();
        store.tasks.push(mk_task("a", TaskStatus::Backlog));
        store.tasks.push(mk_task("b", TaskStatus::Backlog));
        store.tasks.push(mk_task("c", TaskStatus::Backlog));
        add_dependency(&mut store, "a", "b").unwrap();
        add_dependency(&mut store, "a", "c").unwrap();
        let mut down = downstream(&store, "a");
        down.sort();
        assert_eq!(down, vec!["b".to_string(), "c".to_string()]);
        assert_eq!(upstream(&store, "b"), vec!["a".to_string()]);
        assert!(upstream(&store, "a").is_empty());
    }

    #[test]
    fn test_find_backlog_by_title() {
        let mut store = TasksFile::default();
        let mut t = mk_task("a", TaskStatus::Backlog);
        t.title = "DB設計".to_string();
        store.tasks.push(t);
        assert_eq!(
            find_backlog_by_title(&store, "DB設計").map(|t| t.id.clone()),
            Some("a".to_string())
        );
        // Non-backlog should not match.
        find_task_mut(&mut store, "a").unwrap().status = TaskStatus::Running;
        assert!(find_backlog_by_title(&store, "DB設計").is_none());
    }

    #[test]
    fn test_find_by_session_id() {
        let mut store = TasksFile::default();
        let mut t = mk_task("a", TaskStatus::Running);
        t.session_id = Some("sess-xyz".to_string());
        store.tasks.push(t);
        assert_eq!(
            find_by_session_id(&store, "sess-xyz").map(|t| t.id.clone()),
            Some("a".to_string())
        );
        assert!(find_by_session_id(&store, "ghost").is_none());
    }

    #[test]
    fn test_write_and_load_roundtrip() {
        let dir = tempfile::tempdir().unwrap();
        let dpath = dir.path().to_str().unwrap();

        let mut store = TasksFile::default();
        create_task(&mut store, "タスクA", Some("prompt A".to_string()), "/a");
        create_task(&mut store, "タスクB", None, "/b");
        let ids: Vec<String> = store.tasks.iter().map(|t| t.id.clone()).collect();
        add_dependency(&mut store, &ids[0], &ids[1]).unwrap();
        write_tasks(&store, dpath).unwrap();

        let reloaded = load_tasks(dpath);
        assert_eq!(reloaded.tasks.len(), 2);
        assert_eq!(reloaded.dependencies.len(), 1);
        assert_eq!(reloaded.tasks[0].title, "タスクA");
        assert_eq!(reloaded.tasks[1].title, "タスクB");
    }
}
