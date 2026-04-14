use serde::Deserialize;
use std::process::{Command, Stdio};

// ============================================================================
// Shared helpers
// ============================================================================

/// Resolve binary path using `which`, falling back to the bare name
fn detect_binary_path(name: &str) -> String {
    Command::new("which")
        .arg(name)
        .stderr(Stdio::null())
        .output()
        .ok()
        .and_then(|o| {
            if o.status.success() {
                Some(String::from_utf8_lossy(&o.stdout).trim().to_string())
            } else {
                None
            }
        })
        .unwrap_or_else(|| name.to_string())
}

/// Quote an argument for safe inclusion in a shell command line.
/// Wraps the argument in single quotes and escapes any embedded single quotes.
fn shell_quote_single(s: &str) -> String {
    let escaped = s.replace('\'', "'\\''");
    format!("'{}'", escaped)
}

/// Remove trailing empty lines from a line buffer
fn trim_trailing_empty_lines(lines: Vec<String>) -> Vec<String> {
    if lines.is_empty() {
        return lines;
    }
    let last_non_empty = lines.iter().rposition(|l| !l.trim().is_empty());
    match last_non_empty {
        Some(idx) => lines[..=idx].to_vec(),
        None => vec![],
    }
}

// ============================================================================
// Terminal Pane (generic, terminal-agnostic)
// ============================================================================

#[derive(Debug, Clone)]
pub struct TerminalPane {
    pub window_id: i32,
    pub tab_id: i32,
    pub pane_id: i32,
    pub tty_name: String,
    #[allow(dead_code)]
    pub title: String,
    pub is_active: bool,
}

// ============================================================================
// TerminalBackend trait
// ============================================================================

pub trait TerminalBackend {
    /// List all panes in the terminal multiplexer
    fn list_panes(&self) -> Vec<TerminalPane>;

    /// Activate (focus) the given pane
    fn activate_pane(&self, tab_id: i32, pane_id: i32);

    /// Get the text content of a pane buffer
    fn get_pane_text(&self, pane_id: i32) -> Vec<String>;

    /// Get the current pane ID from environment (e.g. WEZTERM_PANE, TMUX_PANE)
    fn current_pane_id(&self) -> i32;

    /// Build a shell command that activates the given pane (for notifications)
    fn build_activate_command(&self, tab_id: i32, pane_id: i32) -> String;

    /// Build a shell command that activates + sends Enter key (for auto-approve)
    fn build_approve_command(&self, tab_id: i32, pane_id: i32) -> String;

    /// Spawn a new tab (or window) running `prog` in `cwd`.
    /// Returns the new pane_id on success, or None on failure.
    ///
    /// `new_window` selects a new OS window instead of a new tab where the
    /// backend supports the distinction (tmux treats both as new-window).
    fn spawn_pane(&self, cwd: &str, prog: &[&str], new_window: bool) -> Option<i32>;

    /// Set the title of the tab containing the given pane. No-op on failure.
    fn set_tab_title(&self, pane_id: i32, title: &str);

    /// Paste `text` into `pane_id`. When `submit` is true, append Enter so the
    /// recipient treats the text as a submitted line (e.g. a Claude Code
    /// prompt). Best-effort — errors are swallowed.
    fn send_text(&self, pane_id: i32, text: &str, submit: bool);

    /// Name of the terminal (for diagnostics and logging)
    fn name(&self) -> &str;

    // -- Default implementations --

    /// Find a pane by its TTY path
    fn find_pane_by_tty(&self, tty: &str) -> Option<(i32, i32)> {
        if tty.is_empty() {
            return None;
        }
        self.list_panes()
            .iter()
            .find(|p| p.tty_name == tty)
            .map(|p| (p.tab_id, p.pane_id))
    }
}

// ============================================================================
// WezTerm Backend
// ============================================================================

#[derive(Debug, Clone, Deserialize)]
struct WezTermPaneJson {
    window_id: i32,
    tab_id: i32,
    pane_id: i32,
    tty_name: String,
    #[allow(dead_code)]
    title: String,
    is_active: bool,
}

pub struct WezTermBackend {
    path: String,
}

impl WezTermBackend {
    pub fn new(path: String) -> Self {
        Self { path }
    }

    pub fn auto_detect() -> Self {
        Self { path: detect_binary_path("wezterm") }
    }
}

impl TerminalBackend for WezTermBackend {
    fn list_panes(&self) -> Vec<TerminalPane> {
        let output = Command::new(&self.path)
            .args(["cli", "list", "--format", "json"])
            .stderr(Stdio::null())
            .output();

        match output {
            Ok(out) => {
                let panes: Vec<WezTermPaneJson> =
                    serde_json::from_slice(&out.stdout).unwrap_or_default();
                panes
                    .into_iter()
                    .map(|p| TerminalPane {
                        window_id: p.window_id,
                        tab_id: p.tab_id,
                        pane_id: p.pane_id,
                        tty_name: p.tty_name,
                        title: p.title,
                        is_active: p.is_active,
                    })
                    .collect()
            }
            Err(_) => Vec::new(),
        }
    }

