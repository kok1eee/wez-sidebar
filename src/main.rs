mod app;
mod config;
mod dock;
mod hooks;
mod init;
mod notify;
mod reaper;
mod session;
mod tasks;
mod terminal;
mod types;
mod ui;
mod usage;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config::load_config;
use crate::dock::run_dock;
use crate::hooks::handle_hook;
use crate::reaper::reap_orphans;
use crate::session::load_sessions_data;
use crate::terminal::create_backend;
use crate::ui::run_tui;

#[derive(Parser)]
#[command(name = "wez-sidebar")]
#[command(about = "WezTerm sidebar for Claude Code session monitoring")]
struct Cli {
    #[command(subcommand)]
    command: Option<Commands>,
}

#[derive(Subcommand)]
enum Commands {
    /// Handle Claude Code hook event
    Hook {
        /// Event name (PreToolUse, PostToolUse, Notification, Stop, UserPromptSubmit)
        event: String,
    },
    /// Run as horizontal dock (bottom bar mode)
    Dock,
    /// Interactive setup wizard
    Init,
    /// Print diagnostic info for debugging
    Diag,
    /// Clean up orphaned Claude Code processes
    Reap {
        /// Dry run: list orphans without killing
        #[arg(long)]
        dry: bool,
    },
    /// Spawn a new Claude Code session in a new terminal tab (or window)
    New {
        /// Working directory for the new session (default: current directory)
        dir: Option<String>,
        /// Open in a new window instead of a new tab
        #[arg(short = 'w', long)]
        window: bool,
        /// Bind this spawn to a new kanban task with the given title
        #[arg(long = "task")]
        task_title: Option<String>,
        /// Initial prompt to send to the new session (requires --task or stand-alone)
        #[arg(long)]
        prompt: Option<String>,
        /// Extra arguments passed through to `claude` (use `--` to separate)
        #[arg(last = true)]
        claude_args: Vec<String>,
    },
    /// Manage kanban tasks
    Tasks {
        #[command(subcommand)]
        action: TasksAction,
    },
}

#[derive(Subcommand)]
enum TasksAction {
    /// Add a new task to the backlog
    Add {
        /// Task title (used as claude -n value and display name)
        title: String,
        /// Working directory (defaults to current dir)
        #[arg(long)]
        cwd: Option<String>,
        /// Initial prompt to send when the task is spawned
        #[arg(long)]
        prompt: Option<String>,
        /// Mark this task as depending on another (can repeat)
        #[arg(long = "depends-on")]
        depends_on: Vec<String>,
    },
    /// List tasks (optionally filtered)
    List {
        /// Filter by status (backlog/running/review/done/trash)
        #[arg(long)]
        status: Option<String>,
        /// Output format: table (default) or json
        #[arg(long, default_value = "table")]
        format: String,
    },
    /// Create a dependency: `from` must be done before `to` can start
    Link {
        from: String,
        to: String,
    },
    /// Remove a dependency edge
    Unlink {
        from: String,
        to: String,
    },
    /// Spawn a backlog task (transitions to running)
    Start {
        id: String,
        /// Open in a new window instead of a new tab
        #[arg(short = 'w', long)]
        window: bool,
    },
    /// Approve a review task (transitions to done, auto-spawns dependents)
    Approve {
        id: String,
    },
    /// Reject a review task (returns to running for further prompts)
    Reject {
        id: String,
    },
    /// Move a task to trash (soft delete)
    Trash {
        id: String,
    },
    /// Restore a trashed task back to backlog
    Restore {
        id: String,
    },
    /// Edit a task's title or prompt
    Edit {
        id: String,
        #[arg(long)]
        title: Option<String>,
        #[arg(long)]
        prompt: Option<String>,
    },
    /// Resume a done task by calling `claude --resume "<title>"`
    Resume {
        id: String,
        /// Open in a new window instead of a new tab
        #[arg(short = 'w', long)]
        window: bool,
    },
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    let config = load_config();

