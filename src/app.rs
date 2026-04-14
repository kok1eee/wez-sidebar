use std::time::Instant;

use ratatui::widgets::ListState;

use crate::config::AppConfig;
use crate::session::load_sessions_data;
use crate::tasks::load_tasks;
use crate::terminal::{create_backend, TerminalBackend};
use crate::types::{
    EffectiveView, KanbanColumn, KanbanTask, SessionItem, TaskStatus, TasksFile, UsageLimits,
    ViewMode,
};

pub struct App {
    pub config: AppConfig,
    pub backend: Box<dyn TerminalBackend>,
    pub sessions: Vec<SessionItem>,
    pub session_state: ListState,
    pub tasks: TasksFile,
    pub view_mode: ViewMode,
    /// Per-column collapse flags for sidebar sections (index via KanbanColumn::index).
    pub section_collapsed: [bool; 3],
    /// Linear selection into the unified card list (kanban mode).
    pub selected_card: usize,
    pub usage: UsageLimits,
    pub show_stale: bool,
    pub should_quit: bool,
    pub show_help: bool,
    pub show_preview: bool,
    pub pane_preview: Vec<String>,
    pub preview_scroll: u16,
    pub tick: u32,
    pub last_manual_select: Option<Instant>,
}

/// A renderable card in kanban mode — either a live session, a session bound
/// to a kanban task, or a task with no live session (backlog or bookkeeping).
#[derive(Debug, Clone, Copy)]
pub enum KanbanCard<'a> {
    Session(&'a SessionItem),
    SessionWithTask(&'a SessionItem, &'a KanbanTask),
    TaskOnly(&'a KanbanTask),
}

impl<'a> KanbanCard<'a> {
    pub fn column(self) -> Option<KanbanColumn> {
        match self {
            KanbanCard::Session(s) => {
                if s.is_disconnected || s.is_stale {
                    return None;
                }
                Some(KanbanColumn::Active)
            }
            KanbanCard::SessionWithTask(_, t) | KanbanCard::TaskOnly(t) => match t.status {
                TaskStatus::Backlog | TaskStatus::Running => Some(KanbanColumn::Active),
                TaskStatus::Review => Some(KanbanColumn::Review),
                TaskStatus::Done => Some(KanbanColumn::Done),
                TaskStatus::Trash => None,
            },
        }
    }

    pub fn task_id(self) -> Option<&'a str> {
        match self {
            KanbanCard::Session(_) => None,
            KanbanCard::SessionWithTask(_, t) | KanbanCard::TaskOnly(t) => Some(&t.id),
        }
    }

    pub fn session(self) -> Option<&'a SessionItem> {
        match self {
            KanbanCard::Session(s) | KanbanCard::SessionWithTask(s, _) => Some(s),
            KanbanCard::TaskOnly(_) => None,
        }
    }

    #[allow(dead_code)]
    pub fn task(self) -> Option<&'a KanbanTask> {
        match self {
            KanbanCard::SessionWithTask(_, t) | KanbanCard::TaskOnly(t) => Some(t),
            KanbanCard::Session(_) => None,
        }
    }
}

impl App {
    pub fn new(config: AppConfig) -> Self {
        let mut session_state = ListState::default();
        session_state.select(Some(0));

        let backend = create_backend(&config.backend, config.effective_terminal_path());

        Self {
            config,
            backend,
            sessions: Vec::new(),
            session_state,
            tasks: TasksFile::default(),
            view_mode: ViewMode::Auto,
            section_collapsed: [false; 3],
            selected_card: 0,
            usage: UsageLimits {
                five_hour: -1,
                weekly: -1,
                sonnet: -1,
                ..Default::default()
            },
            show_stale: false,
            should_quit: false,
            show_help: false,
            show_preview: false,
            pane_preview: Vec::new(),
            preview_scroll: 0,
            tick: 0,
            last_manual_select: None,
        }
    }

    pub fn mark_manual_select(&mut self) {
        self.last_manual_select = Some(Instant::now());
    }

    /// Reload sessions + tasks from disk.
    pub fn reload_all(&mut self) {
        self.sessions = load_sessions_data(&self.config, self.backend.as_ref());
        self.tasks = load_tasks(&self.config.data_dir);
        // Clamp selections.
        let card_count = self.unified_cards().len();
        if self.selected_card >= card_count && card_count > 0 {
            self.selected_card = card_count - 1;
        }
    }

