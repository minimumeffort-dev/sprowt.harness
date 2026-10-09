use super::{Action, App, View};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};

impl View {
    pub fn details_tab(self) -> Option<usize> {
        match self {
            Self::Tasks(_) | Self::Task(_, _) => Some(0),
            Self::Checks(_) => Some(1),
            Self::Findings(_) => Some(2),
            Self::History(_) | Self::TaskHistory(_, _) => Some(3),
            _ => None,
        }
    }
}

impl App {
    pub(super) fn inspector_key(&mut self, key: KeyEvent) -> rusqlite::Result<bool> {
        let Some(tab) = self.view.details_tab() else {
            return Ok(false);
        };
        if key.kind == KeyEventKind::Release {
            return Ok(false);
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) && key.code == KeyCode::Char('t') {
            self.view = View::Chat;
            return Ok(true);
        }
        let next = match key.code {
            KeyCode::Char('1'..='4') if key.modifiers.is_empty() => {
                if let KeyCode::Char(c) = key.code {
                    Some(c as usize - '1' as usize)
                } else {
                    None
                }
            }
            KeyCode::Tab => Some((tab + 1) % 4),
            KeyCode::BackTab => Some((tab + 3) % 4),
            _ => None,
        };
        if let Some(next) = next {
            self.view = [
                View::Tasks(0),
                View::Checks(0),
                View::Findings(0),
                View::History(0),
            ][next];
            self.history_origin = None;
            return Ok(true);
        }
        if key.code == KeyCode::Char('e')
            && key.modifiers.is_empty()
            && matches!(self.view, View::Checks(_) | View::Task(_, _))
        {
            self.show_evidence = !self.show_evidence;
            return Ok(true);
        }
        if let View::Checks(scroll) = self.view {
            if key.code == KeyCode::Char('x')
                && key.modifiers.is_empty()
                && key.kind == KeyEventKind::Press
                && let Some(id) = self.failed_check_owner()
            {
                self.perform_action(Action::FixCheck(id))?;
                return Ok(true);
            }
            self.view = match key.code {
                KeyCode::Esc => View::Chat,
                KeyCode::Up => View::Checks(scroll.saturating_sub(1)),
                KeyCode::Down => View::Checks(scroll.saturating_add(1)),
                KeyCode::PageUp => View::Checks(scroll.saturating_sub(self.page_size)),
                KeyCode::PageDown => View::Checks(scroll.saturating_add(self.page_size)),
                KeyCode::Home => View::Checks(0),
                KeyCode::End => View::Checks(u16::MAX),
                _ => self.view,
            };
            return Ok(true);
        }
        Ok(false)
    }
}
