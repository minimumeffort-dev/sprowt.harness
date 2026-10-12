use super::{Action, App, View};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
impl App {
    pub(super) fn tasks_key(&mut self, key: KeyEvent, selected: usize) -> rusqlite::Result<()> {
        let ids = self.task_ids();
        let selected = selected.min(ids.len().saturating_sub(1));
        match key.code {
            KeyCode::Esc => self.engine.view = View::Chat,
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.engine.view = View::Chat
            }
            KeyCode::Up => self.engine.view = View::Tasks(selected.saturating_sub(1)),
            KeyCode::Down => {
                self.engine.view = View::Tasks((selected + 1).min(ids.len().saturating_sub(1)))
            }
            KeyCode::PageUp => {
                self.engine.view = View::Tasks(selected.saturating_sub(self.page_size as usize))
            }
            KeyCode::PageDown => {
                self.engine.view = View::Tasks(
                    (selected + self.page_size as usize).min(ids.len().saturating_sub(1)),
                )
            }
            KeyCode::Enter => {
                if let Some(id) = ids.get(selected) {
                    self.engine.view = View::Task(*id, 0);
                }
            }
            KeyCode::Char('r') if key.modifiers.is_empty() => {
                if let Some(id) = ids.get(selected) {
                    self.retry_task(*id)?;
                }
            }
            KeyCode::Char('h') if key.modifiers.is_empty() => {
                self.history_origin = Some(View::Tasks(selected));
                self.engine.view = View::History(0);
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn task_key(
        &mut self,
        key: KeyEvent,
        id: i64,
        scroll: u16,
        history: bool,
    ) -> rusqlite::Result<()> {
        let view = |offset| {
            if history {
                View::TaskHistory(id, offset)
            } else {
                View::Task(id, offset)
            }
        };
        match key.code {
            KeyCode::Esc => {
                self.engine.view = if history {
                    View::Task(id, 0)
                } else {
                    View::Tasks(
                        self.task_ids()
                            .iter()
                            .position(|task| *task == id)
                            .unwrap_or(0),
                    )
                }
            }
            KeyCode::Char('t') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.engine.view = View::Chat
            }
            KeyCode::Up => self.engine.view = view(scroll.saturating_sub(1)),
            KeyCode::Down => self.engine.view = view(scroll.saturating_add(1)),
            KeyCode::PageUp => self.engine.view = view(scroll.saturating_sub(self.page_size)),
            KeyCode::PageDown => self.engine.view = view(scroll.saturating_add(self.page_size)),
            KeyCode::Char('g') if !history && key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.open_actions();
            }
            KeyCode::Char('r')
                if !history
                    && key.modifiers.contains(KeyModifiers::CONTROL)
                    && self.can_retry_task(id) =>
            {
                self.retry_task(id)?;
            }
            KeyCode::Char('r' | 'n')
                if !history && key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                let action = self
                    .action_dock()
                    .actions
                    .into_iter()
                    .find(|item| {
                        matches!(
                            item.action,
                            Action::Run | Action::Retry | Action::RetryGit | Action::Network
                        ) && Some(key.code) == item.action.shortcut().map(KeyCode::Char)
                    })
                    .map(|item| item.action);
                if let Some(action) = action {
                    self.engine.view = View::Chat;
                    self.perform_action(action)?;
                }
            }
            KeyCode::Char('a') if history && key.modifiers.is_empty() => {
                self.history_origin = Some(View::TaskHistory(id, scroll));
                self.engine.view = View::History(0);
            }
            KeyCode::Char('h') if !history && key.modifiers.is_empty() => {
                self.engine.view = View::TaskHistory(id, 0)
            }
            _ => {}
        }
        Ok(())
    }
}
