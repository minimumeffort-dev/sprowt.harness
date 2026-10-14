use super::{Engine, View};
use rusqlite::Result;
use serde::Serialize;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Destination {
    Codemod { mod_id: i64 },
    NewCodemod,
}

pub(crate) struct Navigation {
    entries: Vec<Destination>,
    cursor: usize,
}

#[derive(Serialize)]
pub struct NavigationSnapshot {
    pub destination: Destination,
    pub can_go_back: bool,
    pub can_go_forward: bool,
}

impl Navigation {
    pub(crate) fn new(destination: Destination) -> Self {
        Self {
            entries: vec![destination],
            cursor: 0,
        }
    }

    pub(crate) fn current(&self) -> Destination {
        self.entries[self.cursor]
    }

    fn visit(&mut self, destination: Destination) {
        if self.current() == destination {
            return;
        }
        self.entries.truncate(self.cursor + 1);
        self.entries.push(destination);
        self.cursor += 1;
    }

    fn target(&self, back: bool) -> Option<(usize, Destination)> {
        let index = if back {
            self.cursor.checked_sub(1)?
        } else {
            self.cursor + 1
        };
        self.entries
            .get(index)
            .copied()
            .map(|destination| (index, destination))
    }

    pub(crate) fn forget(&mut self, mod_id: i64) {
        let removed = Destination::Codemod { mod_id };
        let before = self.entries[..self.cursor]
            .iter()
            .filter(|d| **d != removed)
            .count();
        let current_removed = self.current() == removed;
        self.entries.retain(|d| *d != removed);
        if self.entries.is_empty() {
            self.entries.push(Destination::NewCodemod);
            self.cursor = 0;
        } else {
            self.cursor = before
                .saturating_sub(usize::from(current_removed))
                .min(self.entries.len() - 1);
        }
    }
}

impl Engine {
    fn navigation_allowed(&self) -> bool {
        // Read-only evidence views are safe to leave; navigation saves the
        // draft and lands on Chat, like selecting another codemod does.
        matches!(
            self.view,
            View::Chat
                | View::NewMod
                | View::Review(_)
                | View::Findings(_)
                | View::Failure(_)
                | View::Sync(_)
                | View::Tasks(_)
                | View::Task(_, _)
                | View::TaskHistory(_, _)
                | View::Checks(_)
                | View::History(_)
        )
    }

    pub fn navigation_snapshot(&self) -> NavigationSnapshot {
        NavigationSnapshot {
            destination: self.navigation.current(),
            can_go_back: self.navigation_allowed() && self.navigation.target(true).is_some(),
            can_go_forward: self.navigation_allowed() && self.navigation.target(false).is_some(),
        }
    }

    fn save_navigation_input(&mut self) -> Result<()> {
        if self.navigation.current() == Destination::NewCodemod
            || self.composer_view() == View::NewMod
        {
            self.creation_draft = self.composer.clone();
        } else if !self.read_only()
            && matches!(self.composer_view(), View::Chat | View::EditQueue(_))
        {
            // Inspection views do not edit the composer; persisting it here
            // would replace the saved draft with stale text.
            self.save_draft()?;
        }
        Ok(())
    }

    pub(crate) fn can_begin_creation(&self) -> bool {
        matches!(
            self.composer_view(),
            View::Chat | View::NewMod | View::Mods(_)
        )
    }

    pub(crate) fn begin_creation(&mut self) -> Result<()> {
        if !self.can_begin_creation() {
            return Err(navigation_blocked());
        }
        self.save_navigation_input()?;
        self.navigation.visit(Destination::NewCodemod);
        self.view = View::NewMod;
        self.notice = None;
        self.restore_input();
        Ok(())
    }

    pub(crate) fn navigate(&mut self, back: bool) -> Result<()> {
        if !self.navigation_allowed() {
            return Err(navigation_blocked());
        }
        let Some((cursor, destination)) = self.navigation.target(back) else {
            return Ok(());
        };
        self.save_navigation_input()?;
        match destination {
            Destination::Codemod { mod_id } => {
                let index = self
                    .mods
                    .iter()
                    .position(|m| m.id == mod_id)
                    .expect("navigation contains saved codemods");
                self.store.select_mod(self.project_id, mod_id)?;
                self.active = Some(index);
                self.view = View::Chat;
            }
            Destination::NewCodemod => self.view = View::NewMod,
        }
        self.navigation.cursor = cursor;
        self.notice = None;
        self.restore_input();
        Ok(())
    }

    pub(crate) fn visit_selected(&mut self) {
        let destination =
            self.current_mod()
                .map_or(Destination::NewCodemod, |m| Destination::Codemod {
                    mod_id: m.id,
                });
        if self.navigation.current() == Destination::NewCodemod
            && destination != Destination::NewCodemod
        {
            self.creation_draft = self.composer.clone();
        }
        self.navigation.visit(destination);
    }

    pub(crate) fn dismiss_picker(&mut self) -> Result<()> {
        self.save_navigation_input()?;
        self.view = if self.navigation.current() == Destination::NewCodemod {
            View::NewMod
        } else {
            View::Chat
        };
        self.restore_input();
        Ok(())
    }

    pub(crate) fn save_before_selection(&mut self) -> Result<()> {
        if !self.navigation_allowed() && !matches!(self.view, View::Mods(_)) {
            return Err(navigation_blocked());
        }
        self.save_navigation_input()
    }
}

fn navigation_blocked() -> rusqlite::Error {
    rusqlite::Error::ToSqlConversionFailure(Box::new(std::io::Error::other(
        "Finish or cancel the open dialog first.",
    )))
}