    match cli.command {
        Some(Commands::Hook { event }) => {
            handle_hook(&event, &config)?;
        }
        Some(Commands::Dock) => {
            run_dock(config)?;
        }
        Some(Commands::Init) => {
            init::run_init();
        }
        Some(Commands::Diag) => {
            let backend = create_backend(&config.backend, config.effective_terminal_path());
            let pane_id = backend.current_pane_id();
            println!("backend: {}", backend.name());
            println!("current_pane_id: {}", pane_id);

            let panes = backend.list_panes();
            println!("terminal panes: {} found", panes.len());
            for p in &panes {
                let marker = if p.pane_id == pane_id { " <-- self" } else { "" };
                println!("  pane={} tab={} win={} tty={} active={}{}", p.pane_id, p.tab_id, p.window_id, p.tty_name, p.is_active, marker);
            }

            let sessions = load_sessions_data(&config, backend.as_ref());
            println!("\nloaded sessions: {}", sessions.len());
            for s in &sessions {
                println!("  {} tab={} pane={} status={} dc={} stale={}", s.name, s.tab_id, s.pane_id, s.status, s.is_disconnected, s.is_stale);
            }
        }
        Some(Commands::Reap { dry }) => {
            let label = if dry { "[DRY RUN] " } else { "" };
            let reaped = reap_orphans(&config, dry);
            if reaped.is_empty() {
                println!("{}No orphaned Claude Code processes found.", label);
            } else {
                println!("{}Found {} orphan(s):", label, reaped.len());
                for p in &reaped {
                    let action = if dry { "would kill" } else { "killed" };
                    println!(
                        "  {} PID={} PGID={} TTY={} elapsed={} {}",
                        action, p.pid, p.pgid, p.tty, p.elapsed, p.args
                    );
                }
            }
        }
        Some(Commands::New { dir, window, task_title, prompt, claude_args }) => {
            spawn_new_session(&config, dir, window, task_title, prompt, claude_args)?;
        }
        Some(Commands::Tasks { action }) => {
            handle_tasks_command(&config, action)?;
        }
        None => {
            run_tui(config)?;
        }
    }

    Ok(())
}

/// Spawn a new Claude Code session in a new terminal tab (or window).
/// Resolves `dir` to an absolute path, spawns `claude` via the active terminal
/// backend, then sets the tab title to the directory's basename (or the
/// `task_title` when --task is set).
///
/// When `task_title` is provided:
///   1. A backlog task is added to tasks.json (unless one with the same title
///      already exists, in which case it is reused).
///   2. The session is spawned with `claude -n "<title>"` for name binding.
///   3. If `prompt` is set, the prompt is pasted into the new pane via the
///      backend's send_text.
fn spawn_new_session(
    config: &crate::config::AppConfig,
    dir: Option<String>,
    window: bool,
    task_title: Option<String>,
    prompt: Option<String>,
    claude_args: Vec<String>,
) -> Result<()> {
    let cwd = match dir {
        Some(d) => {
            let p = std::path::PathBuf::from(&d);
            std::fs::canonicalize(&p)
                .map_err(|e| anyhow::anyhow!("invalid dir '{}': {}", d, e))?
        }
        None => std::env::current_dir()?,
    };

    if !cwd.is_dir() {
        anyhow::bail!("not a directory: {}", cwd.display());
    }

    let backend = create_backend(&config.backend, config.effective_terminal_path());
    let cwd_str = cwd.to_string_lossy().to_string();

    // Build `claude` argv. When --task is given, add `-n "<title>"` so the
    // Claude Code session name equals the task title (for hook reverse lookup
    // and `claude --resume "<title>"`).
    let mut prog: Vec<String> = vec!["claude".to_string()];
    if let Some(ref title) = task_title {
        prog.push("-n".to_string());
        prog.push(title.clone());
    }
    prog.extend(claude_args);
    let prog_refs: Vec<&str> = prog.iter().map(String::as_str).collect();

    // Register the task in tasks.json before spawning. If a backlog task with
    // the same title exists, reuse it; otherwise create a new one.
    if let Some(ref title) = task_title {
        let mut store = crate::tasks::load_tasks(&config.data_dir);
        let existing_id = crate::tasks::find_backlog_by_title(&store, title)
            .map(|t| t.id.clone());
        if existing_id.is_none() {
            crate::tasks::create_task(&mut store, title, prompt.clone(), &cwd_str);
            crate::tasks::write_tasks(&store, &config.data_dir)?;
        }
    }

    let pane_id = backend
        .spawn_pane(&cwd_str, &prog_refs, window)
        .ok_or_else(|| anyhow::anyhow!("failed to spawn new {} session", backend.name()))?;

    // Tab title: task title when present, else cwd basename.
    let title = task_title.clone().unwrap_or_else(|| {
        cwd.file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| "claude".to_string())
    });
    backend.set_tab_title(pane_id, &title);

    // Ship the initial prompt. We wait briefly for `claude` to finish booting
    // (rough heuristic) then paste + Enter. The whole flow is best-effort —
    // the task stays in backlog until UserPromptSubmit wires up session_id.
    if let Some(prompt_text) = prompt {
        if !prompt_text.is_empty() {
            std::thread::sleep(std::time::Duration::from_millis(1500));
            backend.send_text(pane_id, &prompt_text, true);
        }
    }

    println!("spawned pane {} in {}", pane_id, cwd.display());
    if task_title.is_some() {
        println!("task bound to title: {}", title);
    }
    Ok(())
}

// ============================================================================
// tasks subcommand handlers
// ============================================================================

