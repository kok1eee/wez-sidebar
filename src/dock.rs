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

use crate::app::{App, KanbanCard};
use crate::config::AppConfig;
use crate::reaper::reap_orphans;
use crate::session::{
    activate_pane, delete_session, get_sessions_file_path,
    load_sessions_data,
};
use crate::types::{AppEvent, EffectiveView, KanbanColumn};
use crate::ui::{centered_rect, render_kanban_card, render_session_card, render_status_bar};
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
    match app.effective_view_mode() {
        EffectiveView::Kanban => render_dock_kanban(frame, app, area),
        EffectiveView::Flat => render_dock_flat(frame, app, area),
    }
}

/// Dock kanban: 3 equal-width columns (Active / Review / Done) side by side.
fn render_dock_kanban(frame: &mut Frame, app: &mut App, area: Rect) {
    let cards = app.unified_cards();
    let total = cards.len();

    let title = format!(" 🖥 Kanban ({}) ", total);
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let cols_area = Rect::new(area.x, area.y + 1, area.width, area.height.saturating_sub(1));

    let col_layout = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
            Constraint::Ratio(1, 3),
        ])
        .split(cols_area);

    // Global card index that matches `app.selected_card` (kanban iterates
    // columns Active → Review → Done; same ordering as `unified_cards`).
    let mut card_idx = 0usize;

    for (i, col) in KanbanColumn::ALL.iter().enumerate() {
        let col_cards: Vec<KanbanCard<'_>> = cards
            .iter()
            .filter(|c| c.column() == Some(*col))
            .copied()
            .collect();
        render_dock_column(
            frame,
            col_cards,
            *col,
            &mut card_idx,
            app.selected_card,
            app.tick,
            col_layout[i],
        );
    }
}

fn render_dock_column(
    frame: &mut Frame,
    col_cards: Vec<KanbanCard<'_>>,
    col: KanbanColumn,
    card_idx: &mut usize,
    selected: usize,
    tick: u32,
    area: Rect,
) {
    let header = format!(" {} ({}) ", col.label(), col_cards.len());
    let header_color = match col {
        KanbanColumn::Active => Color::Green,
        KanbanColumn::Review => Color::Yellow,
        KanbanColumn::Done => Color::DarkGray,
    };
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            header,
            Style::default()
                .fg(header_color)
                .add_modifier(Modifier::BOLD),
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let card_height = 5u16;
    let cards_area_h = area.height.saturating_sub(1);
    let max_cards = if cards_area_h >= card_height {
        (cards_area_h / card_height) as usize
    } else {
        0
    };

    let cards_in_col = col_cards.len();

    // Show up to max_cards; if selection falls below, scroll vertically.
    let col_start_idx = *card_idx; // global index of col_cards[0]
    let selected_in_col = if selected >= col_start_idx && selected < col_start_idx + cards_in_col {
        Some(selected - col_start_idx)
    } else {
        None
    };
    let scroll = match selected_in_col {
        Some(s) if s >= max_cards => s - max_cards + 1,
        _ => 0,
    };

    for (row, i) in (scroll..).take(max_cards).enumerate() {
        if i >= cards_in_col {
            break;
        }
        let y = area.y + 1 + (row as u16 * card_height);
        if y + card_height > area.y + area.height {
            break;
        }
        let card = col_cards[i];
        let global_idx = col_start_idx + i;
        let is_selected = global_idx == selected;
        let card_area = Rect::new(area.x, y, area.width, card_height);
        render_kanban_card(frame, &card, is_selected, tick, card_area);
    }

    *card_idx += cards_in_col;
}

