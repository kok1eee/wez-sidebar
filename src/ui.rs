use anyhow::Result;
use chrono::{DateTime, Local, Utc};
use crossterm::{
    event::{self, DisableMouseCapture, EnableMouseCapture, Event, KeyCode, KeyEventKind},
    execute,
    terminal::{disable_raw_mode, enable_raw_mode, EnterAlternateScreen, LeaveAlternateScreen},
};
use notify::{Config as NotifyConfig, RecommendedWatcher, RecursiveMode, Watcher};
use ratatui::{
    backend::CrosstermBackend,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, List, ListItem, Paragraph},
    Frame, Terminal,
};
use std::{
    fs, io,
    sync::mpsc,
    thread,
    time::Duration,
};

use crate::api_client;
use crate::app::{App, FocusMode};
use crate::config::{expand_tilde, AppConfig};
use crate::session::{
    activate_pane, delete_session, get_pane_text, get_sessions_file_path, load_sessions_data,
};
use crate::tasks::load_tasks;
use crate::types::{AppEvent, SessionItem};
use crate::usage::load_usage_from_cache;

// ============================================================================
// TUI Rendering
// ============================================================================

fn ui(frame: &mut Frame, app: &mut App) {
    if app.show_preview {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),  // Usage
                Constraint::Length(12), // Tasks (10 items)
                Constraint::Min(5),     // Sessions (smaller)
                Constraint::Length(12), // Preview
                Constraint::Length(1),  // Status bar
            ])
            .split(frame.area());

        render_usage(frame, app, chunks[0]);
        render_tasks(frame, app, chunks[1]);
        render_sessions(frame, app, chunks[2]);
        render_preview(frame, app, chunks[3]);
        render_status_bar(frame, app, chunks[4]);
    } else {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5),  // Usage
                Constraint::Length(12), // Tasks (10 items)
                Constraint::Min(10),    // Sessions
                Constraint::Length(1),  // Status bar
            ])
            .split(frame.area());

        render_usage(frame, app, chunks[0]);
        render_tasks(frame, app, chunks[1]);
        render_sessions(frame, app, chunks[2]);
        render_status_bar(frame, app, chunks[3]);
    }

    if app.show_help {
        render_help_popup(frame, app);
    }
}

