use std::time::Instant;

use ratatui::widgets::ListState;

use crate::config::AppConfig;
use crate::types::{SessionItem, UsageLimits};

pub struct App {
    pub config: AppConfig,
    pub sessions: Vec<SessionItem>,
    pub session_state: ListState,
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

impl App {
    pub fn new(config: AppConfig) -> Self {
        let mut session_state = ListState::default();
        session_state.select(Some(0));

        Self {
            config,
            sessions: Vec::new(),
            session_state,
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

    /// Auto-jump to the first waiting_input session (unless user recently navigated)
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
}
