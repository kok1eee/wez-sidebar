use std::fs;
use std::path::PathBuf;

use crate::config::{expand_tilde, AppConfig};
use crate::types::{Task, TasksFile};

/// Get the tasks file path from config (with tilde expansion)
pub fn get_tasks_file_path(config: &AppConfig) -> Option<PathBuf> {
    config.tasks_file.as_ref().map(|p| expand_tilde(p))
}

/// Load tasks from the tasks file, filtering out completed ones
pub fn load_tasks(config: &AppConfig) -> Vec<Task> {
    let path = match get_tasks_file_path(config) {
        Some(p) => p,
        None => return Vec::new(),
    };
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let tasks_file: TasksFile = match serde_json::from_str(&content) {
        Ok(f) => f,
        Err(_) => return Vec::new(),
    };

    tasks_file
        .tasks
        .into_iter()
        .filter(|t| t.status != "completed")
        .collect()
}

/// Add a new task to the tasks file
pub fn add_task(config: &AppConfig, title: String, priority: i32, due_on: Option<String>) {
    let path = match get_tasks_file_path(config) {
        Some(p) => p,
        None => {
            eprintln!("tasks_file is not configured");
            return;
        }
    };

    let mut tasks_file = read_tasks_file(&path);

    let id = generate_id();
    let task = Task {
        id: id.clone(),
        title,
        status: "pending".to_string(),
        priority,
        due_on,
    };

    tasks_file.tasks.push(task);
    write_tasks_file(&path, &tasks_file);
    println!("Added task: {}", id);
}

/// Mark a task as completed
pub fn complete_task(config: &AppConfig, id: &str) {
    let path = match get_tasks_file_path(config) {
        Some(p) => p,
        None => {
            eprintln!("tasks_file is not configured");
            return;
        }
    };

    let mut tasks_file = read_tasks_file(&path);

    let mut found = false;
    for task in &mut tasks_file.tasks {
        if task.id == id {
            task.status = "completed".to_string();
            found = true;
            break;
        }
    }

    if found {
        write_tasks_file(&path, &tasks_file);
        println!("Completed task: {}", id);
    } else {
        eprintln!("Task not found: {}", id);
    }
}

/// List all non-completed tasks (CLI output)
pub fn list_tasks(config: &AppConfig) {
    let tasks = load_tasks(config);
    if tasks.is_empty() {
        println!("No active tasks.");
        return;
    }

    for task in &tasks {
        let priority_label = match task.priority {
            1 => "high",
            2 => "medium",
            _ => "low",
        };
        let due = task
            .due_on
            .as_deref()
            .unwrap_or("-");
        println!(
            "[{}] {} (priority: {}, due: {}, status: {})",
            task.id, task.title, priority_label, due, task.status
        );
    }
}

/// Read the tasks file, returning an empty TasksFile if it doesn't exist
fn read_tasks_file(path: &PathBuf) -> TasksFile {
    match fs::read_to_string(path) {
        Ok(content) => serde_json::from_str(&content).unwrap_or(TasksFile { tasks: Vec::new() }),
        Err(_) => TasksFile { tasks: Vec::new() },
    }
}

/// Write the tasks file, creating parent directories if needed
fn write_tasks_file(path: &PathBuf, tasks_file: &TasksFile) {
    if let Some(parent) = path.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let json = serde_json::to_string_pretty(tasks_file).expect("Failed to serialize tasks");
    fs::write(path, json).expect("Failed to write tasks file");
}

/// Generate a short unique ID (timestamp-based)
fn generate_id() -> String {
    use std::time::{SystemTime, UNIX_EPOCH};
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{:x}", now.as_millis())
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    fn test_config(dir: &TempDir) -> AppConfig {
        let tasks_path = dir.path().join("tasks.json");
        AppConfig {
            tasks_file: Some(tasks_path.to_string_lossy().to_string()),
            ..AppConfig::default()
        }
    }

    #[test]
    fn test_add_and_load_tasks() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);

        add_task(&config, "Test task 1".to_string(), 1, None);
        add_task(
            &config,
            "Test task 2".to_string(),
            2,
            Some("2026-03-10".to_string()),
        );

        let tasks = load_tasks(&config);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].title, "Test task 1");
        assert_eq!(tasks[0].priority, 1);
        assert_eq!(tasks[0].status, "pending");
        assert_eq!(tasks[1].title, "Test task 2");
        assert_eq!(tasks[1].due_on, Some("2026-03-10".to_string()));
    }

    #[test]
    fn test_complete_task() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);

        add_task(&config, "Task to complete".to_string(), 2, None);

        let tasks = load_tasks(&config);
        assert_eq!(tasks.len(), 1);
        let task_id = tasks[0].id.clone();

        complete_task(&config, &task_id);

        // After completion, load_tasks filters out completed
        let tasks = load_tasks(&config);
        assert_eq!(tasks.len(), 0);

        // But the task is still in the file with status "completed"
        let path = get_tasks_file_path(&config).unwrap();
        let file: TasksFile =
            serde_json::from_str(&fs::read_to_string(&path).unwrap()).unwrap();
        assert_eq!(file.tasks.len(), 1);
        assert_eq!(file.tasks[0].status, "completed");
    }

    #[test]
    fn test_load_empty() {
        let dir = TempDir::new().unwrap();
        let config = test_config(&dir);
        let tasks = load_tasks(&config);
        assert_eq!(tasks.len(), 0);
    }

    #[test]
    fn test_no_tasks_file_configured() {
        let config = AppConfig {
            tasks_file: None,
            ..AppConfig::default()
        };
        let tasks = load_tasks(&config);
        assert_eq!(tasks.len(), 0);
    }
}