fn render_usage(frame: &mut Frame, app: &App, area: Rect) {
    let now = Local::now();
    let time_str = now.format("%H:%M:%S").to_string();

    let mut lines = vec![Line::from(format!(" 🕐 {}", time_str))];

    if app.usage.five_hour >= 0 {
        let color = if app.usage.five_hour >= 80 {
            Color::Red
        } else if app.usage.five_hour >= 50 {
            Color::Yellow
        } else {
            Color::Green
        };
        let mut text = format!(" ⏳ 5h: {}%", app.usage.five_hour);
        if !app.usage.five_hour_reset.is_empty() {
            text.push_str(&format!(" ({})", app.usage.five_hour_reset));
        }
        lines.push(Line::from(Span::styled(text, Style::default().fg(color))));
    }

    if app.usage.weekly >= 0 {
        let color = if app.usage.weekly >= 80 {
            Color::Red
        } else if app.usage.weekly >= 50 {
            Color::Yellow
        } else {
            Color::Green
        };
        let mut text = format!(" 📅 All: {}%", app.usage.weekly);
        if !app.usage.weekly_reset.is_empty() {
            text.push_str(&format!(" ({})", app.usage.weekly_reset));
        }
        lines.push(Line::from(Span::styled(text, Style::default().fg(color))));
    }


    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 📊 Usage ");
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_tasks(frame: &mut Frame, app: &mut App, area: Rect) {
    let active_count = app.global_tasks.iter().filter(|t| t.status != "completed").count();
    let total_count = app.global_tasks.len();

    let tasks_configured = app.config.tasks_file.is_some() || app.config.api_url.is_some();

    let items: Vec<ListItem> = if !tasks_configured {
        vec![ListItem::new(Span::styled(
            "tasks_file 未設定",
            Style::default().fg(Color::DarkGray),
        ))]
    } else if app.global_tasks.is_empty() {
        vec![ListItem::new(Span::styled(
            "タスクなし",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        let today = Local::now().date_naive();
        app.global_tasks
            .iter()
            .map(|task| {
                let priority_icon = match task.priority {
                    1 => "🔴",
                    3 => "🟢",
                    _ => "🟡",
                };
                let status_text = if task.status == "in_progress" { " ▶" } else { "" };
                let title = truncate_name(&task.title, 24);
                let text = format!("{} {}{}", priority_icon, title, status_text);

                // Color based on deadline
                let color = if let Some(ref due) = task.due_on {
                    if let Ok(due_date) = chrono::NaiveDate::parse_from_str(due, "%Y-%m-%d") {
                        let days_left = (due_date - today).num_days();
                        if days_left < 0 {
                            Color::Red        // overdue
                        } else if days_left <= 3 {
                            Color::Yellow     // due soon
                        } else {
                            Color::Reset
                        }
                    } else {
                        Color::Reset
                    }
                } else {
                    Color::Reset
                };

                ListItem::new(Span::styled(text, Style::default().fg(color)))
            })
            .collect()
    };

    let border_color = if app.focus_mode == FocusMode::Tasks {
        Color::Yellow
    } else {
        Color::Reset
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(format!(" 📋 Tasks ({}/{}) ", active_count, total_count));

    let highlight_style = if app.focus_mode == FocusMode::Tasks {
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style);

    frame.render_stateful_widget(list, area, &mut app.task_state);
}

pub fn format_duration(created_at: &DateTime<Utc>) -> String {
    let elapsed = Utc::now().signed_duration_since(*created_at);
    let mins = elapsed.num_minutes();
    if mins < 60 {
        format!("{}m", mins)
    } else {
        format!("{}h{}m", mins / 60, mins % 60)
    }
}

fn render_sessions(frame: &mut Frame, app: &mut App, area: Rect) {
    let visible = app.visible_sessions();

    let items: Vec<ListItem> = visible
        .iter()
        .map(|sess| {
            // Line 1: marker + directory name
            let marker = if sess.is_disconnected {
                "⚫"
            } else if sess.is_yolo {
                "🤖"
            } else if sess.is_current {
                "🟢"
            } else {
                "🔵"
            };

            let name = truncate_name(&sess.name, 18);
            let main_text = format!("{} {}", marker, name);
            let mut lines = vec![Line::from(main_text)];

            // Line 2: status + progress or task info
            let status_icon = match sess.status.as_str() {
                "running" => "▶",
                "waiting_input" => "?",
                "stopped" => "■",
                _ => " ",
            };

            let duration = format_duration(&sess.created_at);

            if sess.tasks_total > 0 {
                let progress_bar = render_progress_bar(sess.tasks_completed, sess.tasks_total, 10);
                lines.push(Line::from(Span::styled(
                    format!("  {} {} {} {}/{}", status_icon, duration, progress_bar, sess.tasks_completed, sess.tasks_total),
                    Style::default().fg(Color::Cyan),
                )));
                // Line 3: Active task name
                if let Some(ref task) = sess.active_task {
                    lines.push(Line::from(Span::styled(
                        format!("  ⤷ {}", truncate_name(task, 20)),
                        Style::default().fg(Color::Yellow),
                    )));
                } else if sess.tasks_completed == sess.tasks_total {
                    lines.push(Line::from(Span::styled(
                        "  ✓ 完了".to_string(),
                        Style::default().fg(Color::Green),
                    )));
                }
            } else if sess.is_disconnected {
                lines.push(Line::from(Span::styled(
                    format!("  {} {} (disconnected)", status_icon, duration),
                    Style::default().fg(Color::DarkGray),
                )));
            } else if sess.is_stale {
                lines.push(Line::from(Span::styled(
                    format!("  {} {} (stale)", status_icon, duration),
                    Style::default().fg(Color::DarkGray),
                )));
            } else {
                lines.push(Line::from(Span::styled(
                    format!("  {} {}", status_icon, duration),
                    Style::default().fg(Color::DarkGray),
                )));
            }

            ListItem::new(lines)
        })
        .collect();

    // Helper function for progress bar
    fn render_progress_bar(completed: i32, total: i32, width: usize) -> String {
        if total == 0 {
            return format!("[{}]", "░".repeat(width));
        }
        let filled = ((completed as f64 / total as f64) * width as f64) as usize;
        let empty = width - filled;
        format!("[{}{}]", "█".repeat(filled), "░".repeat(empty))
    }

    let title = if app.show_stale {
        " 🖥 Sessions [All] "
    } else {
        " 🖥 Sessions [Active] "
    };

    let block = Block::default().borders(Borders::ALL).title(title);

    let highlight_style = if app.focus_mode == FocusMode::Sessions {
        Style::default()
            .bg(Color::DarkGray)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default()
    };

    let list = List::new(items)
        .block(block)
        .highlight_style(highlight_style);

    frame.render_stateful_widget(list, area, &mut app.session_state);
}

fn render_preview(frame: &mut Frame, app: &App, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize; // border top+bottom

    // Show last N lines (scroll from bottom)
    let total_lines = app.pane_preview.len();
    let max_scroll = total_lines.saturating_sub(inner_height) as u16;
    let scroll = app.preview_scroll.min(max_scroll);

    let start = scroll as usize;
    let end = (start + inner_height).min(total_lines);

    let lines: Vec<Line> = if app.pane_preview.is_empty() {
        vec![Line::from(Span::styled(
            "(no data)",
            Style::default().fg(Color::DarkGray),
        ))]
    } else {
        app.pane_preview[start..end]
            .iter()
            .map(|l| Line::from(l.as_str()))
            .collect()
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 👁 Preview ");
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

pub fn render_status_bar(frame: &mut Frame, app: &App, area: Rect) {
    let api_indicator = if app.config.api_url.is_some() {
        if app.api_connected {
            Span::styled("● ", Style::default().fg(Color::Green))
        } else {
            Span::styled("○ ", Style::default().fg(Color::Red))
        }
    } else {
        Span::raw("")
    };

    let text = Line::from(vec![
        api_indicator,
        Span::styled("?:help q:quit", Style::default().fg(Color::DarkGray)),
    ]);
    let paragraph = Paragraph::new(text);
    frame.render_widget(paragraph, area);
}

fn render_help_popup(frame: &mut Frame, app: &App) {
    let area = centered_rect(36, 14, frame.area());

    let lines = if app.focus_mode == FocusMode::Tasks {
        vec![
            Line::from(" 📋 Tasks Mode"),
            Line::from(""),
            Line::from(" j/k   上下移動"),
            Line::from(" Esc   セッションに戻る"),
            Line::from(" q     終了"),
            Line::from(""),
            Line::from(" Press any key to close"),
        ]
    } else {
        vec![
            Line::from(" 🖥 Sessions Mode"),
            Line::from(""),
            Line::from(" t       タスクモード"),
            Line::from(" p       プレビュー切替"),
            Line::from(" Enter   ペイン切替"),
            Line::from(" 1-9     番号でペイン切替"),
            Line::from(" d       セッション削除"),
            Line::from(" f       全表示切替"),
            Line::from(" r       更新"),
            Line::from(" q/Esc   終了"),
            Line::from(""),
            Line::from(" Press any key to close"),
        ]
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ");

    let paragraph = Paragraph::new(lines).block(block);

    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

pub fn centered_rect(width: u16, height: u16, area: Rect) -> Rect {
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + (area.height.saturating_sub(height)) / 2;
    Rect::new(x, y, width.min(area.width), height.min(area.height))
}

pub fn truncate_name(name: &str, max_len: usize) -> String {
    let chars: Vec<char> = name.chars().collect();
    if chars.len() <= max_len {
        name.to_string()
    } else {
        format!("{}…", chars[..max_len - 1].iter().collect::<String>())
    }
}

// ============================================================================
// Event Handling
// ============================================================================

fn update_preview(app: &mut App) {
    let visible = app.visible_sessions();
    if let Some(idx) = app.session_state.selected() {
        if idx < visible.len() {
            let pane_id = visible[idx].pane_id;
            app.pane_preview = get_pane_text(pane_id, &app.config.wezterm_path);
            // Auto-scroll to bottom
            app.preview_scroll = app.pane_preview.len().saturating_sub(1) as u16;
        } else {
            app.pane_preview.clear();
        }
    } else {
        app.pane_preview.clear();
    }
}

fn handle_key(app: &mut App, key: event::KeyEvent) {
    // Common keys for all modes
    if key.code == KeyCode::Char('?') {
        app.show_help = true;
        return;
    }

    match app.focus_mode {
        FocusMode::Tasks => handle_tasks_key(app, key),
        FocusMode::Sessions => handle_sessions_key(app, key),
    }
}

fn handle_sessions_key(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('t') => app.focus_mode = FocusMode::Tasks,
        KeyCode::Char('f') => app.show_stale = !app.show_stale,
        KeyCode::Char('p') => {
            app.show_preview = !app.show_preview;
            if app.show_preview {
                update_preview(app);
            }
        }
        KeyCode::Char('r') => {
            app.sessions = load_sessions_data(&app.config);
            app.global_tasks = load_tasks(&app.config);
            app.usage = load_usage_from_cache(&app.config.data_dir);
        }
        KeyCode::Char('d') => {
            let visible = app.visible_sessions();
            if let Some(idx) = app.session_state.selected() {
                if idx < visible.len() {
                    delete_session(visible[idx], &app.config.data_dir);
                    app.sessions = load_sessions_data(&app.config);
                }
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.previous_session();
            if app.show_preview {
                update_preview(app);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.next_session();
            if app.show_preview {
                update_preview(app);
            }
        }
        KeyCode::Enter => {
            let visible = app.visible_sessions();
            if let Some(idx) = app.session_state.selected() {
                if idx < visible.len() {
                    activate_pane(visible[idx], &app.config.wezterm_path);
                }
            }
        }
        KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
            let idx = (c as usize) - ('1' as usize);
            let visible: Vec<SessionItem> = app.visible_sessions().into_iter().cloned().collect();
            if idx < visible.len() {
                app.session_state.select(Some(idx));
                activate_pane(&visible[idx], &app.config.wezterm_path);
            }
        }
        _ => {}
    }
}

fn handle_tasks_key(app: &mut App, key: event::KeyEvent) {
    match key.code {
        KeyCode::Char('q') => app.should_quit = true,
        KeyCode::Esc => app.focus_mode = FocusMode::Sessions,
        KeyCode::Up | KeyCode::Char('k') => app.previous_task(),
        KeyCode::Down | KeyCode::Char('j') => app.next_task(),
        _ => {}
    }
}

// ============================================================================
// Main Loop
// ============================================================================

pub fn run_tui(config: AppConfig) -> Result<()> {
    // Setup terminal
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(config);

    // Load initial data
    app.sessions = load_sessions_data(&app.config);

    // Setup channels for events
    let (tx, rx) = mpsc::channel::<AppEvent>();

    // Tick thread
    let tx_tick = tx.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        let _ = tx_tick.send(AppEvent::Tick);
    });

    // Delayed reload to handle race condition when panes aren't ready yet
    let tx_delayed = tx.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(1));
        let _ = tx_delayed.send(AppEvent::SessionsUpdated);
    });

    // File watcher for sessions
    // data_dir watcher: sessions.json + usage-cache.json の変更を監視
    let tx_sessions = tx.clone();
    let sessions_path = get_sessions_file_path(&app.config.data_dir);
    let sessions_dir = sessions_path.parent().unwrap().to_path_buf();
    let sessions_api_url = app.config.api_url.clone();
    let sessions_data_dir = app.config.data_dir.clone();
    thread::spawn(move || {
        let (watcher_tx, watcher_rx) = mpsc::channel();
        let mut watcher: RecommendedWatcher =
            Watcher::new(watcher_tx, NotifyConfig::default()).unwrap();
        let _ = fs::create_dir_all(&sessions_dir);
        let _ = watcher.watch(&sessions_dir, RecursiveMode::NonRecursive);

        loop {
            if let Ok(Ok(event)) = watcher_rx.recv() {
                let is_sessions = event.paths.iter().any(|p| {
                    p.file_name().map(|n| n == "sessions.json").unwrap_or(false)
                });
                let is_usage = event.paths.iter().any(|p| {
                    p.file_name().map(|n| n == "usage-cache.json").unwrap_or(false)
                });

                if is_sessions {
                    thread::sleep(Duration::from_millis(150));
                    let _ = tx_sessions.send(AppEvent::SessionsUpdated);
                    if let Some(ref url) = sessions_api_url {
                        let connected = api_client::check_health(url);
                        let _ = tx_sessions.send(AppEvent::ApiStatusChanged(connected));
                    }
                }
                if is_usage {
                    thread::sleep(Duration::from_millis(100));
                    let usage = load_usage_from_cache(&sessions_data_dir);
                    if usage.five_hour >= 0 {
                        let _ = tx_sessions.send(AppEvent::UsageUpdated(usage));
                    }
                }
            }
        }
    });

    // Tasks: initial load from cache
    app.global_tasks = load_tasks(&app.config);

    // File watcher for tasks cache (only if tasks_file is configured)
    if let Some(ref tasks_file) = app.config.tasks_file {
        let tx_tasks = tx.clone();
        let tasks_config = app.config.clone();
        let tasks_path = expand_tilde(tasks_file);
        let watch_dir = tasks_path.parent().unwrap_or(&tasks_path).to_path_buf();
        let watch_filename = tasks_path
            .file_name()
            .map(|n| n.to_os_string())
            .unwrap_or_default();
        thread::spawn(move || {
            let (watcher_tx, watcher_rx) = mpsc::channel();
            let mut watcher: RecommendedWatcher =
                Watcher::new(watcher_tx, NotifyConfig::default()).unwrap();
            let _ = fs::create_dir_all(&watch_dir);
            let _ = watcher.watch(&watch_dir, RecursiveMode::NonRecursive);

            loop {
                if let Ok(Ok(event)) = watcher_rx.recv() {
                    if event
                        .paths
                        .iter()
                        .any(|p| p.file_name().map(|n| n == watch_filename).unwrap_or(false))
                    {
                        thread::sleep(Duration::from_millis(200));
                        let tasks = load_tasks(&tasks_config);
                        let _ = tx_tasks.send(AppEvent::TasksUpdated(tasks));
                    }
                }
            }
        });
    }

    // API health check thread (when api_url is configured)
    // sessions.json への書き込みはビルトイン hook が行う。
    // ここでは /health で EC2 接続状態を確認するのみ。
    if let Some(ref api_url) = app.config.api_url {
        let tx_api = tx.clone();
        let api_url = api_url.clone();
        thread::spawn(move || {
            loop {
                let connected = api_client::check_health(&api_url);
                let _ = tx_api.send(AppEvent::ApiStatusChanged(connected));
                thread::sleep(Duration::from_secs(3600));
            }
        });
    }

    // Usage: initial load only (subsequent updates piggybacked on sessions watcher)
    app.usage = load_usage_from_cache(&app.config.data_dir);

    // Key event thread
    let tx_key = tx.clone();
    thread::spawn(move || loop {
        if event::poll(Duration::from_millis(100)).unwrap() {
            if let Event::Key(key) = event::read().unwrap() {
                if key.kind == KeyEventKind::Press {
                    let _ = tx_key.send(AppEvent::Key(key));
                }
            }
        }
    });

    // Main loop
    let mut tick_count: u32 = 0;
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(AppEvent::Tick) => {
                tick_count = tick_count.wrapping_add(1);
                // Refresh preview every 3 seconds
                if app.show_preview && tick_count % 3 == 0 {
                    update_preview(&mut app);
                }
            }
            Ok(AppEvent::Key(key)) => {
                if app.show_help {
                    app.show_help = false;
                } else {
                    handle_key(&mut app, key);
                }
            }
            Ok(AppEvent::SessionsUpdated) => {
                app.sessions = load_sessions_data(&app.config);
            }
            Ok(AppEvent::TasksUpdated(tasks)) => {
                app.global_tasks = tasks;
            }
            Ok(AppEvent::UsageUpdated(usage)) => {
                app.usage = usage;
            }
            Ok(AppEvent::ApiStatusChanged(connected)) => {
                app.api_connected = connected;
            }
            Err(_) => {}
        }

        if app.should_quit {
            break;
        }
    }

    // Restore terminal
    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}
