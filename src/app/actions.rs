use super::{Action, App, View};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use rusqlite::Result;
impl App {
    pub(super) fn findings_key(&mut self, key: KeyEvent, scroll: u16) -> Result<()> {
        match key.code {
            KeyCode::Esc => self.engine.view = View::Chat,
            KeyCode::Up => self.engine.view = View::Findings(scroll.saturating_sub(1)),
            KeyCode::Down => self.engine.view = View::Findings(scroll.saturating_add(1)),
            KeyCode::PageUp => {
                self.engine.view = View::Findings(scroll.saturating_sub(self.page_size))
            }
            KeyCode::PageDown => {
                self.engine.view = View::Findings(scroll.saturating_add(self.page_size))
            }
            KeyCode::Char('v') if key.modifiers.is_empty() && key.kind == KeyEventKind::Press => {
                self.perform_action(Action::ReviseRepair)?;
            }
            KeyCode::Char('x') if key.modifiers.is_empty() && key.kind == KeyEventKind::Press => {
                self.perform_action(Action::FixIssues)?;
            }
            KeyCode::Char('e')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.kind == KeyEventKind::Press =>
            {
                self.perform_action(Action::Review)?;
            }
            KeyCode::Char('r')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.kind == KeyEventKind::Press =>
            {
                self.action_shortcut('r')?;
            }
            KeyCode::Char('h') if key.modifiers.is_empty() => {
                self.history_origin = Some(View::Findings(scroll));
                self.engine.view = View::History(0);
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn open_actions(&mut self) {
        self.engine.action_origin = Some(self.engine.view);
        let dock = self.action_dock();
        if let Some(first) = dock.menu_actions().first() {
            self.engine.view = View::Actions(dock.primary.unwrap_or(first.action));
        }
    }

    pub(super) fn actions_key(&mut self, key: KeyEvent, selected: Action) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let dock = self.action_dock();
        let actions = dock.menu_actions();
        let index = actions
            .iter()
            .position(|a| a.action == selected)
            .unwrap_or(0);
        match key.code {
            KeyCode::Esc | KeyCode::Char('g') if key.code == KeyCode::Esc || ctrl => {
                self.engine.view = self.engine.action_origin.take().unwrap_or(View::Chat);
            }
            KeyCode::Up | KeyCode::Down => {
                let next = if key.code == KeyCode::Up {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(actions.len().saturating_sub(1))
                };
                if let Some(item) = actions.get(next) {
                    self.engine.view = View::Actions(item.action);
                }
            }
            KeyCode::Enter if key.modifiers.is_empty() && key.kind == KeyEventKind::Press => {
                // A changed state must not redirect Enter to a different action.
                if actions.iter().any(|a| a.action == selected) {
                    self.engine.view = self.engine.action_origin.take().unwrap_or(View::Chat);
                    self.perform_action(selected)?;
                }
            }
            KeyCode::Char(c) if ctrl && Action::has_shortcut(c) => {
                self.engine.view = self.engine.action_origin.take().unwrap_or(View::Chat);
                self.action_shortcut(c)?;
            }
            KeyCode::Char(c) if key.modifiers.is_empty() && key.kind == KeyEventKind::Press => {
                if let Some(item) = dock
                    .actions
                    .iter()
                    .find(|a| a.action.menu_shortcut() == Some(c))
                {
                    self.engine.view = self.engine.action_origin.take().unwrap_or(View::Chat);
                    self.perform_action(item.action)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn action_shortcut(&mut self, key: char) -> Result<()> {
        if let Some(item) = self
            .action_dock()
            .actions
            .into_iter()
            .find(|a| a.action.shortcut() == Some(key))
        {
            self.perform_action(item.action)?;
        }
        Ok(())
    }
}
