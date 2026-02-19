use std::fs;

use crate::config::{expand_tilde, AppConfig};
use crate::types::{GlobalTask, TasksCache};

pub fn load_tasks(config: &AppConfig) -> Vec<GlobalTask> {
    let path = match config.tasks_file {
        Some(ref p) => expand_tilde(p),
        None => return Vec::new(),
    };
    let content = match fs::read_to_string(&path) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    let cache: TasksCache = match serde_json::from_str(&content) {
        Ok(c) => c,
        Err(_) => return Vec::new(),
    };

    cache
        .tasks
        .into_iter()
        .filter(|t| {
            if t.completed {
                return false;
            }
            match config.task_filter_name {
                Some(ref filter_name) => t.assignee.contains(filter_name),
                None => true,
            }
        })
        .map(|t| GlobalTask {
            id: t.gid,
            title: t.name,
            status: "pending".to_string(),
            priority: t.priority,
            due_on: t.due_on,
            created_at: String::new(),
            updated_at: String::new(),
        })
        .collect()
}
