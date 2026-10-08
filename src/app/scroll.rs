use super::{App, View};
use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers, MouseEvent, MouseEventKind},
    layout::{Position, Rect},
};
use rusqlite::Result;

#[derive(Default)]
pub struct Scroll {
    pub content: Rect,
    pub input: Rect,
    pub offset: u16,
    view: Option<View>,
}

impl Scroll {
    pub fn begin(&mut self, view: View) {
        if self.view != Some(view) {
            self.offset = 0;
        }
        self.content = Rect::default();
        self.input = Rect::default();
        self.view = Some(view);
    }

    pub fn finish(&mut self, view: View) {
        self.view = Some(view);
    }
}

impl App {
    pub(super) fn mouse_scroll(&mut self, mouse: MouseEvent) -> Result<()> {
        let up = match mouse.kind {
            MouseEventKind::ScrollUp => true,
            MouseEventKind::ScrollDown => false,
            _ => return Ok(()),
        };
        if self.scroll.view != Some(self.view) {
            return Ok(());
        }
        let position = Position::new(mouse.column, mouse.row);
        if self.scroll.input.contains(position) {
            self.input.scroll((if up { -3 } else { 3 }, 0));
            return Ok(());
        }
        if !self.scroll.content.contains(position) {
            return Ok(());
        }
        let key = KeyEvent::new(
            if up { KeyCode::Up } else { KeyCode::Down },
            KeyModifiers::NONE,
        );
        let shift = |offset: u16| {
            if up {
                offset.saturating_sub(3)
            } else {
                offset.saturating_add(3)
            }
        };
        self.view = match self.view {
            View::Chat | View::EditQueue(_) => {
                self.history_offset = if up {
                    self.history_offset.saturating_add(3)
                } else {
                    self.history_offset.saturating_sub(3)
                };
                self.focus_plan = false;
                self.view
            }
            View::Actions(selected) => return self.actions_key(key, selected),
            View::Mods(index) => return self.picker_key(key, index),
            View::Queue(index) => return self.queue_key(key, index),
            View::Tasks(index) => {
                self.tasks_key(key, index)?;
                return Ok(());
            }
            View::Task(id, offset) => View::Task(id, shift(offset)),
            View::TaskHistory(id, offset) => View::TaskHistory(id, shift(offset)),
            View::Failure(offset) => View::Failure(shift(offset)),
            View::Review(offset) => View::Review(shift(offset)),
            View::Findings(offset) => View::Findings(shift(offset)),
            View::History(offset) => View::History(shift(offset)),
            View::ProjectSetup(saved, offset) => View::ProjectSetup(saved, shift(offset)),
            View::Network(id, offset) => View::Network(id, shift(offset)),
            View::DeleteMod(_)
            | View::CloseMod(_)
            | View::Publish
            | View::ConfirmRepository(_)
            | View::Repository(_) => {
                self.scroll.offset = shift(self.scroll.offset);
                self.view
            }
            View::NewMod => self.view,
        };
        Ok(())
    }
}
