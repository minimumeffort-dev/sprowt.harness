use super::{Action, App, View};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

impl View {
    // Full-screen evidence reached by drilling in from the workbench or
    // Ctrl+T. Each view stands alone; Esc or Ctrl+T returns to the feed.
    pub fn is_details(self) -> bool {
        matches!(
            self,
            Self::Tasks(_)
                | Self::Task(_, _)
                | Self::Checks(_)
                | Self::Findings(_)
                | Self::History(_)
                | Self::TaskHistory(_, _)
        )
    }
}

impl App {
    pub(super) fn inspector_key(&mut self, key: KeyEvent) -> rusqlite::Result<bool> {
        if !self.engine.view.is_details() {
            return Ok(false);
        }
        if key.kind == KeyEventKind::Release {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('t') {
            self.engine.view = View::Chat;
            return Ok(true);
        }
        if key.code == KeyCode::Char('e')
            && key.modifiers.is_empty()
            && matches!(self.engine.view, View::Checks(_) | View::Task(_, _))
        {
            self.show_evidence = !self.show_evidence;
            return Ok(true);
        }
        if let View::Checks(scroll) = self.engine.view {
            if key.code == KeyCode::Char('x')
                && key.modifiers.is_empty()
                && key.kind == KeyEventKind::Press
                && let Some(id) = self.failed_check_owner()
            {
                self.perform_action(Action::FixCheck(id))?;
                return Ok(true);
            }
            self.engine.view = match key.code {
                KeyCode::Esc => View::Chat,
                KeyCode::Up => View::Checks(scroll.saturating_sub(1)),
                KeyCode::Down => View::Checks(scroll.saturating_add(1)),
                KeyCode::PageUp => View::Checks(scroll.saturating_sub(self.page_size)),
                KeyCode::PageDown => View::Checks(scroll.saturating_add(self.page_size)),
                KeyCode::Home => View::Checks(0),
                KeyCode::End => View::Checks(u16::MAX),
                _ => self.engine.view,
            };
            return Ok(true);
        }
        Ok(false)
    }
}
