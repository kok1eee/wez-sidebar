# Design: wez-sidebar Kanban Phase 6-8

## 1. Architecture Overview

Phase 6-8 adds a kanban layer **on top of** the existing session rendering, without touching hooks, tasks.rs, notify.rs, or session.rs. The kanban view unifies `SessionItem` (live WezTerm panes) and `KanbanTask` (tasks.json entries) into a single `KanbanCard` concept that both renderers consume.

```
App { sessions, tasks, view_mode, collapsed_sections, selected_card }
           │
           ▼
  unified_cards()  →  Vec<KanbanCard>
           │
   ┌───────┴───────┐
   ▼               ▼
 dock.rs        ui.rs  (sidebar)
 (kanban /      (section /
  flat)          flat)
```

## 2. New types (src/app.rs or new src/kanban.rs)

### 2.1 `ViewMode` enum

```rust
pub enum ViewMode {
    Auto,      // Decide per-tick based on auto_flat_threshold
    Kanban,    // Force kanban
    Flat,      // Force flat (existing UI)
}
```

Default: `Auto`. `v` key cycles `Auto → Kanban → Flat → Auto`.

### 2.2 `KanbanColumn` enum

```rust
pub enum KanbanColumn { Active, Review, Done }
```

Mapping rules (`derive_column`):

| Input | Column |
|---|---|
| Session with no task binding, status != "stopped" / disconnected | **Active** |
| Session bound to task, task.status = Backlog or Running | **Active** |
| Session bound to task, task.status = Review | **Review** |
| Task (no session) with status = Backlog or Running | **Active** |
| Task with status = Review, no session | **Review** |
| Task with status = Done | **Done** |

Excluded from kanban: trash tasks, `backlog` tasks with no session when threshold already exceeded by others (we show backlog as placeholder cards in Active).

### 2.3 `KanbanCard` enum (internal)

```rust
pub enum KanbanCard<'a> {
    Session(&'a SessionItem),
    SessionWithTask(&'a SessionItem, &'a KanbanTask),
    TaskOnly(&'a KanbanTask),
}
```

Each card knows:
- `column()` → KanbanColumn
- `display_title()` → prefer task title when bound
- `task_id()` → Some(id) if bound
- `session_pane_id()` → for activate on Enter
- `status_str()` → task-derived when bound, session-derived otherwise

## 3. App state additions (src/app.rs)

```rust
pub struct App {
    ...existing...
    pub tasks: TasksFile,               // reloaded alongside sessions
    pub view_mode: ViewMode,            // user-chosen override
    pub section_collapsed: [bool; 3],   // [Active, Review, Done] for sidebar
    pub selected_card: usize,           // linear index into unified card list
}
```

Existing `session_state: ListState` stays for backward compat in flat mode. In kanban mode, `selected_card` drives rendering and key handling.

## 4. Public API additions

### 4.1 `app.rs`

```rust
pub fn unified_cards(&self) -> Vec<KanbanCard<'_>>;
pub fn cards_in_column(&self, col: KanbanColumn) -> Vec<KanbanCard<'_>>;
pub fn effective_view_mode(&self) -> EffectiveView;  // Kanban | Flat
pub fn cycle_view_mode(&mut self);                    // v key
pub fn toggle_section(&mut self, col: KanbanColumn); // Space/Enter on header
pub fn next_card(&mut self); pub fn previous_card(&mut self);
```

`effective_view_mode` resolves `Auto` by comparing total active-relevant item count against `config.kanban.auto_flat_threshold`.

### 4.2 Task mutation helpers (in tasks.rs or main.rs)

Expose already-existing logic as callable helpers that both CLI and TUI use:

```rust
pub fn approve_task(config: &AppConfig, id: &str) -> Result<Vec<String>>; // returns spawned ids
pub fn reject_task(config: &AppConfig, id: &str) -> Result<()>;
pub fn trash_task(config: &AppConfig, id: &str) -> Result<()>;
```

These factor out the core of `TasksAction::Approve/Reject/Trash`. TUI calls these on `a/R/T`; CLI handlers refactor to use them.

## 5. Rendering (src/dock.rs + src/ui.rs)

### 5.1 dock.rs

```rust
fn render_dock_sessions(frame, app, area) {
    match app.effective_view_mode() {
        EffectiveView::Flat => render_flat(frame, app, area),   // existing path
        EffectiveView::Kanban => render_kanban_columns(frame, app, area),
    }
}

fn render_kanban_columns(frame, app, area) {
    // 3 equal horizontal columns: Active | Review | Done
    // Each column: title row + card stack (vertical)
    // Per-card height fixed at 5 rows (matches sidebar)
}
```

Key handling: `h/l` (and arrows) now move between columns when in kanban; `j/k` within column. Selection wraps to first card of next column when at end.

### 5.2 ui.rs (sidebar)

```rust
fn render_sessions(frame, app, area) {
    match app.effective_view_mode() {
        EffectiveView::Flat => render_sessions_flat(frame, app, area),
        EffectiveView::Kanban => render_sidebar_sections(frame, app, area),
    }
}

fn render_sidebar_sections(frame, app, area) {
    // Vertical: Active header + cards / Review header + cards / Done header + cards
    // Header: "▼ Active (3)" or "▶ Active (3)" when collapsed
    // Space/Enter on header toggles collapse
}
```

### 5.3 Card rendering (shared)

