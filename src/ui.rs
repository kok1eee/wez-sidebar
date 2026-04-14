use anyhow::Result;
use chrono::{DateTime, Utc};
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

use crate::app::{App, KanbanCard};
use crate::config::AppConfig;
use crate::reaper::reap_orphans;
use crate::session::{
    activate_pane, delete_session, get_sessions_file_path,
    load_sessions_data,
};
use crate::types::{
    AppEvent, EffectiveView, KanbanColumn, KanbanTask, SessionItem, TaskStatus,
};
use crate::usage::load_usage_from_cache;

// ============================================================================
// TUI Rendering
// ============================================================================

fn ui(frame: &mut Frame, app: &mut App) {
    if app.show_preview {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(5),    // Sessions
                Constraint::Length(12), // Preview
                Constraint::Length(1),  // Status bar
            ])
            .split(frame.area());

        render_sessions(frame, app, chunks[0]);
        render_preview(frame, app, chunks[1]);
        render_status_bar(frame, app, chunks[2]);
    } else {
        let chunks = Layout::default()
            .direction(Direction::Vertical)
            .constraints([
                Constraint::Min(10),   // Sessions
                Constraint::Length(1), // Status bar
            ])
            .split(frame.area());

        render_sessions(frame, app, chunks[0]);
        render_status_bar(frame, app, chunks[1]);
    }

    if app.show_help {
        render_help_popup(frame);
    }
}

fn render_sessions(frame: &mut Frame, app: &mut App, area: Rect) {
    match app.effective_view_mode() {
        EffectiveView::Kanban => render_sidebar_sections(frame, app, area),
        EffectiveView::Flat => render_sessions_flat(frame, app, area),
    }
}

fn render_sessions_flat(frame: &mut Frame, app: &mut App, area: Rect) {
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

/// Sidebar kanban: render Active / Review / Done as vertical sections.
///
/// Each section has a header row (`▼ Active (N)` or `▶ Active (N)` when
/// collapsed). Headers count as "cards" for keyboard navigation — selecting a
/// header + pressing Space/Enter toggles its collapse state.
fn render_sidebar_sections(frame: &mut Frame, app: &mut App, area: Rect) {
    let cards = app.unified_cards();
    let card_height = 5u16;

    // Title bar row (consistent with flat view)
    let title = format!(" 🖥 Kanban ({}) ", cards.len());
    frame.render_widget(
        Paragraph::new(Line::from(Span::styled(
            title,
            Style::default().add_modifier(Modifier::BOLD),
        ))),
        Rect::new(area.x, area.y, area.width, 1),
    );

    let mut y = area.y + 1;
    let max_y = area.y + area.height;

    // Track global card index so `app.selected_card` aligns with visible
    // card positions. Headers never move the selection cursor.
    let mut card_idx: usize = 0;

    for col in KanbanColumn::ALL {
        let col_cards: Vec<KanbanCard<'_>> = cards
            .iter()
            .filter(|c| c.column() == Some(col))
            .copied()
            .collect();
        let collapsed = app.section_collapsed[col.index()];

        // Header row (1 line)
        if y >= max_y {
            break;
        }
        let glyph = if collapsed { "▶" } else { "▼" };
        let header = format!(" {} {} ({})", glyph, col.label(), col_cards.len());
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                header,
                Style::default()
                    .fg(column_color(col))
                    .add_modifier(Modifier::BOLD),
            ))),
            Rect::new(area.x, y, area.width, 1),
        );
        y += 1;

        if collapsed {
            card_idx += col_cards.len();
            continue;
        }

        for card in col_cards {
            if y + card_height > max_y {
                card_idx += 1;
                continue;
            }
            let is_selected = app.selected_card == card_idx;
            let card_area = Rect::new(area.x, y, area.width, card_height);
            render_kanban_card(frame, &card, is_selected, app.tick, card_area);
            y += card_height;
            card_idx += 1;
        }
    }
}

fn column_color(col: KanbanColumn) -> Color {
    match col {
        KanbanColumn::Active => Color::Green,
        KanbanColumn::Review => Color::Yellow,
        KanbanColumn::Done => Color::DarkGray,
    }
}

/// Render a single kanban card — dispatches to session-based or task-only rendering.
pub fn render_kanban_card(
    frame: &mut Frame,
    card: &KanbanCard<'_>,
    is_selected: bool,
    tick: u32,
    area: Rect,
) {
    match card {
        KanbanCard::Session(s) => render_session_card(frame, s, is_selected, tick, area),
        KanbanCard::SessionWithTask(s, t) => {
            render_session_card_with_title(frame, s, Some(&t.title), is_selected, tick, area)
        }
        KanbanCard::TaskOnly(t) => render_task_card(frame, t, is_selected, area),
    }
}