fn render_dock_flat(frame: &mut Frame, app: &mut App, area: Rect) {
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
    let area = centered_rect(50, 16, frame.area());

    let lines = vec![
        Line::from(" Dock Mode + Kanban"),
        Line::from(""),
        Line::from(" h/l      prev/next (col in kanban)"),
        Line::from(" j/k      prev/next card/session"),
        Line::from(" Enter    switch pane"),
        Line::from(" v        toggle view (auto/kanban/flat)"),
        Line::from(" a        approve task (review → done)"),
        Line::from(" R        reject task (review → running)"),
        Line::from(" T        trash task (or delete session)"),
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

    let in_kanban = app.effective_view_mode() == EffectiveView::Kanban;

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('v') => {
            app.cycle_view_mode();
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.mark_manual_select();
            if in_kanban {
                app.previous_card();
            } else {
                app.previous_session();
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.mark_manual_select();
            if in_kanban {
                app.next_card();
            } else {
                app.next_session();
            }
        }
        KeyCode::Left | KeyCode::Char('h') => {
            app.mark_manual_select();
            if in_kanban {
                jump_prev_column(app);
            } else {
                app.previous_session();
            }
        }
        KeyCode::Right | KeyCode::Char('l') => {
            app.mark_manual_select();
            if in_kanban {
                jump_next_column(app);
            } else {
                app.next_session();
            }
        }
        KeyCode::Enter => {
            app.mark_manual_select();
            if in_kanban {
                let sess_opt = app
                    .selected_kanban_card()
                    .and_then(|c| c.session().cloned());
                if let Some(sess) = sess_opt {
                    activate_pane(&sess, app.backend.as_ref());
                }
            } else {
                let visible = app.visible_sessions();
                if let Some(idx) = app.session_state.selected() {
                    if idx < visible.len() {
                        activate_pane(visible[idx], app.backend.as_ref());
                    }
                }
            }
        }
        KeyCode::Char('a') => {
            if in_kanban {
                if let Some(id) = app
                    .selected_kanban_card()
                    .and_then(|c| c.task_id().map(String::from))
                {
                    if let Err(e) = crate::approve_task(&app.config, &id) {
                        eprintln!("approve failed: {}", e);
                    }
                    app.reload_all();
                }
            }
        }
        KeyCode::Char('R') => {
            if in_kanban {
                if let Some(id) = app
                    .selected_kanban_card()
                    .and_then(|c| c.task_id().map(String::from))
                {
                    if let Err(e) = crate::reject_task(&app.config, &id) {
                        eprintln!("reject failed: {}", e);
                    }
                    app.reload_all();
                }
            }
        }
        KeyCode::Char('T') => {
            if in_kanban {
                let selected = app.selected_kanban_card();
                match selected.and_then(|c| c.task_id().map(String::from)) {
                    Some(id) => {
                        if let Err(e) = crate::trash_task(&app.config, &id) {
                            eprintln!("trash failed: {}", e);
                        }
                        app.reload_all();
                    }
                    None => {
                        if let Some(sess) = selected.and_then(|c| c.session()) {
                            delete_session(sess, &app.config.data_dir);
                            app.sessions =
                                load_sessions_data(&app.config, app.backend.as_ref());
                        }
                    }
                }
            }
        }
        KeyCode::Char('d') => {
            let visible = app.visible_sessions();
            if let Some(idx) = app.session_state.selected() {
                if idx < visible.len() {
                    delete_session(visible[idx], &app.config.data_dir);
                    app.sessions = load_sessions_data(&app.config, app.backend.as_ref());
                }
            }
        }
        KeyCode::Char('f') => app.show_stale = !app.show_stale,
        KeyCode::Char('r') => {
            app.reload_all();
            app.usage = load_usage_from_cache(&app.config.data_dir);
        }
        _ => {}
    }
}

/// Kanban column navigation: jump the selected card to the first card of the
/// previous column (wrapping at the start).
fn jump_prev_column(app: &mut App) {
    let cards = app.unified_cards();
    if cards.is_empty() {
        return;
    }
    let cur_col = cards
        .get(app.selected_card)
        .and_then(|c| c.column())
        .unwrap_or(KanbanColumn::Active);
    // Find the card index that starts the previous non-empty column.
    let prev_order = [KanbanColumn::Done, KanbanColumn::Review, KanbanColumn::Active];
    let cur_i = prev_order.iter().position(|c| *c == cur_col).unwrap_or(0);
    for step in 1..=prev_order.len() {
        let target = prev_order[(cur_i + step) % prev_order.len()];
        if let Some(first) = cards.iter().position(|c| c.column() == Some(target)) {
            app.selected_card = first;
            return;
        }
    }
}

fn jump_next_column(app: &mut App) {
    let cards = app.unified_cards();
    if cards.is_empty() {
        return;
    }
    let cur_col = cards
        .get(app.selected_card)
        .and_then(|c| c.column())
        .unwrap_or(KanbanColumn::Active);
    let next_order = [KanbanColumn::Active, KanbanColumn::Review, KanbanColumn::Done];
    let cur_i = next_order.iter().position(|c| *c == cur_col).unwrap_or(0);
    for step in 1..=next_order.len() {
        let target = next_order[(cur_i + step) % next_order.len()];
        if let Some(first) = cards.iter().position(|c| c.column() == Some(target)) {
            app.selected_card = first;
            return;
        }
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
    app.reload_all();

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
                let is_tasks = event.paths.iter().any(|p| {
                    p.file_name().map(|n| n == "tasks.json").unwrap_or(false)
                });
                let is_usage = event.paths.iter().any(|p| {
                    p.file_name().map(|n| n == "usage-cache.json").unwrap_or(false)
                });

                if is_sessions || is_tasks {
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
                // Reap orphaned claude processes every 5 minutes
                if app.config.reaper.enabled && app.tick.is_multiple_of(300) {
                    reap_orphans(&app.config, false);
                }
                // Block-alert: evaluate review-stale tasks every 30 seconds.
                // 30s cadence keeps log noise low while still catching 5-min
                // thresholds within half a minute.
                if app.tick.is_multiple_of(30) {
                    crate::notify::process_block_alerts(&app.config, app.backend.as_ref());
                }
            }
            Ok(AppEvent::Key(key)) => {
                if app.show_help {
                    app.show_help = false;
                } else {
                    handle_dock_key(&mut app, key);
                }
            }
            Ok(AppEvent::SessionsUpdated) => {
                app.reload_all();
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