    /// Auto-jump to the first waiting_input session (unless user recently navigated).
    pub fn auto_jump_to_waiting(&mut self) {
        if let Some(t) = self.last_manual_select {
            if t.elapsed().as_secs() < 5 {
                return;
            }
        }
        let visible = self.visible_sessions();
        if let Some(idx) = visible.iter().position(|s| s.status == "waiting_input") {
            self.session_state.select(Some(idx));
        }
    }

    pub fn visible_sessions(&self) -> Vec<&SessionItem> {
        if self.show_stale {
            self.sessions.iter().collect()
        } else {
            self.sessions
                .iter()
                .filter(|s| s.is_disconnected || !s.is_stale)
                .collect()
        }
    }

    pub fn next_session(&mut self) {
        let visible = self.visible_sessions();
        if visible.is_empty() {
            return;
        }
        let i = match self.session_state.selected() {
            Some(i) => (i + 1) % visible.len(),
            None => 0,
        };
        self.session_state.select(Some(i));
    }

    pub fn previous_session(&mut self) {
        let visible = self.visible_sessions();
        if visible.is_empty() {
            return;
        }
        let i = match self.session_state.selected() {
            Some(i) => {
                if i == 0 {
                    visible.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.session_state.select(Some(i));
    }

    // ------------------------------------------------------------------------
    // Kanban view helpers
    // ------------------------------------------------------------------------

    /// Find an active (non-trash) task bound to `session_id`, if any.
    /// Prefers Running > Review > Backlog > Done ordering so the hottest task
    /// "wins" when more than one points at the same session_id.
    fn task_for_session(&self, session_id: &str) -> Option<&KanbanTask> {
        let priority = |s: TaskStatus| -> u8 {
            match s {
                TaskStatus::Running => 0,
                TaskStatus::Review => 1,
                TaskStatus::Backlog => 2,
                TaskStatus::Done => 3,
                TaskStatus::Trash => u8::MAX,
            }
        };
        self.tasks
            .tasks
            .iter()
            .filter(|t| {
                t.status != TaskStatus::Trash
                    && t.session_id.as_deref() == Some(session_id)
            })
            .min_by_key(|t| priority(t.status))
    }

    /// Return the unified list of cards, ordered by column (Active, Review, Done).
    ///
    /// Ordering inside a column:
    /// 1. visible sessions (bound or unbound) in their existing order
    /// 2. task-only entries (no live session) sorted by created_at asc
    pub fn unified_cards(&self) -> Vec<KanbanCard<'_>> {
        let mut cards: Vec<KanbanCard<'_>> = Vec::new();
        let visible = self.visible_sessions();
        let mut bound_task_ids: std::collections::HashSet<&str> =
            std::collections::HashSet::new();

        for col in KanbanColumn::ALL {
            // Session cards for this column
            for sess in &visible {
                let card = match self.task_for_session(&sess.session_id) {
                    Some(task) => {
                        let kc = KanbanCard::SessionWithTask(sess, task);
                        if kc.column() != Some(col) {
                            continue;
                        }
                        bound_task_ids.insert(task.id.as_str());
                        kc
                    }
                    None => {
                        let kc = KanbanCard::Session(sess);
                        if kc.column() != Some(col) {
                            continue;
                        }
                        kc
                    }
                };
                cards.push(card);
            }
            // Task-only cards for this column
            let mut task_only: Vec<&KanbanTask> = self
                .tasks
                .tasks
                .iter()
                .filter(|t| {
                    !bound_task_ids.contains(t.id.as_str())
                        && t.session_id
                            .as_deref()
                            .and_then(|sid| {
                                visible.iter().find(|s| s.session_id == sid).map(|_| ())
                            })
                            .is_none()
                        && KanbanCard::TaskOnly(t).column() == Some(col)
                })
                .collect();
            task_only.sort_by(|a, b| a.created_at.cmp(&b.created_at));
            cards.extend(task_only.into_iter().map(KanbanCard::TaskOnly));
        }

        cards
    }

    #[allow(dead_code)]
    pub fn cards_in_column(&self, col: KanbanColumn) -> Vec<KanbanCard<'_>> {
        self.unified_cards()
            .into_iter()
            .filter(|c| c.column() == Some(col))
            .collect()
    }

    /// Resolve `ViewMode::Auto` using the kanban threshold.
    pub fn effective_view_mode(&self) -> EffectiveView {
        match self.view_mode {
            ViewMode::Kanban => EffectiveView::Kanban,
            ViewMode::Flat => EffectiveView::Flat,
            ViewMode::Auto => {
                let count = self.unified_cards().len();
                let threshold = self.config.kanban.auto_flat_threshold;
                if threshold == 0 {
                    // 0 = always kanban when any cards exist
                    if count > 0 {
                        EffectiveView::Kanban
                    } else {
                        EffectiveView::Flat
                    }
                } else if count >= threshold {
                    EffectiveView::Kanban
                } else {
                    EffectiveView::Flat
                }
            }
        }
    }

    pub fn cycle_view_mode(&mut self) {
        self.view_mode = self.view_mode.next();
    }

    pub fn toggle_section(&mut self, col: KanbanColumn) {
        let idx = col.index();
        self.section_collapsed[idx] = !self.section_collapsed[idx];
    }

    pub fn next_card(&mut self) {
        let n = self.unified_cards().len();
        if n == 0 {
            return;
        }
        self.selected_card = (self.selected_card + 1) % n;
    }

    pub fn previous_card(&mut self) {
        let n = self.unified_cards().len();
        if n == 0 {
            return;
        }
        if self.selected_card == 0 {
            self.selected_card = n - 1;
        } else {
            self.selected_card -= 1;
        }
    }

    /// Card currently selected in kanban mode (if any).
    pub fn selected_kanban_card(&self) -> Option<KanbanCard<'_>> {
        let cards = self.unified_cards();
        cards.get(self.selected_card).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{KanbanTask, Session, SessionsFile, TaskStatus, TasksFile};

    fn mk_session_item(name: &str, session_id: &str) -> SessionItem {
        SessionItem {
            tab_id: 1,
            pane_id: 1,
            name: name.to_string(),
            status: "running".to_string(),
            is_current: false,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
            is_stale: false,
            session_id: session_id.to_string(),
            is_disconnected: false,
            permission_mode: "normal".to_string(),
            last_activity: None,
            is_dangerous: false,
            git_branch: None,
            home_cwd: "/tmp".to_string(),
            last_user_message: None,
            last_user_message_at: None,
            tasks: Vec::new(),
            active_subagents: 0,
            context_percent: None,
        }
    }

    fn mk_task(id: &str, status: TaskStatus, session_id: Option<&str>) -> KanbanTask {
        KanbanTask {
            id: id.to_string(),
            title: format!("task-{}", id),
            prompt: None,
            status,
            cwd: "/tmp".to_string(),
            session_id: session_id.map(String::from),
            created_at: "2026-01-01T00:00:00Z".to_string(),
            started_at: None,
            review_started_at: None,
            completed_at: None,
            block_alerted_at: None,
        }
    }

    fn app_with(sessions: Vec<SessionItem>, tasks: TasksFile) -> App {
        // Avoid creating a real terminal backend; use defaults.
        let mut app = App::new(AppConfig::default());
        app.sessions = sessions;
        app.tasks = tasks;
        app
    }

    // Suppress unused-import warnings when test helpers aren't exhaustive.
    #[allow(dead_code)]
    fn _ignore() -> (Session, SessionsFile) {
        (
            Session {
                session_id: String::new(),
                home_cwd: String::new(),
                tty: String::new(),
                status: String::new(),
                created_at: String::new(),
                updated_at: String::new(),
                is_yolo: false,
                permission_mode: String::new(),
                last_activity: None,
                is_dangerous: false,
                git_branch: None,
                last_user_message: None,
                last_user_message_at: None,
                tasks: Vec::new(),
                subagents: Vec::new(),
                pane_id: None,
                context_percent: None,
            },
            SessionsFile::default(),
        )
    }

    #[test]
    fn test_view_mode_cycle() {
        assert_eq!(ViewMode::Auto.next(), ViewMode::Kanban);
        assert_eq!(ViewMode::Kanban.next(), ViewMode::Flat);
        assert_eq!(ViewMode::Flat.next(), ViewMode::Auto);
    }

    #[test]
    fn test_unified_cards_session_only() {
        let s1 = mk_session_item("a", "sess-1");
        let s2 = mk_session_item("b", "sess-2");
        let app = app_with(vec![s1, s2], TasksFile::default());
        let cards = app.unified_cards();
        assert_eq!(cards.len(), 2);
        for c in &cards {
            assert_eq!(c.column(), Some(KanbanColumn::Active));
            assert!(c.task().is_none());
            assert!(c.session().is_some());
        }
    }

    #[test]
    fn test_unified_cards_binds_session_and_task() {
        let s1 = mk_session_item("a", "sess-1");
        let mut tf = TasksFile::default();
        tf.tasks.push(mk_task("t1", TaskStatus::Review, Some("sess-1")));
        let app = app_with(vec![s1], tf);
        let cards = app.unified_cards();
        assert_eq!(cards.len(), 1);
        let card = cards[0];
        assert_eq!(card.column(), Some(KanbanColumn::Review));
        assert!(matches!(card, KanbanCard::SessionWithTask(_, _)));
    }

    #[test]
    fn test_unified_cards_task_only() {
        let mut tf = TasksFile::default();
        tf.tasks.push(mk_task("t1", TaskStatus::Done, None));
        tf.tasks.push(mk_task("t2", TaskStatus::Review, None));
        tf.tasks.push(mk_task("t3", TaskStatus::Trash, None));
        let app = app_with(Vec::new(), tf);
        let cards = app.unified_cards();
        // Trash excluded
        assert_eq!(cards.len(), 2);
        let cols: Vec<_> = cards.iter().map(|c| c.column()).collect();
        assert!(cols.contains(&Some(KanbanColumn::Done)));
        assert!(cols.contains(&Some(KanbanColumn::Review)));
    }

    #[test]
    fn test_unified_cards_column_order() {
        let mut tf = TasksFile::default();
        tf.tasks.push(mk_task("d", TaskStatus::Done, None));
        tf.tasks.push(mk_task("r", TaskStatus::Review, None));
        tf.tasks.push(mk_task("a", TaskStatus::Running, None));
        let app = app_with(Vec::new(), tf);
        let cards = app.unified_cards();
        assert_eq!(cards.len(), 3);
        assert_eq!(cards[0].column(), Some(KanbanColumn::Active));
        assert_eq!(cards[1].column(), Some(KanbanColumn::Review));
        assert_eq!(cards[2].column(), Some(KanbanColumn::Done));
    }

    #[test]
    fn test_effective_view_threshold() {
        let mut tf = TasksFile::default();
        tf.tasks.push(mk_task("a", TaskStatus::Running, None));
        tf.tasks.push(mk_task("b", TaskStatus::Running, None));
        let mut app = app_with(Vec::new(), tf);
        // Default threshold = 3; only 2 cards → flat
        assert_eq!(app.effective_view_mode(), EffectiveView::Flat);

        app.tasks.tasks.push(mk_task("c", TaskStatus::Running, None));
        assert_eq!(app.effective_view_mode(), EffectiveView::Kanban);
    }

    #[test]
    fn test_effective_view_manual_override() {
        let app_no = app_with(Vec::new(), TasksFile::default());
        let mut app = app_no;
        app.view_mode = ViewMode::Kanban;
        assert_eq!(app.effective_view_mode(), EffectiveView::Kanban);
        app.view_mode = ViewMode::Flat;
        assert_eq!(app.effective_view_mode(), EffectiveView::Flat);
    }

    #[test]
    fn test_cards_in_column() {
        let mut tf = TasksFile::default();
        tf.tasks.push(mk_task("r1", TaskStatus::Review, None));
        tf.tasks.push(mk_task("r2", TaskStatus::Review, None));
        tf.tasks.push(mk_task("d1", TaskStatus::Done, None));
        let app = app_with(Vec::new(), tf);
        assert_eq!(app.cards_in_column(KanbanColumn::Review).len(), 2);
        assert_eq!(app.cards_in_column(KanbanColumn::Done).len(), 1);
        assert_eq!(app.cards_in_column(KanbanColumn::Active).len(), 0);
    }

    #[test]
    fn test_toggle_section() {
        let mut app = app_with(Vec::new(), TasksFile::default());
        assert!(!app.section_collapsed[KanbanColumn::Review.index()]);
        app.toggle_section(KanbanColumn::Review);
        assert!(app.section_collapsed[KanbanColumn::Review.index()]);
        app.toggle_section(KanbanColumn::Review);
        assert!(!app.section_collapsed[KanbanColumn::Review.index()]);
    }

    #[test]
    fn test_next_previous_card_wraps() {
        let mut tf = TasksFile::default();
        tf.tasks.push(mk_task("a", TaskStatus::Running, None));
        tf.tasks.push(mk_task("b", TaskStatus::Review, None));
        tf.tasks.push(mk_task("c", TaskStatus::Done, None));
        let mut app = app_with(Vec::new(), tf);
        assert_eq!(app.selected_card, 0);
        app.next_card();
        assert_eq!(app.selected_card, 1);
        app.next_card();
        assert_eq!(app.selected_card, 2);
        app.next_card();
        assert_eq!(app.selected_card, 0); // wrap
        app.previous_card();
        assert_eq!(app.selected_card, 2); // wrap
    }
}
