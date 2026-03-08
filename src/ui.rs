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
    widgets::{Block, BorderType, Borders, Clear, Paragraph},
    Frame, Terminal,
};
use std::{
    fs, io,
    sync::mpsc,
    thread,
    time::Duration,
};

use crate::app::App;
use crate::config::AppConfig;
use crate::session::{
    activate_pane, delete_session, get_pane_text, get_sessions_file_path,
    load_sessions_data,
};
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
                Constraint::Min(5),    // Sessions
                Constraint::Length(12), // Preview
                Constraint::Length(1),  // Status bar
            ])
            .split(frame.area());

        render_usage(frame, app, chunks[0]);
        render_sessions(frame, app, chunks[1]);
        render_preview(frame, app, chunks[2]);
        render_status_bar(frame, app, chunks[3]);
    } else {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Length(5), // Usage
                Constraint::Min(10),   // Sessions
                Constraint::Length(1), // Status bar
            ])
            .split(frame.area());

        render_usage(frame, app, chunks[0]);
        render_sessions(frame, app, chunks[1]);
        render_status_bar(frame, app, chunks[2]);
    }

    if app.show_help {
        render_help_popup(frame);
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

fn render_sessions(frame: &mut Frame, app: &mut App, area: Rect) {
    let visible = app.visible_sessions();
    let selected = app.session_state.selected().unwrap_or(0);
    let total = visible.len();

    let card_height = 5u16; // 2 border + 3 content (compact for sidebar)
    let cards_area_height = area.height.saturating_sub(1);
    let max_cards = if cards_area_height >= card_height {
        (cards_area_height / card_height) as usize
    } else {
        0
    };

    let scroll_hint = if total > max_cards { " ↕" } else { "" };
    let title = if app.show_stale {
        format!(" 🖥 Sessions [All] ({}){} ", total, scroll_hint)
    } else {
        format!(" 🖥 Sessions ({}){} ", total, scroll_hint)
    };

    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let cards_area = Rect::new(area.x, area.y + 1, area.width, cards_area_height);

    if visible.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                " No sessions",
                Style::default().fg(Color::DarkGray),
            )),
            cards_area,
        );
        return;
    }

    if max_cards == 0 {
        return;
    }

    let scroll_offset = if selected >= max_cards {
        selected - max_cards + 1
    } else {
        0
    };

    for (vi, i) in (scroll_offset..).take(max_cards).enumerate() {
        if i >= visible.len() {
            break;
        }
        let sess = visible[i];
        let is_selected = i == selected;

        let y = cards_area.y + (vi as u16 * card_height);
        if y + card_height > cards_area.y + cards_area.height {
            break;
        }
        let card_area = Rect::new(cards_area.x, y, cards_area.width, card_height);
        render_session_card(frame, sess, is_selected, app.tick, card_area);
    }
}