fn handle_tasks_command(config: &crate::config::AppConfig, action: TasksAction) -> Result<()> {
    use crate::tasks as t;
    use crate::types::TaskStatus;

    match action {
        TasksAction::Add {
            title,
            cwd,
            prompt,
            depends_on,
        } => {
            let cwd = match cwd {
                Some(d) => std::fs::canonicalize(&d)
                    .map_err(|e| anyhow::anyhow!("invalid cwd '{}': {}", d, e))?
                    .to_string_lossy()
                    .to_string(),
                None => std::env::current_dir()?.to_string_lossy().to_string(),
            };

            let mut store = t::load_tasks(&config.data_dir);
            let task = t::create_task(&mut store, &title, prompt, &cwd);
            let new_id = task.id.clone();
            for up in &depends_on {
                t::add_dependency(&mut store, up, &new_id)
                    .map_err(|e| anyhow::anyhow!("depends-on {}: {}", up, e))?;
            }
            t::write_tasks(&store, &config.data_dir)?;
            println!("{}\tbacklog\t{}", new_id, title);
        }

        TasksAction::List { status, format } => {
            let store = t::load_tasks(&config.data_dir);
            let filter = status
                .as_deref()
                .and_then(TaskStatus::parse);
            let mut tasks: Vec<&crate::types::KanbanTask> = store
                .tasks
                .iter()
                .filter(|task| {
                    filter.map(|f| task.status == f).unwrap_or(true)
                })
                .collect();
            // Stable sort: backlog → running → review → done → trash, then created_at asc
            tasks.sort_by(|a, b| {
                status_order(&a.status)
                    .cmp(&status_order(&b.status))
                    .then(a.created_at.cmp(&b.created_at))
            });

            match format.as_str() {
                "json" => {
                    let out: Vec<_> = tasks.into_iter().cloned().collect();
                    println!("{}", serde_json::to_string_pretty(&out)?);
                }
                _ => {
                    if tasks.is_empty() {
                        println!("(no tasks)");
                        return Ok(());
                    }
                    println!("{:<10} {:<10} {:<40} DEPS", "ID", "STATUS", "TITLE");
                    for task in tasks {
                        let deps = t::upstream(&store, &task.id);
                        let deps_str = if deps.is_empty() {
                            "-".to_string()
                        } else {
                            deps.join(",")
                        };
                        println!(
                            "{:<10} {:<10} {:<40} {}",
                            task.id,
                            task.status.as_str(),
                            ellipsis(&task.title, 40),
                            deps_str
                        );
                    }
                }
            }
        }

        TasksAction::Link { from, to } => {
            let mut store = t::load_tasks(&config.data_dir);
            t::add_dependency(&mut store, &from, &to)?;
            t::write_tasks(&store, &config.data_dir)?;
            println!("linked {} -> {}", from, to);
        }

        TasksAction::Unlink { from, to } => {
            let mut store = t::load_tasks(&config.data_dir);
            t::remove_dependency(&mut store, &from, &to);
            t::write_tasks(&store, &config.data_dir)?;
            println!("unlinked {} -> {}", from, to);
        }

        TasksAction::Start { id, window } => {
            let store = t::load_tasks(&config.data_dir);
            let task = t::find_task(&store, &id)
                .ok_or_else(|| anyhow::anyhow!("unknown task: {}", id))?;
            if task.status != TaskStatus::Backlog {
                anyhow::bail!(
                    "task {} is {} (must be backlog to start)",
                    id,
                    task.status.as_str()
                );
            }
            let title = task.title.clone();
            let cwd = task.cwd.clone();
            let prompt = task.prompt.clone();
            // Defer to spawn_new_session for the actual spawn — it also records
            // the task in tasks.json; since our backlog entry already exists
            // with this title, it will be reused.
            spawn_new_session(
                config,
                Some(cwd),
                window,
                Some(title),
                prompt,
                Vec::new(),
            )?;
        }

        TasksAction::Approve { id } => {
            let spawned = approve_task(config, &id)?;
            println!("approved {}", id);
            for (dn_id, title) in spawned {
                println!("  auto-spawned downstream: {} ({})", dn_id, title);
            }
        }

        TasksAction::Reject { id } => {
            reject_task(config, &id)?;
            println!("rejected {} (status -> running)", id);
        }

        TasksAction::Trash { id } => {
            trash_task(config, &id)?;
            println!("trashed {}", id);
        }

        TasksAction::Restore { id } => {
            let mut store = t::load_tasks(&config.data_dir);
            let task = t::find_task(&store, &id)
                .ok_or_else(|| anyhow::anyhow!("unknown task: {}", id))?;
            if task.status != TaskStatus::Trash {
                anyhow::bail!(
                    "task {} is {} (must be trash to restore)",
                    id,
                    task.status.as_str()
                );
            }
            t::set_task_status(&mut store, &id, TaskStatus::Backlog)?;
            t::write_tasks(&store, &config.data_dir)?;
            println!("restored {} (status -> backlog)", id);
        }

        TasksAction::Edit { id, title, prompt } => {
            let mut store = t::load_tasks(&config.data_dir);
            let task = t::find_task_mut(&mut store, &id)
                .ok_or_else(|| anyhow::anyhow!("unknown task: {}", id))?;
            if let Some(new_title) = title {
                task.title = new_title;
            }
            if let Some(new_prompt) = prompt {
                task.prompt = if new_prompt.is_empty() {
                    None
                } else {
                    Some(new_prompt)
                };
            }
            store.updated_at = chrono::Utc::now().to_rfc3339();
            t::write_tasks(&store, &config.data_dir)?;
            println!("edited {}", id);
        }

        TasksAction::Resume { id, window } => {
            let store = t::load_tasks(&config.data_dir);
            let task = t::find_task(&store, &id)
                .ok_or_else(|| anyhow::anyhow!("unknown task: {}", id))?;
            let title = task.title.clone();
            let cwd = task.cwd.clone();

            let backend = create_backend(&config.backend, config.effective_terminal_path());
            let prog = vec!["claude", "--resume", &title];
            let pane_id = backend
                .spawn_pane(&cwd, &prog, window)
                .ok_or_else(|| anyhow::anyhow!("failed to spawn resumed session"))?;
            backend.set_tab_title(pane_id, &title);
            println!("resumed pane {} for task {}", pane_id, id);
        }
    }

    Ok(())
}

