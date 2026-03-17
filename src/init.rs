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
    print_wezterm_guide();
}

/// Step 1: Hook registration
fn setup_hooks() {
    println!("📡 Step 1: Claude Code hooks\n");

    let settings_path = expand_tilde(CLAUDE_SETTINGS_PATH);

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

/// Step 2: WezTerm configuration guide
fn print_wezterm_guide() {
    println!("🖥️  Step 2: WezTerm keybinding\n");
    println!("  Add to your wezterm.lua — LEADER+t opens a mode picker:\n");
    println!("  ```lua");
    println!("  {{");
    println!("    key = \"t\",");
    println!("    mods = \"LEADER\",");
    println!("    action = wezterm.action_callback(function(window, pane)");
    println!("      -- Close existing wez-sidebar pane if any");
    println!("      local tab = window:active_tab()");
    println!("      for _, p in ipairs(tab:panes_with_info()) do");
    println!("        if p.pane:get_foreground_process_name():find(\"wez%-sidebar\") then");
    println!("          p.pane:activate()");
    println!("          window:perform_action(");
    println!("            wezterm.action.CloseCurrentPane {{ confirm = false }}, p.pane)");
    println!("          return");
    println!("        end");
    println!("      end");
    println!("      -- Show mode picker");
    println!("      window:perform_action(wezterm.action.InputSelector {{");
    println!("        title = \"wez-sidebar\",");
    println!("        choices = {{");
    println!("          {{ label = \"Right sidebar\",  id = \"right\" }},");
    println!("          {{ label = \"Left sidebar\",   id = \"left\" }},");
    println!("          {{ label = \"Bottom dock\",    id = \"dock\" }},");
    println!("        }},");
    println!("        action = wezterm.action_callback(function(inner_window, inner_pane, id)");
    println!("          if not id then return end");
    println!("          if id == \"right\" then");
    println!("            inner_pane:split({{ direction = \"Right\",");
    println!("              args = {{ \"wez-sidebar\" }} }})");
    println!("          elseif id == \"left\" then");
    println!("            inner_pane:split({{ direction = \"Left\",");
    println!("              args = {{ \"wez-sidebar\" }} }})");
    println!("          elseif id == \"dock\" then");
    println!("            inner_pane:split({{ direction = \"Bottom\",");
    println!("              args = {{ \"wez-sidebar\", \"dock\" }} }})");
    println!("          end");
    println!("        end),");
    println!("      }}, pane)");
    println!("    end),");
    println!("  }},");
    println!("  ```\n");
    println!("✨ Setup complete! Press LEADER+t to open/close wez-sidebar.");
}