/// Render a task card that has no bound live session — shows title, status,
/// and cwd. Style is subdued (DarkGray) since there is no live spinner.
pub fn render_task_card(
    frame: &mut Frame,
    task: &KanbanTask,
    is_selected: bool,
    area: Rect,
) {
    let (icon, color) = match task.status {
        TaskStatus::Backlog => ("○", Color::DarkGray),
        TaskStatus::Running => ("●", Color::Green),
        TaskStatus::Review => ("?", Color::Yellow),
        TaskStatus::Done => ("■", Color::Blue),
        TaskStatus::Trash => ("✕", Color::DarkGray),
    };

    let border_style = if is_selected {
        Style::default().fg(color).add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(color)
    };

    let inner_w = (area.width as usize).saturating_sub(2);
    let max_title = inner_w.saturating_sub(6); // "{} " + id(8) margin
    let card_title = format!(" {} {} ", icon, truncate_name(&task.title, max_title));

    let block = Block::default()
        .borders(Borders::ALL)
        .border_type(BorderType::Rounded)
        .border_style(border_style)
        .title(card_title);

    let mut lines = Vec::new();
    lines.push(Line::from(vec![
        Span::styled(
            format!(" id={} ", task.id),
            Style::default().fg(Color::DarkGray),
        ),
        Span::styled(
            task.status.as_str().to_string(),
            Style::default().fg(color),
        ),
    ]));

    // cwd line
    let home = dirs::home_dir()
        .map(|h| h.to_string_lossy().to_string())
        .unwrap_or_default();
    let display_cwd = if !home.is_empty() && task.cwd.starts_with(&home) {
        format!("~{}", &task.cwd[home.len()..])
    } else {
        task.cwd.clone()
    };
    lines.push(Line::from(Span::styled(
        format!(" {}", truncate_name(&display_cwd, inner_w.saturating_sub(1))),
        Style::default().fg(Color::DarkGray),
    )));

    // Prompt preview if present
    if let Some(prompt) = &task.prompt {
        lines.push(Line::from(Span::styled(
            format!(" {}", truncate_name(prompt, inner_w.saturating_sub(1))),
            Style::default().fg(Color::Cyan),
        )));
    }

    let paragraph = Paragraph::new(lines).block(block);
    frame.render_widget(paragraph, area);
}

/// Same as `render_session_card` but overrides the card title (used when a
/// session is bound to a kanban task — we prefer task title over pane name).
pub fn render_session_card_with_title(
    frame: &mut Frame,
    sess: &SessionItem,
    title_override: Option<&str>,
    is_selected: bool,
    tick: u32,
    area: Rect,
) {
    // Temporarily swap name in a clone; render_session_card uses sess.name.
    let mut tmp = sess.clone();
    if let Some(t) = title_override {
        tmp.name = t.to_string();
    }
    render_session_card(frame, &tmp, is_selected, tick, area);
}

pub const SPINNER_FRAMES: &[char] = &['⠋', '⠙', '⠹', '⠸', '⠼', '⠴', '⠦', '⠧', '⠇', '⠏'];