    fn activate_pane(&self, tab_id: i32, pane_id: i32) {
        let _ = Command::new(&self.path)
            .args(["cli", "activate-tab", "--tab-id", &tab_id.to_string()])
            .stderr(Stdio::null())
            .output();
        let _ = Command::new(&self.path)
            .args(["cli", "activate-pane", "--pane-id", &pane_id.to_string()])
            .stderr(Stdio::null())
            .output();
    }

    fn get_pane_text(&self, pane_id: i32) -> Vec<String> {
        if pane_id < 0 {
            return vec!["(disconnected)".to_string()];
        }
        let output = Command::new(&self.path)
            .args(["cli", "get-text", "--pane-id", &pane_id.to_string()])
            .stderr(Stdio::null())
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let lines = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(|l| l.to_string())
                    .collect();
                trim_trailing_empty_lines(lines)
            }
            _ => vec!["(取得失敗)".to_string()],
        }
    }

    fn current_pane_id(&self) -> i32 {
        std::env::var("WEZTERM_PANE")
            .ok()
            .and_then(|s| s.parse().ok())
            .unwrap_or(-1)
    }

    fn build_activate_command(&self, tab_id: i32, pane_id: i32) -> String {
        format!(
            "{} cli activate-tab --tab-id {} && {} cli activate-pane --pane-id {}",
            self.path, tab_id, self.path, pane_id
        )
    }

    fn build_approve_command(&self, tab_id: i32, pane_id: i32) -> String {
        let activate = self.build_activate_command(tab_id, pane_id);
        format!(
            "{} && {} cli send-text --pane-id {} --no-paste $'\\n'",
            activate, self.path, pane_id
        )
    }

    fn spawn_pane(&self, cwd: &str, prog: &[&str], new_window: bool) -> Option<i32> {
        let mut args: Vec<&str> = vec!["cli", "spawn", "--cwd", cwd];
        if new_window {
            args.push("--new-window");
        }
        args.push("--");
        args.extend_from_slice(prog);

        let output = Command::new(&self.path)
            .args(&args)
            .stderr(Stdio::null())
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        String::from_utf8_lossy(&output.stdout)
            .trim()
            .parse::<i32>()
            .ok()
    }

    fn set_tab_title(&self, pane_id: i32, title: &str) {
        let _ = Command::new(&self.path)
            .args([
                "cli",
                "set-tab-title",
                "--pane-id",
                &pane_id.to_string(),
                title,
            ])
            .stderr(Stdio::null())
            .output();
    }

    fn send_text(&self, pane_id: i32, text: &str, submit: bool) {
        if pane_id < 0 {
            return;
        }
        // `cli send-text --no-paste` streams stdin to the pane as typed input,
        // so any characters (including newlines) are sent verbatim. We pipe
        // the prompt via stdin to avoid argv quoting pitfalls.
        use std::io::Write;
        let mut child = match Command::new(&self.path)
            .args([
                "cli",
                "send-text",
                "--pane-id",
                &pane_id.to_string(),
                "--no-paste",
            ])
            .stdin(Stdio::piped())
            .stderr(Stdio::null())
            .stdout(Stdio::null())
            .spawn()
        {
            Ok(c) => c,
            Err(_) => return,
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
            if submit {
                let _ = stdin.write_all(b"\r");
            }
        }
        let _ = child.wait();
    }

    fn name(&self) -> &str {
        "wezterm"
    }
}

// ============================================================================
// tmux Backend
// ============================================================================

pub struct TmuxBackend {
    path: String,
}

impl TmuxBackend {
    pub fn new(path: String) -> Self {
        Self { path }
    }

    pub fn auto_detect() -> Self {
        Self { path: detect_binary_path("tmux") }
    }
}

impl TerminalBackend for TmuxBackend {
    fn list_panes(&self) -> Vec<TerminalPane> {
        // tmux list-panes -a -F '#{session_id} #{window_id} #{pane_id} #{pane_tty} #{pane_title} #{pane_active}'
        let output = Command::new(&self.path)
            .args([
                "list-panes",
                "-a",
                "-F",
                "#{session_id}\t#{window_index}\t#{pane_index}\t#{pane_tty}\t#{pane_title}\t#{pane_active}",
            ])
            .stderr(Stdio::null())
            .output();

        let output = match output {
            Ok(o) if o.status.success() => o,
            _ => return Vec::new(),
        };

        let text = String::from_utf8_lossy(&output.stdout);
        let mut panes = Vec::new();

        for line in text.lines() {
            let parts: Vec<&str> = line.splitn(6, '\t').collect();
            if parts.len() < 6 {
                continue;
            }
            // session_id is like "$0", strip the "$"
            let window_id = parts[0]
                .strip_prefix('$')
                .and_then(|s| s.parse().ok())
                .unwrap_or(0);
            let tab_id = parts[1].parse().unwrap_or(0);
            let pane_id = parts[2].parse().unwrap_or(0);
            let tty_name = parts[3].to_string();
            let title = parts[4].to_string();
            let is_active = parts[5] == "1";

            panes.push(TerminalPane {
                window_id,
                tab_id,
                pane_id,
                tty_name,
                title,
                is_active,
            });
        }

        panes
    }

