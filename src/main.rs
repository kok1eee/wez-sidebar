mod api_client;
mod app;
mod config;
mod dock;
mod hooks;
mod init;
mod session;
mod tasks;
mod types;
mod ui;
mod usage;

use anyhow::Result;
use clap::{Parser, Subcommand};

use crate::config::load_config;
use crate::dock::run_dock;
use crate::hooks::handle_hook;
use crate::session::{get_wezterm_panes, load_sessions_data};
use crate::ui::run_tui;

#[derive(Parser)]
#[command(name = "wez-sidebar")]
#[command(about = "WezTerm sidebar with Claude Code monitoring and task management")]
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
    /// Manage tasks
    Task {
        #[command(subcommand)]
        action: TaskAction,
    },
}

#[derive(Subcommand)]
enum TaskAction {
    /// Add a new task
    Add {
        /// Task title
        title: String,
        /// Priority (1=high, 2=medium, 3=low)
        #[arg(short, long, default_value_t = 2)]
        priority: i32,
        /// Due date (YYYY-MM-DD)
        #[arg(short, long)]
        due: Option<String>,
    },
    /// Mark a task as completed
    Done {
        /// Task ID
        id: String,
    },
    /// List all active tasks
    List,
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
            let pane_env = std::env::var("WEZTERM_PANE").unwrap_or_else(|_| "(not set)".into());
            println!("WEZTERM_PANE: {}", pane_env);
            println!("wezterm_path: {}", config.wezterm_path);

            let panes = get_wezterm_panes(&config.wezterm_path);
            println!("wezterm panes: {} found", panes.len());
            for p in &panes {
                let marker = if p.pane_id.to_string() == pane_env { " <-- self" } else { "" };
                println!("  pane={} tab={} win={} tty={} active={}{}", p.pane_id, p.tab_id, p.window_id, p.tty_name, p.is_active, marker);
            }

            let sessions = load_sessions_data(&config);
            println!("\nloaded sessions: {}", sessions.len());
            for s in &sessions {
                println!("  {} tab={} pane={} status={} dc={} stale={}", s.name, s.tab_id, s.pane_id, s.status, s.is_disconnected, s.is_stale);
            }
        }
        Some(Commands::Task { action }) => match action {
            TaskAction::Add {
                title,
                priority,
                due,
            } => {
                tasks::add_task(&config, title, priority, due);
            }
            TaskAction::Done { id } => {
                tasks::complete_task(&config, &id);
            }
            TaskAction::List => {
                tasks::list_tasks(&config);
            }
        },
        None => {
            run_tui(config)?;
        }
    }

    Ok(())
}
