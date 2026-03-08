use std::fs;
use std::io::{self, Write};
use std::path::PathBuf;

use crate::config::expand_tilde;

const CLAUDE_SETTINGS_PATH: &str = "~/.claude/settings.json";

const HOOK_EVENTS: &[&str] = &[
    "PreToolUse",
    "PostToolUse",
    "Notification",
    "Stop",
    "UserPromptSubmit",
];

/// Run the interactive init wizard
pub fn run_init() {
    println!("=== wez-sidebar setup ===\n");

    setup_hooks();
    println!();
    setup_tasks();
    println!();
    print_wezterm_guide();
}

/// Step 1: Hook registration
fn setup_hooks() {
    println!("📡 Step 1: Claude Code hooks\n");

    let settings_path = expand_tilde(CLAUDE_SETTINGS_PATH);

    // Check current state
    let (settings_exists, hooks_registered) = check_hooks_status(&settings_path);

    if hooks_registered {
        println!("  ✅ Hooks are already registered in {}", CLAUDE_SETTINGS_PATH);
        return;
    }

    if settings_exists {
        println!("  Found {}", CLAUDE_SETTINGS_PATH);
        println!("  ⚠️  Some hooks are missing.\n");
    } else {
        println!("  {} not found.\n", CLAUDE_SETTINGS_PATH);
    }

    print!("  Register hooks automatically? [Y/n] ");
    io::stdout().flush().unwrap();

    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    let input = input.trim().to_lowercase();

    if input.is_empty() || input == "y" || input == "yes" {
        if register_hooks(&settings_path, settings_exists) {
            println!("  ✅ Hooks registered successfully!");
        } else {
            println!("  ❌ Failed to register hooks. Please add manually:");
            print_manual_hooks();
        }
    } else {
        println!("\n  To register manually, add to {}:", CLAUDE_SETTINGS_PATH);
        print_manual_hooks();
    }
}

/// Check if hooks are already registered
fn check_hooks_status(settings_path: &PathBuf) -> (bool, bool) {
    let content = match fs::read_to_string(settings_path) {
        Ok(c) => c,
        Err(_) => return (false, false),
    };

    let all_present = HOOK_EVENTS
        .iter()
        .all(|event| content.contains(event) && content.contains("wez-sidebar hook"));

    (true, all_present)
}

/// Register hooks in settings.json
fn register_hooks(settings_path: &PathBuf, exists: bool) -> bool {
    let mut settings: serde_json::Value = if exists {
        match fs::read_to_string(settings_path) {
            Ok(content) => serde_json::from_str(&content).unwrap_or(serde_json::json!({})),
            Err(_) => serde_json::json!({}),
        }
    } else {
        serde_json::json!({})
    };

    let hooks = settings
        .as_object_mut()
        .unwrap()
        .entry("hooks")
        .or_insert(serde_json::json!({}));

    let hooks_obj = match hooks.as_object_mut() {
        Some(h) => h,
        None => return false,
    };

    for event in HOOK_EVENTS {
        let command = format!("~/.cargo/bin/wez-sidebar hook {}", event);
        let hook_entry = serde_json::json!([{
            "type": "command",
            "command": command
        }]);

        // Only add if not already present
        if !hooks_obj.contains_key(*event) {
            hooks_obj.insert(event.to_string(), hook_entry);
        }
    }

    let json = match serde_json::to_string_pretty(&settings) {
        Ok(j) => j,
        Err(_) => return false,
    };

    if let Some(parent) = settings_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    fs::write(settings_path, json).is_ok()
}

/// Print manual hook registration instructions
fn print_manual_hooks() {
    println!();
    println!("  ```json");
    println!("  {{");
    println!("    \"hooks\": {{");
    for (i, event) in HOOK_EVENTS.iter().enumerate() {
        let comma = if i < HOOK_EVENTS.len() - 1 { "," } else { "" };
        println!(
            "      \"{}\": [{{ \"type\": \"command\", \"command\": \"~/.cargo/bin/wez-sidebar hook {}\" }}]{}",
            event, event, comma
        );
    }
    println!("    }}");
    println!("  }}");
    println!("  ```");
}