    fn activate_pane(&self, _tab_id: i32, pane_id: i32) {
        // tmux select-pane -t %<pane_id>
        let _ = Command::new(&self.path)
            .args(["select-pane", "-t", &format!("%{}", pane_id)])
            .stderr(Stdio::null())
            .output();
    }

    fn get_pane_text(&self, pane_id: i32) -> Vec<String> {
        if pane_id < 0 {
            return vec!["(disconnected)".to_string()];
        }
        // tmux capture-pane -t %<pane_id> -p
        let output = Command::new(&self.path)
            .args(["capture-pane", "-t", &format!("%{}", pane_id), "-p"])
            .stderr(Stdio::null())
            .output();

        match output {
            Ok(out) if out.status.success() => {
                let lines = String::from_utf8_lossy(&out.stdout)
                    .lines()
                    .map(|l| l.to_string())
                    .collect();
                trim_trailing_empty_lines(lines)
            }
            _ => vec!["(取得失敗)".to_string()],
        }
    }

    fn current_pane_id(&self) -> i32 {
        // TMUX_PANE is like "%5"
        std::env::var("TMUX_PANE")
            .ok()
            .and_then(|s| s.strip_prefix('%').and_then(|n| n.parse().ok()))
            .unwrap_or(-1)
    }

    fn build_activate_command(&self, _tab_id: i32, pane_id: i32) -> String {
        format!("{} select-pane -t %{}", self.path, pane_id)
    }

    fn build_approve_command(&self, _tab_id: i32, pane_id: i32) -> String {
        format!(
            "{} select-pane -t %{} && {} send-keys -t %{} Enter",
            self.path, pane_id, self.path, pane_id
        )
    }

    fn spawn_pane(&self, cwd: &str, prog: &[&str], _new_window: bool) -> Option<i32> {
        // tmux has no native "new OS window" concept; both tab and window map to
        // new-window. The initial pane of the new window becomes the spawn target.
        let cmd_str = prog
            .iter()
            .map(|s| shell_quote_single(s))
            .collect::<Vec<_>>()
            .join(" ");

        let output = Command::new(&self.path)
            .args([
                "new-window",
                "-c",
                cwd,
                "-P",
                "-F",
                "#{pane_id}",
                &cmd_str,
            ])
            .stderr(Stdio::null())
            .output()
            .ok()?;

        if !output.status.success() {
            return None;
        }

        // Output is "%5" — strip the '%' prefix
        String::from_utf8_lossy(&output.stdout)
            .trim()
            .strip_prefix('%')
            .and_then(|s| s.parse::<i32>().ok())
    }

    fn set_tab_title(&self, pane_id: i32, title: &str) {
        let _ = Command::new(&self.path)
            .args(["rename-window", "-t", &format!("%{}", pane_id), title])
            .stderr(Stdio::null())
            .output();
    }

    fn send_text(&self, pane_id: i32, text: &str, submit: bool) {
        if pane_id < 0 {
            return;
        }
        // tmux send-keys handles the literal text as one argument; use `-l`
        // (literal) to avoid keyname interpretation of embedded specials.
        let target = format!("%{}", pane_id);
        let _ = Command::new(&self.path)
            .args(["send-keys", "-t", &target, "-l", text])
            .stderr(Stdio::null())
            .output();
        if submit {
            let _ = Command::new(&self.path)
                .args(["send-keys", "-t", &target, "Enter"])
                .stderr(Stdio::null())
                .output();
        }
    }

    fn name(&self) -> &str {
        "tmux"
    }
}

// ============================================================================
// Factory
// ============================================================================

pub fn create_backend(backend_name: &str, terminal_path: &str) -> Box<dyn TerminalBackend> {
    match backend_name {
        "tmux" => {
            if terminal_path.is_empty() {
                Box::new(TmuxBackend::auto_detect())
            } else {
                Box::new(TmuxBackend::new(terminal_path.to_string()))
            }
        }
        _ => {
            // Default: wezterm
            if terminal_path.is_empty() {
                Box::new(WezTermBackend::auto_detect())
            } else {
                Box::new(WezTermBackend::new(terminal_path.to_string()))
            }
        }
    }
}