/// Approve a task (review → done) and auto-spawn newly-ready downstream
/// tasks. Returns a list of `(downstream_id, title)` pairs for each
/// downstream task that was spawned.
///
/// Callable from both CLI and TUI.
pub fn approve_task(
    config: &crate::config::AppConfig,
    id: &str,
) -> Result<Vec<(String, String)>> {
    use crate::tasks as t;
    use crate::types::TaskStatus;

    let mut store = t::load_tasks(&config.data_dir);
    let task = t::find_task(&store, id)
        .ok_or_else(|| anyhow::anyhow!("unknown task: {}", id))?;
    if task.status != TaskStatus::Review {
        anyhow::bail!(
            "task {} is {} (must be review to approve)",
            id,
            task.status.as_str()
        );
    }
    t::set_task_status(&mut store, id, TaskStatus::Done)?;
    t::write_tasks(&store, &config.data_dir)?;

    let ready: Vec<String> = t::downstream(&store, id)
        .into_iter()
        .filter(|dn| t::is_ready(&store, dn))
        .collect();

    let mut spawned = Vec::new();
    for dn_id in ready {
        let (title, cwd, prompt) = {
            let store = t::load_tasks(&config.data_dir);
            match t::find_task(&store, &dn_id) {
                Some(t) => (t.title.clone(), t.cwd.clone(), t.prompt.clone()),
                None => continue,
            }
        };
        if let Err(e) =
            spawn_new_session(config, Some(cwd), false, Some(title.clone()), prompt, Vec::new())
        {
            eprintln!("spawn downstream {} failed: {}", dn_id, e);
            continue;
        }
        spawned.push((dn_id, title));
    }
    Ok(spawned)
}

/// Reject a task (review → running). Callable from both CLI and TUI.
pub fn reject_task(config: &crate::config::AppConfig, id: &str) -> Result<()> {
    use crate::tasks as t;
    use crate::types::TaskStatus;

    let mut store = t::load_tasks(&config.data_dir);
    let task = t::find_task(&store, id)
        .ok_or_else(|| anyhow::anyhow!("unknown task: {}", id))?;
    if task.status != TaskStatus::Review {
        anyhow::bail!(
            "task {} is {} (must be review to reject)",
            id,
            task.status.as_str()
        );
    }
    t::set_task_status(&mut store, id, TaskStatus::Running)?;
    t::write_tasks(&store, &config.data_dir)?;
    Ok(())
}

/// Trash a task (any state → trash). Callable from both CLI and TUI.
pub fn trash_task(config: &crate::config::AppConfig, id: &str) -> Result<()> {
    use crate::tasks as t;
    use crate::types::TaskStatus;

    let mut store = t::load_tasks(&config.data_dir);
    t::set_task_status(&mut store, id, TaskStatus::Trash)?;
    t::write_tasks(&store, &config.data_dir)?;
    Ok(())
}

fn status_order(s: &crate::types::TaskStatus) -> u8 {
    use crate::types::TaskStatus as S;
    match s {
        S::Backlog => 0,
        S::Running => 1,
        S::Review => 2,
        S::Done => 3,
        S::Trash => 4,
    }
}

fn ellipsis(s: &str, max: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    if UnicodeWidthStr::width(s) <= max {
        return s.to_string();
    }
    let mut out = String::new();
    let mut w = 0;
    for ch in s.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw + 1 > max {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}
