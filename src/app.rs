use ratatui::widgets::ListState;

use crate::config::AppConfig;
use crate::types::{SessionItem, Task, UsageLimits};

#[derive(Debug, Clone, Copy, PartialEq)]
pub enum FocusMode {
    Sessions,
    Tasks,
}

pub struct App {
    pub config: AppConfig,
    pub sessions: Vec<SessionItem>,
    pub session_state: ListState,
    pub global_tasks: Vec<Task>,
    pub task_state: ListState,
    pub usage: UsageLimits,
    pub show_stale: bool,
    pub focus_mode: FocusMode,
    pub should_quit: bool,
    pub show_help: bool,
    pub show_preview: bool,
    pub pane_preview: Vec<String>,
    pub preview_scroll: u16,
    pub api_connected: bool,
}

impl App {
    pub fn new(config: AppConfig) -> Self {
        let mut session_state = ListState::default();
        session_state.select(Some(0));
        let mut task_state = ListState::default();
        task_state.select(Some(0));

        Self {
            config,
            sessions: Vec::new(),
            session_state,
            global_tasks: Vec::new(),
            task_state,
            usage: UsageLimits {
                five_hour: -1,
                weekly: -1,
                sonnet: -1,
                ..Default::default()
            },
            show_stale: false,
            focus_mode: FocusMode::Sessions,
            should_quit: false,
            show_help: false,
            show_preview: false,
            pane_preview: Vec::new(),
            preview_scroll: 0,
            api_connected: false,
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

    pub fn next_task(&mut self) {
        if self.global_tasks.is_empty() {
            return;
        }
        let i = match self.task_state.selected() {
            Some(i) => (i + 1) % self.global_tasks.len(),
            None => 0,
        };
        self.task_state.select(Some(i));
    }

    pub fn previous_task(&mut self) {
        if self.global_tasks.is_empty() {
            return;
        }
        let i = match self.task_state.selected() {
            Some(i) => {
                if i == 0 {
                    self.global_tasks.len() - 1
                } else {
                    i - 1
                }
            }
            None => 0,
        };
        self.task_state.select(Some(i));
    }
}