/// Step 2: Task setup guide
fn setup_tasks() {
    println!("📋 Step 2: Task management (optional)\n");
    println!("  wez-sidebar can display tasks in the TUI panel.");
    println!("  Choose how you want to manage tasks:\n");
    println!("  [1] Built-in CLI (recommended for personal use)");
    println!("      $ wez-sidebar task add \"Implement auth\" -p 1 -d 2026-03-10");
    println!("      $ wez-sidebar task list");
    println!("      $ wez-sidebar task done <id>\n");
    println!("  [2] External JSON file (for tool integrations)");
    println!("      Any tool can write to the same JSON format:");
    println!("      {{\"tasks\": [{{\"id\": \"1\", \"title\": \"...\", \"status\": \"pending\", \"priority\": 1}}]}}\n");
    println!("  [3] REST API (for remote task sources)");
    println!("      Set api_url in config.toml to fetch tasks from GET {{api_url}}/api/tasks/cache\n");
    println!("  [0] Skip - I don't need tasks\n");

    print!("  Choose [0-3, default: 0] ");
    io::stdout().flush().unwrap();

    let mut input = String::new();
    io::stdin().read_line(&mut input).unwrap();
    let choice = input.trim();

    match choice {
        "1" | "2" => {
            create_tasks_config(false);
            if choice == "1" {
                println!("\n  Try it now:");
                println!("    $ wez-sidebar task add \"My first task\" -p 1");
            }
        }
        "3" => {
            create_tasks_config(true);
        }
        _ => {
            println!("  Skipped. You can enable tasks later in ~/.config/wez-sidebar/config.toml");
        }
    }
}

/// Create config.toml with tasks_file (and optionally api_url placeholder)
fn create_tasks_config(with_api: bool) {
    let config_path = expand_tilde("~/.config/wez-sidebar/config.toml");

    let existing = fs::read_to_string(&config_path).unwrap_or_default();

    if existing.contains("tasks_file") {
        println!("  ✅ tasks_file is already configured in config.toml");
        return;
    }

    let mut additions = String::new();
    additions.push_str("tasks_file = \"~/.config/wez-sidebar/tasks.json\"\n");
    if with_api {
        additions.push_str("# api_url = \"http://localhost:3000\"\n");
    }

    let new_content = if existing.is_empty() {
        additions
    } else {
        format!("{}\n{}", existing.trim_end(), additions)
    };

    if let Some(parent) = config_path.parent() {
        let _ = fs::create_dir_all(parent);
    }

    match fs::write(&config_path, new_content) {
        Ok(_) => println!("  ✅ Updated {}", config_path.display()),
        Err(e) => println!("  ❌ Failed to write config: {}", e),
    }
}

/// Step 3: WezTerm configuration guide
fn print_wezterm_guide() {
    println!("🖥️  Step 3: WezTerm keybinding\n");
    println!("  Add a keybinding to your wezterm.lua to open the sidebar:\n");
    println!("  ```lua");
    println!("  {{");
    println!("    key = \"b\",");
    println!("    mods = \"LEADER\",");
    println!("    action = wezterm.action_callback(function(window, pane)");
    println!("      local tab = window:active_tab()");
    println!("      tab:active_pane():split({{ direction = \"Right\", args = {{ \"wez-sidebar\" }} }})");
    println!("    end),");
    println!("  }}");
    println!("  ```\n");
    println!("  Or for dock mode (bottom bar):\n");
    println!("  ```lua");
    println!("  args = {{ \"wez-sidebar\", \"dock\" }}");
    println!("  ```\n");
    println!("✨ Setup complete! Start Claude Code and open the sidebar in WezTerm.");
}
