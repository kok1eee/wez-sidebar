use anyhow::Result;
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
    widgets::{Block, Borders, Clear, Paragraph},
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
    activate_pane, delete_session, get_sessions_file_path,
    load_sessions_data,
};
use crate::types::AppEvent;
use crate::ui::{centered_rect, render_session_card, render_status_bar};
use crate::usage::load_usage_from_cache;

// ============================================================================
// Dock Mode Rendering
// ============================================================================

fn dock_ui(frame: &mut Frame, app: &mut App) {
    let main_layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Min(3),    // Sessions (full width)
            Constraint::Length(1), // Status bar (usage + help)
        ])
        .split(frame.area());

    render_dock_sessions(frame, app, main_layout[0]);
    render_status_bar(frame, app, main_layout[1]);

    if app.show_help {
        render_dock_help_popup(frame);
    }
}


fn render_dock_sessions(frame: &mut Frame, app: &mut App, area: Rect) {
    let visible = app.visible_sessions();
    let selected = app.session_state.selected().unwrap_or(0);
    let total = visible.len();

    // Show up to 6 cards, scroll for the rest
    let max_cards = 6usize;

    let scroll_hint = if total > max_cards { " ↕" } else { "" };
    let title = if app.show_stale {
        format!(" 🖥 Sessions [All] ({}){} ", total, scroll_hint)
    } else {
        format!(" 🖥 Sessions ({}){} ", total, scroll_hint)
    };

    // Title row
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let cards_area = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));

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

    let scroll_offset = if selected >= max_cards {
        selected - max_cards + 1
    } else {
        0
    };

    // Each visible card gets equal width (fills the full area)
    let num_cards = max_cards.min(total - scroll_offset);
    let constraints: Vec<Constraint> = (0..num_cards)
        .map(|_| Constraint::Ratio(1, num_cards as u32))
        .collect();

    let cols = Layout::default()
        .direction(Direction::Horizontal)
        .constraints(constraints)
        .split(cards_area);

    for (ci, i) in (scroll_offset..).take(num_cards).enumerate() {
        if i >= visible.len() {
            break;
        }
        let sess = visible[i];
        let is_selected = i == selected;
        render_session_card(frame, sess, is_selected, app.tick, cols[ci]);
    }
}

fn render_dock_help_popup(frame: &mut Frame) {
    let area = centered_rect(40, 10, frame.area());

    let lines = vec![
        Line::from(" Dock Mode"),
        Line::from(""),
        Line::from(" h/l      prev/next session"),
        Line::from(" j/k      prev/next session"),
        Line::from(" Enter    switch pane"),
        Line::from(" d/f/r    delete/show all/refresh"),
        Line::from(" q        quit"),
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
        KeyCode::Up | KeyCode::Char('k') | KeyCode::Left | KeyCode::Char('h') => {
            app.mark_manual_select();
            app.previous_session();
        }
        KeyCode::Down | KeyCode::Char('j') | KeyCode::Right | KeyCode::Char('l') => {
            app.mark_manual_select();
            app.next_session();
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
        KeyCode::Char('d') => {
            let visible = app.visible_sessions();
            if let Some(idx) = app.session_state.selected() {
                if idx < visible.len() {
                    delete_session(visible[idx], &app.config.data_dir);
                    app.sessions = load_sessions_data(&app.config);
                }
            }
        }
        KeyCode::Char('f') => app.show_stale = !app.show_stale,
        KeyCode::Char('r') => {
            app.sessions = load_sessions_data(&app.config);
            app.usage = load_usage_from_cache(&app.config.data_dir);
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
        terminal.draw(|f| dock_ui(f, &mut app))?;

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(AppEvent::Tick) => {
                app.tick = app.tick.wrapping_add(1);
            }
            Ok(AppEvent::Key(key)) => {
                if app.show_help {
                    app.show_help = false;
                } else {
                    handle_dock_key(&mut app, key);
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
