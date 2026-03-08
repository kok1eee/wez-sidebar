mod app;
mod config;
mod dock;
mod hooks;
mod init;
mod session;
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
        None => {
            run_tui(config)?;
        }
    }

    Ok(())
}
