use anyhow::Result;
use chrono::Local;
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

use crate::app::{App, FocusMode};
use crate::config::{expand_tilde, AppConfig};
use crate::session::{
    activate_pane, delete_session, get_sessions_file_path, load_sessions_data,
};
use crate::tasks::load_tasks;
use crate::types::AppEvent;
use crate::ui::{centered_rect, format_duration, render_status_bar, truncate_name};
use crate::usage::load_usage_data;

// ============================================================================
// Dock Mode Rendering
// ============================================================================

fn dock_ui(frame: &mut Frame, app: &mut App) {
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // Main area (3 columns)
            Constraint::Length(1), // Status bar
        ])
        .split(frame.area());

    let columns = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Percentage(15), // Usage
            Constraint::Percentage(20), // Tasks
            Constraint::Min(0),         // Sessions (残り)
        ])
        .split(main_layout[0]);

    render_dock_usage(frame, app, columns[0]);
    render_dock_tasks(frame, app, columns[1]);
    render_dock_sessions(frame, app, columns[2]);
    render_status_bar(frame, app, main_layout[1]);

    if app.show_help {
        render_dock_help_popup(frame);
    }
}

fn render_dock_usage(frame: &mut Frame, app: &App, area: Rect) {
    let now = Local::now();
    let time_str = now.format("%H:%M").to_string();

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

    if app.usage.sonnet >= 0 {
        let color = if app.usage.sonnet >= 80 {
            Color::Red
        } else if app.usage.sonnet >= 50 {
            Color::Yellow
        } else {
            Color::Green
        };
        lines.push(Line::from(Span::styled(
            format!(" 🎵 Son: {}%", app.usage.sonnet),
            Style::default().fg(color),
        )));
    }

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" 📊 Usage ");
    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_dock_tasks(frame: &mut Frame, app: &mut App, area: Rect) {
    let active_count = app.global_tasks.iter().filter(|t| t.status != "completed").count();
    let total_count = app.global_tasks.len();
    let max_title_len = (area.width as usize).saturating_sub(6); // icon + padding

    let items: Vec<ListItem> = if app.global_tasks.is_empty() {
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
                let title = truncate_name(&task.title, max_title_len);
                let text = format!("{} {}{}", priority_icon, title, status_text);

                let color = if let Some(ref due) = task.due_on {
                    if let Ok(due_date) = chrono::NaiveDate::parse_from_str(due, "%Y-%m-%d") {
                        let days_left = (due_date - today).num_days();
                        if days_left < 0 {
                            Color::Red
                        } else if days_left <= 3 {
                            Color::Yellow
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

fn render_dock_sessions(frame: &mut Frame, app: &mut App, area: Rect) {
    let visible = app.visible_sessions();
    let selected = app.session_state.selected().unwrap_or(0);

    let border_color = if app.focus_mode == FocusMode::Sessions {
        Color::Yellow
    } else {
        Color::Reset
    };

    let total = visible.len();
    let lines_per_session = 2usize;
    let inner_height = area.height.saturating_sub(2) as usize;
    let rows_visible = if inner_height > 0 { inner_height / lines_per_session } else { 0 };
    let total_per_page = if rows_visible > 0 { rows_visible * 2 } else { 1 };
    let total_pages = (total + total_per_page - 1) / total_per_page.max(1);
    let current_page = selected / total_per_page.max(1) + 1;

    let page_info = if total_pages > 1 {
        format!(" {}/{}", current_page, total_pages)
    } else {
        String::new()
    };

    let title = if app.show_stale {
        format!(" 🖥 Sessions [All] ({}){} ", total, page_info)
    } else {
        format!(" 🖥 Sessions ({}){} ", total, page_info)
    };

    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(border_color))
        .title(title);
    let inner = block.inner(area);
    frame.render_widget(block, area);

    if visible.is_empty() {
        let msg = Paragraph::new(Span::styled(
            "セッションなし",
            Style::default().fg(Color::DarkGray),
        ));
        frame.render_widget(msg, inner);
        return;
    }

    // Split into 2 columns
    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(50), Constraint::Percentage(50)])
        .split(inner);

    let highlight_style = Style::default()
        .bg(Color::DarkGray)
        .add_modifier(Modifier::BOLD);

    // Row-based layout: [0,1] [2,3] [4,5] ...
    // Left col: indices 0,2,4,...  Right col: indices 1,3,5,...
    if rows_visible == 0 {
        return;
    }
    let page = selected / total_per_page;
    let scroll_offset = page * total_per_page;

    for (col_idx, col_area) in cols.iter().enumerate() {
        let mut lines: Vec<Line> = Vec::new();
        for row in 0..rows_visible {
            let i = scroll_offset + row * 2 + col_idx;
            if i >= visible.len() {
                break;
            }

            let sess = visible[i];
            let is_selected = i == selected && app.focus_mode == FocusMode::Sessions;
            let base_style = if is_selected {
                highlight_style
            } else {
                Style::default()
            };

            // Line 1: marker + name + status
            let marker = if sess.is_disconnected {
                "⚫"
            } else if sess.is_yolo {
                "🤖"
            } else if sess.is_current {
                "🟢"
            } else {
                "🔵"
            };
            let status_icon = match sess.status.as_str() {
                "running" => "▶",
                "waiting_input" => "?",
                "stopped" => "■",
                _ => " ",
            };
            let max_name_len = (col_area.width as usize).saturating_sub(6);
            let name = truncate_name(&sess.name, max_name_len);
            lines.push(Line::from(Span::styled(
                format!("{} {} {}", marker, name, status_icon),
                base_style,
            )));

            // Line 2: duration + task/progress info
            let duration = format_duration(&sess.created_at);

            if let Some(ref task) = sess.active_task {
                let detail_style = if is_selected {
                    highlight_style
                } else {
                    Style::default().fg(Color::Yellow)
                };
                let max_task_len = (col_area.width as usize).saturating_sub(duration.len() + 5);
                lines.push(Line::from(Span::styled(
                    format!("  {} ⤷{}", duration, truncate_name(task, max_task_len)),
                    detail_style,
                )));
            } else if sess.tasks_total > 0 {
                let detail_style = if is_selected {
                    highlight_style
                } else if sess.tasks_completed == sess.tasks_total {
                    Style::default().fg(Color::Green)
                } else {
                    Style::default().fg(Color::Cyan)
                };
                let progress = if sess.tasks_completed == sess.tasks_total {
                    format!("  {} ✓ 完了", duration)
                } else {
                    format!("  {} {}/{}", duration, sess.tasks_completed, sess.tasks_total)
                };
                lines.push(Line::from(Span::styled(progress, detail_style)));
            } else {
                let detail_style = if is_selected {
                    highlight_style
                } else {
                    Style::default().fg(Color::DarkGray)
                };
                let suffix = if sess.is_disconnected {
                    " (dc)"
                } else if sess.is_stale {
                    " (stale)"
                } else {
                    ""
                };
                lines.push(Line::from(Span::styled(
                    format!("  {}{}", duration, suffix),
                    detail_style,
                )));
            }
        }

        let paragraph = Paragraph::new(lines);
        frame.render_widget(paragraph, *col_area);
    }
}

fn render_dock_help_popup(frame: &mut Frame) {
    let area = centered_rect(40, 10, frame.area());

    let lines = vec![
        Line::from(" Dock Mode"),
        Line::from(""),
        Line::from(" Tab/h/l  カラム移動"),
        Line::from(" j/k      リスト移動"),
        Line::from(" Enter    ペイン切替"),
        Line::from(" d/f/r    削除/全表示/更新"),
        Line::from(" q        終了"),
        Line::from(""),
    ];

    let block = Block::default()
        .borders(Borders::ALL)
        .title(" Help ");

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

// ============================================================================
// Event Handling
// ============================================================================

fn handle_dock_key(app: &mut App, key: event::KeyEvent) {
    if key.code == KeyCode::Char('?') {
        app.show_help = true;
        return;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Tab | KeyCode::Char('l') => {
            app.focus_mode = match app.focus_mode {
                FocusMode::Tasks => FocusMode::Sessions,
                FocusMode::Sessions => FocusMode::Tasks,
            };
        }
        KeyCode::Char('h') => {
            app.focus_mode = match app.focus_mode {
                FocusMode::Tasks => FocusMode::Sessions,
                FocusMode::Sessions => FocusMode::Tasks,
            };
        }
        KeyCode::Up | KeyCode::Char('k') => match app.focus_mode {
            FocusMode::Tasks => app.previous_task(),
            FocusMode::Sessions => app.previous_session(),
        },
        KeyCode::Down | KeyCode::Char('j') => match app.focus_mode {
            FocusMode::Tasks => app.next_task(),
            FocusMode::Sessions => app.next_session(),
        },
        KeyCode::Enter => {
            if app.focus_mode == FocusMode::Sessions {
                let visible = app.visible_sessions();
                if let Some(idx) = app.session_state.selected() {
                    if idx < visible.len() {
                        activate_pane(visible[idx], &app.config.wezterm_path);
                    }
                }
            }
        }
        KeyCode::Char('d') => {
            if app.focus_mode == FocusMode::Sessions {
                let visible = app.visible_sessions();
                if let Some(idx) = app.session_state.selected() {
                    if idx < visible.len() {
                        delete_session(visible[idx], &app.config.data_dir);
                        app.sessions = load_sessions_data(&app.config);
                    }
                }
            }
        }
        KeyCode::Char('f') => app.show_stale = !app.show_stale,
        KeyCode::Char('r') => {
            app.sessions = load_sessions_data(&app.config);
            app.global_tasks = load_tasks(&app.config);
            app.usage = load_usage_data();
        }
        _ => {}
    }
}

// ============================================================================
// Main Loop
// ============================================================================

pub fn run_dock(config: AppConfig) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(config);
    app.focus_mode = FocusMode::Tasks;
    app.sessions = load_sessions_data(&app.config);
    app.global_tasks = load_tasks(&app.config);

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
    let tx_sessions = tx.clone();
    let sessions_path = get_sessions_file_path(&app.config.data_dir);
    let sessions_dir = sessions_path.parent().unwrap().to_path_buf();
    thread::spawn(move || {
        let (watcher_tx, watcher_rx) = mpsc::channel();
        let mut watcher: RecommendedWatcher =
            Watcher::new(watcher_tx, NotifyConfig::default()).unwrap();
        let _ = fs::create_dir_all(&sessions_dir);
        let _ = watcher.watch(&sessions_dir, RecursiveMode::NonRecursive);

        loop {
            if let Ok(Ok(event)) = watcher_rx.recv() {
                if event
                    .paths
                    .iter()
                    .any(|p| p.file_name().map(|n| n == "sessions.json").unwrap_or(false))
                {
                    thread::sleep(Duration::from_millis(150));
                    let _ = tx_sessions.send(AppEvent::SessionsUpdated);
                    // Hook が発火した = Claude Code がアクティブ → usage 更新
                    let usage = load_usage_data();
                    if usage.five_hour >= 0 {
                        let _ = tx_sessions.send(AppEvent::UsageUpdated(usage));
                    }
                }
            }
        }
    });

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

    // Usage: initial load only (subsequent updates piggybacked on sessions watcher)
    app.usage = load_usage_data();

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
    loop {
        terminal.draw(|f| dock_ui(f, &mut app))?;

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(AppEvent::Tick) => {}
            Ok(AppEvent::Key(key)) => {
                if app.show_help {
                    app.show_help = false;
                } else {
                    handle_dock_key(&mut app, key);
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

    disable_raw_mode()?;
    execute!(
        terminal.backend_mut(),
        LeaveAlternateScreen,
        DisableMouseCapture
    )?;
    terminal.show_cursor()?;

    Ok(())
}