Reuse existing `render_session_card` with a small adapter: for `TaskOnly` cards synthesize a stub `SessionItem` (empty pane_id, status derived from task). Simpler: write a new `render_task_card(frame, task, is_selected, tick, area)` that mirrors layout. For session-bound cards (SessionWithTask, Session) we still call `render_session_card` but the card title uses task title when bound.

Given that adding a TaskOnly adapter is small and the renderer is already 200 lines, go with a **small helper fn `render_kanban_card(frame, card, is_selected, tick, area)`** that dispatches:
- Session / SessionWithTask → extended `render_session_card` (allow override title)
- TaskOnly → compact task-only card (title, status, cwd, no spinner since no live pane)

## 6. Key bindings

### 6.1 New keys

| Key | Action | Context |
|---|---|---|
| `v` | Cycle ViewMode (Auto→Kanban→Flat→Auto) | dock + sidebar |
| `a` | Approve selected task (review→done + spawn downstream) | kanban or flat |
| `R` | Reject selected task (review→running) | kanban or flat |
| `T` | Trash selected task (any→trash) | kanban or flat |
| `Space`/`Enter` on section header | Toggle section collapse | sidebar kanban |

### 6.2 Existing keys

- `k` stays as navigation (NOT view toggle, per requirements)
- `j`/`k`/`h`/`l`/arrows: navigation (column- and card-aware in kanban)
- `Enter` on card: activate pane (unchanged)
- `d`: delete session (unchanged — differs from `T` trash)
- `?`: help (updated with new keys)

### 6.3 Help popup

Updated lines in both dock and sidebar help:

```
 v        toggle view (auto/kanban/flat)
 a        approve task (review → done)
 R        reject task (review → running)
 T        trash task
```

Sidebar gets additional: `Space/Enter on header   toggle section`.

## 7. Session → Task binding

Already done upstream (Phase 4/5): `KanbanTask.session_id` points to the Claude Code session_id. Matching is done in `unified_cards()` by:

```rust
tasks.iter().filter(|t| t.session_id.as_deref() == Some(&session.session_id))
```

Each session is bound to at most one active (non-trash, non-done) task. When multiple match (shouldn't happen but defensive), pick the most recent by `started_at`.

## 8. Data loading

`App` needs tasks loaded alongside sessions:

```rust
// app.rs
pub fn reload_all(&mut self) {
    self.sessions = load_sessions_data(&self.config, self.backend.as_ref());
    self.tasks = load_tasks(&self.config.data_dir);
}
```

Called on:
- App::new() (initial load)
- `SessionsUpdated` event (file watcher)
- `r` key (manual refresh)
- After `a`/`R`/`T` key mutations (to pick up new state)

File watcher already watches `data_dir`; we extend the filter to include `tasks.json`:

```rust
let is_tasks = event.paths.iter().any(|p| p.file_name().map(|n| n == "tasks.json").unwrap_or(false));
if is_tasks { tx.send(AppEvent::SessionsUpdated) }  // reuse existing event
```

## 9. Module boundaries

- **No new module required.** Add types in `src/types.rs`, state in `src/app.rs`, rendering in `src/ui.rs` + `src/dock.rs`. If dock/ui duplication grows, factor `render_kanban_columns` into a helper used by both.
- `approve_task/reject_task/trash_task` live in `src/main.rs` next to the existing CLI handlers. Move enough into helper fns that both CLI and TUI call the same code path.

## 10. Testing strategy

- `unified_cards` tests: mock sessions + tasks, assert column assignment
- `effective_view_mode` tests: threshold boundaries
- `cycle_view_mode` state transitions
- Integration: CLI approve_task helper produces expected state (already tested indirectly via `tasks::set_task_status`; add a wrapper test if needed)

Mostly pure logic — no TUI snapshot tests needed (existing tests don't have them either).

## 11. File modifications summary

| File | Change type | Notes |
|---|---|---|
| `src/types.rs` | add | `ViewMode`, `KanbanColumn` enums |
| `src/app.rs` | modify | add `tasks`, `view_mode`, `section_collapsed`, `selected_card`, card helpers |
| `src/ui.rs` | modify | section rendering, key handling extensions, help popup |
| `src/dock.rs` | modify | column rendering, key handling extensions, help popup |
| `src/main.rs` | minor | extract approve/reject/trash helpers (reused by TUI) |
| `src/tasks.rs` | no change | already complete |
| `src/notify.rs` | no change | already complete |
| `src/config.rs` | no change | `[kanban]` already exists |
| `README_JA.md` | add section | Kanban section |
| `README.md` | add section | Kanban section |
| `config.example.toml` | verify | already has `[kanban]` |

## 12. Risks & Mitigations

- **Risk**: `render_session_card` currently takes `&SessionItem`. Task-only cards have no session.
  - **Mitigation**: Add a sibling `render_task_card` using the same block style.
- **Risk**: Column navigation becomes complex when some columns are empty.
  - **Mitigation**: Skip empty columns during h/l traversal; selection index clamps to valid cards only.
- **Risk**: tasks.json watcher triggers unnecessary reloads.
  - **Mitigation**: tasks.json changes from CLI are infrequent; keep the existing 150ms debounce.
- **Risk**: `T` (trash) for session-only cards could conflict with session `delete_session`.
  - **Mitigation**: For session-only cards, `T` falls back to delete_session (same behavior as `d`). Keep `d` as an explicit alias.