pub const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn render_session_card(frame: &mut Frame, sess: &SessionItem, is_selected: bool, tick: u32, area: Rect) {
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
        "running" => {
            let ch = SPINNER_FRAMES[(tick as usize) % SPINNER_FRAMES.len()];
            format!(" {}", ch)
        }
        "waiting_input" => " ?".to_string(),
        "stopped" => " ■".to_string(),
        _ => String::new(),
    };

    let base_color = match sess.status.as_str() {
        "running" => Color::Green,
        "waiting_input" => Color::Yellow,
        "stopped" => Color::DarkGray,
        _ if sess.is_disconnected => Color::DarkGray,
        _ => Color::Gray,
    };

    let border_style = if is_selected {
        let bright = match base_color {
            Color::Green => Color::LightGreen,
            Color::Yellow => Color::LightYellow,
            Color::DarkGray => Color::Gray,
            _ => base_color,
        };
        Style::default().fg(bright).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(base_color)
    };

    // inner_width = card width minus left/right borders
    let inner_w = (area.width as usize).saturating_sub(2);

    let max_name_len = inner_w.saturating_sub(8); // marker(2) + spaces(2) + status_icon(~3) + padding
    let name = truncate_name(&sess.name, max_name_len);
    let card_title = format!(" {}{}{} ", marker, name, status_icon);

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(card_title);

    let duration = format_duration(&sess.created_at);
    let mut lines = Vec::new();

    // Line 1: duration + git branch
    let mut line1_spans = vec![Span::styled(
        format!(" {}", duration),
        Style::default().fg(Color::DarkGray),
    )];
    if let Some(ref branch) = sess.git_branch {
        let used = duration.len() + 2; // " " prefix + duration
        let max_branch = inner_w.saturating_sub(used + 2);
        if max_branch > 3 {
            line1_spans.push(Span::styled(
                format!(" {}", truncate_name(branch, max_branch)),
                Style::default().fg(Color::Magenta),
            ));
        }
    }
    lines.push(Line::from(line1_spans));

    // Line 2: last activity (red if dangerous) or status hint
    if let Some(ref activity) = sess.last_activity {
        let color = if sess.is_dangerous { Color::Red } else { Color::Cyan };
        let prefix = if sess.is_dangerous { " ⚠ " } else { " " };
        let prefix_w = if sess.is_dangerous { 4 } else { 1 };
        let max_len = inner_w.saturating_sub(prefix_w);
        lines.push(Line::from(Span::styled(
            format!("{}{}", prefix, truncate_name(activity, max_len)),
            Style::default().fg(color),
        )));
    } else if sess.is_disconnected {
        lines.push(Line::from(Span::styled(
            " disconnected",
            Style::default().fg(Color::DarkGray),
        )));
    } else if sess.is_stale {
        lines.push(Line::from(Span::styled(
            " stale",
            Style::default().fg(Color::DarkGray),
        )));
    } else {
        lines.push(Line::from(""));
    }

    // Remaining lines: user message + recent activity history (dock)
    let inner_h = (area.height as usize).saturating_sub(2);
    let mut remaining = inner_h.saturating_sub(lines.len());

    // User message with elapsed time, or cwd fallback
    if remaining > 0 {
        if let Some(ref msg) = sess.last_user_message {
            let elapsed = sess.last_user_message_at
                .map(format_elapsed)
                .unwrap_or_default();
            let suffix_len = if elapsed.is_empty() { 0 } else { elapsed.len() + 2 }; // " (Xm前)"
            let max_msg = inner_w.saturating_sub(1 + suffix_len);
            let truncated = truncate_name(msg, max_msg);
            if elapsed.is_empty() {
                lines.push(Line::from(Span::styled(
                    format!(" {}", truncated),
                    Style::default().fg(Color::White),
                )));
            } else {
                lines.push(Line::from(vec![
                    Span::styled(format!(" {}", truncated), Style::default().fg(Color::White)),
                    Span::styled(format!(" ({})", elapsed), Style::default().fg(Color::DarkGray)),
                ]));
            }
        } else {
            let home = dirs::home_dir().map(|h| h.to_string_lossy().to_string()).unwrap_or_default();
            let display_cwd = if sess.home_cwd.starts_with(&home) {
                format!("~{}", &sess.home_cwd[home.len()..])
            } else {
                sess.home_cwd.clone()
            };
            lines.push(Line::from(Span::styled(
                format!(" {}", truncate_name(&display_cwd, inner_w.saturating_sub(1))),
                Style::default().fg(Color::DarkGray),
            )));
        }
        remaining -= 1;
    }

    // Extra lines (dock mode has more space): show tasks
    for task in sess.tasks.iter().take(remaining) {
        let (icon, color) = match task.status.as_str() {
            "completed" => ("✓", Color::Green),
            "in_progress" => ("●", Color::Cyan),
            _ => ("○", Color::DarkGray),
        };
        lines.push(Line::from(Span::styled(
            format!(" {} {}", icon, truncate_name(&task.content, inner_w.saturating_sub(4))),
            Style::default().fg(color),
        )));
    }

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

fn render_preview(frame: &mut Frame, app: &App, area: Rect) {
    let inner_height = area.height.saturating_sub(2) as usize;

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
    let usage_spans = format_usage_spans(app);
    let mut spans = usage_spans;
    spans.push(Span::styled(" ?:help q:quit", Style::default().fg(Color::DarkGray)));

    let text = Line::from(spans);
    let paragraph = Paragraph::new(text);
    frame.render_widget(paragraph, area);
}

/// Format usage as compact spans for status bar
pub fn format_usage_spans(app: &App) -> Vec<Span<'static>> {
    let mut spans = Vec::new();

    if app.usage.five_hour >= 0 {
        let color = if app.usage.five_hour >= 80 {
            Color::Red
        } else if app.usage.five_hour >= 50 {
            Color::Yellow
        } else {
            Color::Green
        };
        let mut text = format!("⏳{}%", app.usage.five_hour);
        if !app.usage.five_hour_reset.is_empty() {
            text.push_str(&format!("({})", app.usage.five_hour_reset));
        }
        spans.push(Span::styled(text, Style::default().fg(color)));
        spans.push(Span::raw(" "));
    }

    if app.usage.weekly >= 0 {
        let color = if app.usage.weekly >= 80 {
            Color::Red
        } else if app.usage.weekly >= 50 {
            Color::Yellow
        } else {
            Color::Green
        };
        let text = format!("📅{}%", app.usage.weekly);
        spans.push(Span::styled(text, Style::default().fg(color)));
        spans.push(Span::raw(" "));
    }

    spans
}