pub fn render_session_card(frame: &mut Frame, sess: &SessionItem, is_selected: bool, tick: u32, area: Rect) {
    let marker = if sess.is_disconnected {
        "⚫"
    } else if sess.permission_mode == "yolo" {
        "🤖"
    } else if sess.permission_mode == "auto" {
        "⚡"
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

    // Line 1: duration + context bar + git branch
    let mut line1_spans = vec![Span::styled(
        format!(" {}", duration),
        Style::default().fg(Color::DarkGray),
    )];
    let ctx_display_len = if let Some(pct) = sess.context_percent {
        let bar_w = 8;
        let filled = ((pct as usize) * bar_w / 100).min(bar_w);
        let empty = bar_w - filled;
        let bar_color = if pct >= 80 {
            Color::Red
        } else if pct >= 50 {
            Color::Yellow
        } else {
            Color::Green
        };
        let label = format!(" {}%", pct);
        line1_spans.push(Span::styled(
            " ".to_string(),
            Style::default(),
        ));
        line1_spans.push(Span::styled(
            "█".repeat(filled),
            Style::default().fg(bar_color),
        ));
        line1_spans.push(Span::styled(
            "░".repeat(empty),
            Style::default().fg(Color::DarkGray),
        ));
        line1_spans.push(Span::styled(
            label.clone(),
            Style::default().fg(Color::DarkGray),
        ));
        1 + bar_w + label.len() // " " + bar + label
    } else {
        0
    };
    if let Some(ref branch) = sess.git_branch {
        let used = duration.len() + ctx_display_len + 2;
        let max_branch = inner_w.saturating_sub(used + 2);
        if max_branch > 3 {
            line1_spans.push(Span::styled(
                format!(" {}", truncate_name(branch, max_branch)),
                Style::default().fg(Color::Magenta),
            ));
        }
    }
    lines.push(Line::from(line1_spans));

    // Line 2: last activity (red if dangerous) or status hint + subagent count
    let subagent_suffix = if sess.active_subagents > 0 {
        format!(" {}agents", sess.active_subagents)
    } else {
        String::new()
    };
    let suffix_w = unicode_width::UnicodeWidthStr::width(subagent_suffix.as_str());

    if let Some(ref activity) = sess.last_activity {
        let color = if sess.is_dangerous { Color::Red } else { Color::Cyan };
        let prefix = if sess.is_dangerous { " ⚠ " } else { " " };
        let prefix_w = if sess.is_dangerous { 4 } else { 1 };
        let max_len = inner_w.saturating_sub(prefix_w + suffix_w);
        let mut spans = vec![Span::styled(
            format!("{}{}", prefix, truncate_name(activity, max_len)),
            Style::default().fg(color),
        )];
        if !subagent_suffix.is_empty() {
            spans.push(Span::styled(subagent_suffix.clone(), Style::default().fg(Color::Blue)));
        }
        lines.push(Line::from(spans));
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
    } else if !subagent_suffix.is_empty() {
        lines.push(Line::from(Span::styled(
            format!(" {}", subagent_suffix.trim()),
            Style::default().fg(Color::Blue),
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

    // Extra lines (dock mode has more space): show active tasks (in_progress first, then pending)
    let active_tasks: Vec<_> = sess.tasks.iter()
        .filter(|t| t.status != "completed" && t.status != "deleted")
        .collect();
    // Sort: in_progress before pending
    let mut sorted_tasks = active_tasks;
    sorted_tasks.sort_by_key(|t| if t.status == "in_progress" { 0 } else { 1 });
    for task in sorted_tasks.iter().take(remaining) {
        let (icon, color) = match task.status.as_str() {
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

/// Check if Anthropic 2x usage campaign is active
/// Campaign: outside US Pacific weekday 5AM-11AM, until 2026-03-27
fn is_double_usage_active() -> bool {
    use chrono::{Datelike, FixedOffset, Local, NaiveDate, Timelike, Weekday};

    let now = Local::now();

    // Campaign ends 2026-03-27
    let end_date = NaiveDate::from_ymd_opt(2026, 3, 27).unwrap();
    if now.date_naive() > end_date {
        return false;
    }

    // Convert to US Pacific (UTC-7 PDT)
    let pacific = FixedOffset::west_opt(7 * 3600).unwrap();
    let pacific_now = now.with_timezone(&pacific);
    let hour = pacific_now.hour();
    let weekday = pacific_now.weekday();

    // Peak = weekday 5AM-11AM Pacific → NOT double
    let is_weekday = !matches!(weekday, Weekday::Sat | Weekday::Sun);
    let is_peak = is_weekday && (5..11).contains(&hour);

    !is_peak
}

/// 30分以上経過した cache は古いとみなしグレー表示する
const STALE_USAGE_AGE_SECS: u64 = 30 * 60;

/// 経過秒数を "今" / "5分前" / "2時間前" / "3日前" のような短い相対時間表現にする
fn format_age(secs: u64) -> String {
    if secs < 60 {
        "今".to_string()
    } else if secs < 3600 {
        format!("{}分前", secs / 60)
    } else if secs < 86400 {
        format!("{}時間前", secs / 3600)
    } else {
        format!("{}日前", secs / 86400)
    }
}

/// Format usage as compact spans for status bar
pub fn format_usage_spans(app: &App) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let stale = app
        .usage
        .cache_age_secs
        .map(|s| s >= STALE_USAGE_AGE_SECS)
        .unwrap_or(false);

    let pick_color = |percent: i32| -> Color {
        if stale {
            Color::DarkGray
        } else if percent >= 80 {
            Color::Red
        } else if percent >= 50 {
            Color::Yellow
        } else {
            Color::Green
        }
    };

    if app.usage.five_hour >= 0 {
        let color = pick_color(app.usage.five_hour);
        let mut text = format!("⏳{}%", app.usage.five_hour);
        if !app.usage.five_hour_reset.is_empty() {
            text.push_str(&format!("({})", app.usage.five_hour_reset));
        }
        spans.push(Span::styled(text, Style::default().fg(color)));
        spans.push(Span::raw(" "));
    }

    if app.usage.weekly >= 0 {
        let color = pick_color(app.usage.weekly);
        let mut text = format!("📅{}%", app.usage.weekly);
        if !app.usage.weekly_reset.is_empty() {
            text.push_str(&format!("(~{})", app.usage.weekly_reset));
        }
        spans.push(Span::styled(text, Style::default().fg(color)));
        spans.push(Span::raw(" "));
    }

    if let Some(secs) = app.usage.cache_age_secs {
        spans.push(Span::styled(
            format!("[{}]", format_age(secs)),
            Style::default().fg(Color::DarkGray),
        ));
        spans.push(Span::raw(" "));
    }

    if is_double_usage_active() {
        spans.push(Span::styled("2倍中🔥", Style::default().fg(Color::Rgb(255, 140, 0))));
        spans.push(Span::raw(" "));
    }

    spans
}

fn render_help_popup(frame: &mut Frame) {
    let area = centered_rect(44, 18, frame.area());

    let lines = vec![
        Line::from(" 🖥 Sessions + Kanban"),
        Line::from(""),
        Line::from(" j/k     up/down"),
        Line::from(" Enter   switch pane"),
        Line::from(" 1-9     switch by number"),
        Line::from(" v       toggle view (auto/kanban/flat)"),
        Line::from(" a       approve task (review → done)"),
        Line::from(" R       reject task (review → running)"),
        Line::from(" T       trash task (or delete session)"),
        Line::from(" Space   toggle section collapse"),
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
            app.pane_preview = app.backend.get_pane_text(pane_id);
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

    let in_kanban = app.effective_view_mode() == EffectiveView::Kanban;

    match key.code {
        KeyCode::Char('q') | KeyCode::Esc => app.should_quit = true,
        KeyCode::Char('f') => app.show_stale = !app.show_stale,
        KeyCode::Char('v') => {
            app.cycle_view_mode();
        }
        KeyCode::Char('p') => {
            app.show_preview = !app.show_preview;
            if app.show_preview {
                update_preview(app);
            }
        }
        KeyCode::Char('r') => {
            app.reload_all();
            app.usage = load_usage_from_cache(&app.config.data_dir);
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
                        // Session-only card: fall back to delete_session
                        if let Some(sess) = selected.and_then(|c| c.session()) {
                            delete_session(sess, &app.config.data_dir);
                            app.sessions =
                                load_sessions_data(&app.config, app.backend.as_ref());
                        }
                    }
                }
            }
        }
        KeyCode::Up | KeyCode::Char('k') => {
            app.mark_manual_select();
            if in_kanban {
                app.previous_card();
            } else {
                app.previous_session();
                if app.show_preview {
                    update_preview(app);
                }
            }
        }
        KeyCode::Down | KeyCode::Char('j') => {
            app.mark_manual_select();
            if in_kanban {
                app.next_card();
            } else {
                app.next_session();
                if app.show_preview {
                    update_preview(app);
                }
            }
        }
        KeyCode::Enter => {
            app.mark_manual_select();
            if in_kanban {
                if let Some(sess) = app.selected_kanban_card().and_then(|c| c.session()) {
                    let sess_cloned = sess.clone();
                    activate_pane(&sess_cloned, app.backend.as_ref());
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
        KeyCode::Char(' ') => {
            // Toggle section collapse: toggle the column containing the selected card
            if in_kanban {
                if let Some(col) = app.selected_kanban_card().and_then(|c| c.column()) {
                    app.toggle_section(col);
                }
            }
        }
        KeyCode::Char(c) if c.is_ascii_digit() && c != '0' => {
            app.mark_manual_select();
            let idx = (c as usize) - ('1' as usize);
            let visible: Vec<SessionItem> = app.visible_sessions().into_iter().cloned().collect();
            if idx < visible.len() {
                app.session_state.select(Some(idx));
                activate_pane(&visible[idx], app.backend.as_ref());
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
        terminal.draw(|f| ui(f, &mut app))?;

        match rx.recv_timeout(Duration::from_millis(100)) {
            Ok(AppEvent::Tick) => {
                app.tick = app.tick.wrapping_add(1);
                if app.show_preview && app.tick.is_multiple_of(3) {
                    update_preview(&mut app);
                }
                // Reap orphaned claude processes every 5 minutes
                if app.config.reaper.enabled && app.tick.is_multiple_of(300) {
                    reap_orphans(&app.config, false);
                }
                // Block-alert: evaluate review-stale tasks every 30 seconds.
                if app.tick.is_multiple_of(30) {
                    crate::notify::process_block_alerts(&app.config, app.backend.as_ref());
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
