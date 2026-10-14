use super::App;
use crate::{appearance::Appearance, ui::theme::Palette};
use ratatui::crossterm::event::{Event, KeyCode, KeyEventKind, KeyModifiers};

impl App {
    pub(crate) fn palette(&self) -> Palette {
        Palette::new(self.engine.appearance, self.system_appearance.dark)
    }

    pub(super) fn appearance_event(&mut self, event: Event) {
        let Event::Key(key) = event else {
            return;
        };
        if key.kind == KeyEventKind::Release {
            return;
        }
        let selected = self.appearance_picker.unwrap();
        match key.code {
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => self.quit = true,
            KeyCode::Esc => {
                self.appearance_picker = None;
                self.appearance_error = None;
            }
            KeyCode::Up | KeyCode::Down => {
                let index = Appearance::ALL
                    .iter()
                    .position(|value| *value == selected)
                    .unwrap();
                let next = if key.code == KeyCode::Up {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(2)
                };
                self.appearance_picker = Some(Appearance::ALL[next]);
            }
            KeyCode::Enter if key.kind == KeyEventKind::Press => {
                match self.engine.set_appearance(selected) {
                    Ok(()) => {
                        self.appearance_picker = None;
                        self.appearance_error = None;
                    }
                    Err(error) => {
                        self.appearance_error = Some(format!("Could not save appearance: {error}"))
                    }
                }
            }
            _ => {}
        }
    }
}