fn render_help_popup(frame: &mut Frame) {
    let area = centered_rect(36, 12, frame.area());

    let lines = vec![
        Line::from(" 🖥 Sessions"),
        Line::from(""),
        Line::from(" j/k     up/down"),
        Line::from(" Enter   switch pane"),
        Line::from(" 1-9     switch by number"),
        Line::from(" p       toggle preview"),
        Line::from(" d       delete session"),
        Line::from(" f       show all/active"),
        Line::from(" r       refresh"),
        Line::from(" q/Esc   quit"),
    ];

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

pub fn format_elapsed(at: DateTime<Utc>) -> String {
    let mins = Utc::now().signed_duration_since(at).num_minutes();
    if mins < 1 {
        "now".to_string()
    } else if mins < 60 {
        format!("{}m前", mins)
    } else {
        format!("{}h前", mins / 60)
    }
}

pub fn truncate_name(name: &str, max_width: usize) -> String {
    use unicode_width::UnicodeWidthStr;
    let width = UnicodeWidthStr::width(name);
    if width <= max_width {
        return name.to_string();
    }
    let mut result = String::new();
    let mut w = 0;
    for ch in name.chars() {
        let cw = unicode_width::UnicodeWidthChar::width(ch).unwrap_or(0);
        if w + cw + 1 > max_width {
            break;
        }
        result.push(ch);
        w += cw;
    }
    result.push('…');
    result
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

// ============================================================================
// Event Handling
// ============================================================================

fn update_preview(app: &mut App) {
    let visible = app.visible_sessions();
    if let Some(idx) = app.session_state.selected() {
        if idx < visible.len() {
            let pane_id = visible[idx].pane_id;
            app.pane_preview = get_pane_text(pane_id, &app.config.wezterm_path);
            app.preview_scroll = app.pane_preview.len().saturating_sub(1) as u16;
        } else {
            app.pane_preview.clear();
        }
    } else {
        app.pane_preview.clear();
    }
}

fn handle_key(app: &mut App, key: event::KeyEvent) {
    if key.code == KeyCode::Char('?') {
        app.show_help = true;
        return;
    }

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('f') => app.show_stale = !app.show_stale,
        KeyCode::Char('p') => {
            app.show_preview = !app.show_preview;
            if app.show_preview {
                update_preview(app);
            }
        }
        KeyCode::Char('r') => {
            app.sessions = load_sessions_data(&app.config);
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
            app.mark_manual_select();
            app.previous_session();
            if app.show_preview {
                update_preview(app);
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.mark_manual_select();
            app.next_session();
            if app.show_preview {
                update_preview(app);
            }
        }
        KeyCode::Enter => {
            app.mark_manual_select();
            let visible = app.visible_sessions();
            if let Some(idx) = app.session_state.selected() {
                if idx < visible.len() {
                    activate_pane(visible[idx], &app.config.wezterm_path);
                }
            }
        }
        KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
            app.mark_manual_select();
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

// ============================================================================
// Main Loop
// ============================================================================

pub fn run_tui(config: AppConfig) -> Result<()> {
    enable_raw_mode()?;
    let mut stdout = io::stdout();
    execute!(stdout, EnterAlternateScreen, EnableMouseCapture)?;
    let backend = CrosstermBackend::new(stdout);
    let mut terminal = Terminal::new(backend)?;

    let mut app = App::new(config);
    app.sessions = load_sessions_data(&app.config);

    let (tx, rx) = mpsc::channel::<AppEvent>();

    // Tick thread
    let tx_tick = tx.clone();
    thread::spawn(move || loop {
        thread::sleep(Duration::from_secs(1));
        let _ = tx_tick.send(AppEvent::Tick);
    });

    // Delayed reload
    let tx_delayed = tx.clone();
    thread::spawn(move || {
        thread::sleep(Duration::from_secs(1));
        let _ = tx_delayed.send(AppEvent::SessionsUpdated);
    });

    // data_dir watcher: sessions.json + usage-cache.json
    let tx_sessions = tx.clone();
    let sessions_path = get_sessions_file_path(&app.config.data_dir);
    let sessions_dir = sessions_path.parent().unwrap().to_path_buf();
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

    // Usage: initial load
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
    loop {
        terminal.draw(|f| ui(f, &mut app))?;

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(AppEvent::Tick) => {
                app.tick = app.tick.wrapping_add(1);
                if app.show_preview && app.tick.is_multiple_of(3) {
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
                app.auto_jump_to_waiting();
            }
            Ok(AppEvent::UsageUpdated(usage)) => {
                app.usage = usage;
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
