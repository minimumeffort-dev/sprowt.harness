use super::{App, View};
use crate::engine::api::Command;
use ratatui::crossterm::event::{Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use ratatui_textarea::TextArea;
use rusqlite::Result;

pub enum PickerPanel {
    Projects,
    ProjectMenu(i64),
    Open(Box<TextArea<'static>>),
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum ProjectRow {
    Project(i64),
    Codemod { project: i64, id: i64 },
    Empty(i64),
}

impl ProjectRow {
    pub fn project(self) -> i64 {
        match self {
            Self::Project(id) | Self::Empty(id) => id,
            Self::Codemod { project, .. } => project,
        }
    }
}

impl App {
    pub(super) fn project_picker_focus(&self) -> Option<(usize, ProjectRow)> {
        if !matches!(self.view, View::Mods(_) | View::DeleteMod(_)) {
            return None;
        }
        let rows = self.project_rows();
        let index = match self.view {
            View::Mods(index) => index,
            View::DeleteMod(index) => rows.iter().position(|row| {
                matches!(row,
                ProjectRow::Codemod { id, .. } if *id == self.mods[index].id)
            })?,
            _ => return None,
        };
        rows.get(index).copied().map(|row| (index, row))
    }

    pub(super) fn reconcile_project_picker(&mut self, focus: Option<(usize, ProjectRow)>) {
        if matches!(self.view, View::Mods(_))
            && let Some((index, selected)) = focus
        {
            let rows = self.project_rows();
            self.view = View::Mods(
                rows.iter()
                    .position(|row| *row == selected)
                    .unwrap_or(index.min(rows.len().saturating_sub(1))),
            );
        }
    }

    pub fn project_rows(&self) -> Vec<ProjectRow> {
        let mut rows = Vec::new();
        for project in &self.projects {
            rows.push(ProjectRow::Project(project.id));
            if self.expanded_project == Some(project.id) {
                let before = rows.len();
                rows.extend(
                    project
                        .mods
                        .iter()
                        .filter(|m| m.closed == self.show_closed)
                        .map(|m| ProjectRow::Codemod {
                            project: project.id,
                            id: m.id,
                        }),
                );
                if rows.len() == before {
                    rows.push(ProjectRow::Empty(project.id));
                }
            }
        }
        rows
    }

    pub(super) fn project_command(&mut self, command: Command) {
        let creating = matches!(command, Command::BeginProjectCreation { .. });
        self.project_picker_error = self.with_engine(|engine| engine.command(command)).err();
        if creating && self.project_picker_error.is_none() {
            self.show_closed = false;
        }
    }

    fn open_project_prompt(&mut self) {
        let mut input = crate::ui::name_input();
        input.set_placeholder_text("Project folder path");
        self.project_picker = PickerPanel::Open(Box::new(input));
        self.project_picker_error = None;
    }

    pub(super) fn project_prompt_event(&mut self, event: &Event) -> bool {
        if matches!(event, Event::Key(key) if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.quit = true;
            return true;
        }
        let PickerPanel::Open(input) = &mut self.project_picker else {
            return false;
        };
        match event {
            Event::Key(key) if key.kind == KeyEventKind::Release => {}
            Event::Key(key) if key.code == KeyCode::Esc => {
                self.project_picker = PickerPanel::Projects;
                self.project_picker_error = None;
            }
            Event::Key(key) if key.code == KeyCode::Enter => {
                let path = input.lines().join("\n");
                self.project_command(Command::OpenProject { path });
                if self.project_picker_error.is_none() {
                    self.project_picker = PickerPanel::Projects;
                    let _ = self.with_engine(|engine| engine.dismiss_picker());
                }
            }
            Event::Key(key) => {
                input.input(*key);
            }
            Event::Paste(text) => {
                input.insert_str(text.trim());
            }
            _ => {}
        }
        true
    }

    pub(super) fn picker_key(&mut self, key: KeyEvent, index: usize) -> Result<()> {
        if key.kind == KeyEventKind::Release {
            return Ok(());
        }
        if let PickerPanel::ProjectMenu(id) = self.project_picker {
            match key.code {
                KeyCode::Esc | KeyCode::Char('.') => {
                    self.project_picker = PickerPanel::Projects;
                }
                KeyCode::Enter => {
                    self.project_command(Command::SelectProject { project_id: id });
                    if self.project_picker_error.is_none() {
                        self.project_picker = PickerPanel::Projects;
                        self.with_engine(|engine| engine.dismiss_picker())?;
                    }
                }
                KeyCode::Tab => {
                    self.show_closed = !self.show_closed;
                    self.project_picker = PickerPanel::Projects;
                }
                KeyCode::Char('x') => {
                    self.project_command(Command::RemoveProject { project_id: id });
                    if self.project_picker_error.is_none() {
                        self.project_picker = PickerPanel::Projects;
                    }
                }
                _ => {}
            }
            return Ok(());
        }
        let rows = self.project_rows();
        let selected = rows.get(index).copied();
        let project = selected.map_or(self.project_id, ProjectRow::project);
        match key.code {
            KeyCode::Up => self.engine.view = View::Mods(index.saturating_sub(1)),
            KeyCode::Down => {
                self.engine.view = View::Mods((index + 1).min(rows.len().saturating_sub(1)))
            }
            KeyCode::Tab => {
                self.show_closed = !self.show_closed;
                self.engine.view = View::Mods(0);
            }
            KeyCode::Esc => {
                self.with_engine(|engine| engine.dismiss_picker())?;
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.with_engine(|engine| engine.dismiss_picker())?;
            }
            KeyCode::Char('+') | KeyCode::Char('o')
                if !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.open_project_prompt()
            }
            KeyCode::Char('.') if selected.is_some() => {
                self.project_picker = PickerPanel::ProjectMenu(project)
            }
            KeyCode::Char('n') if selected.is_some() => {
                self.project_command(Command::BeginProjectCreation {
                    project_id: project,
                })
            }
            KeyCode::Enter | KeyCode::Left | KeyCode::Right => match selected {
                Some(ProjectRow::Project(id)) => {
                    if key.code == KeyCode::Enter
                        || (key.code == KeyCode::Right) != (self.expanded_project == Some(id))
                    {
                        self.project_command(Command::ToggleProject { project_id: id });
                    }
                }
                Some(ProjectRow::Codemod { id, .. }) if key.code == KeyCode::Enter => {
                    self.project_command(Command::SelectMod { mod_id: id })
                }
                _ => {}
            },
            KeyCode::Char(action @ ('d' | 'c' | 'r'))
                if key.modifiers.is_empty()
                    && (action == 'd' || (action == 'c') != self.show_closed) =>
            {
                if let Some(ProjectRow::Codemod { id, .. }) = selected {
                    if project != self.project_id {
                        self.project_command(Command::SelectProject {
                            project_id: project,
                        });
                    }
                    if self.project_picker_error.is_none() {
                        let Some(index) = self.mods.iter().position(|m| m.id == id) else {
                            return Ok(());
                        };
                        match action {
                            'd' => self.engine.view = View::DeleteMod(index),
                            'c' if !self.show_closed => self.engine.view = View::CloseMod(index),
                            'r' if self.show_closed => self.reopen_mod(index),
                            _ => {}
                        }
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }
}
