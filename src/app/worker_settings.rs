use super::*;
use crate::worker_settings::{Allocation, ModelChoice, WorkerSettings};

pub(crate) struct Editor {
    pub settings: WorkerSettings,
    pub mod_id: Option<i64>,
    pub row: usize,
    pub field: usize,
    pub error: Option<String>,
}
impl Editor {
    pub fn choice(&self, row: usize) -> &ModelChoice {
        if row == 0 {
            &self.settings.planner
        } else if row <= self.settings.executors.len() {
            &self.settings.executors[row - 1].choice
        } else {
            &self.settings.reviewer
        }
    }
    fn choice_mut(&mut self) -> &mut ModelChoice {
        if self.row == 0 {
            &mut self.settings.planner
        } else if self.row <= self.settings.executors.len() {
            &mut self.settings.executors[self.row - 1].choice
        } else {
            &mut self.settings.reviewer
        }
    }
}
impl App {
    pub(crate) fn open_worker_settings(&mut self) -> Result<()> {
        let mod_id = if self.navigation.current() == crate::engine::Destination::NewCodemod {
            None
        } else {
            self.current_mod().map(|m| m.id)
        };
        self.worker_settings_editor = Some(Editor {
            settings: self.engine.settings_for(mod_id)?,
            mod_id,
            row: 0,
            field: 1,
            error: None,
        });
        self.engine.refresh_models();
        Ok(())
    }
    pub(crate) fn worker_settings_event(&mut self, event: Event) {
        let Event::Key(key) = event else {
            return;
        };
        if key.kind == KeyEventKind::Release {
            return;
        }
        if key.code == KeyCode::Char('c') && key.modifiers.contains(KeyModifiers::CONTROL) {
            self.quit = true;
            return;
        }
        if key.code == KeyCode::Esc {
            self.worker_settings_editor = None;
            return;
        }
        if key.code == KeyCode::Char('r') {
            self.engine.refresh_models();
            return;
        }
        if key.code == KeyCode::Char('s') {
            let editor = self.worker_settings_editor.as_ref().unwrap();
            let (id, settings) = (editor.mod_id, editor.settings.clone());
            match self.engine.save_settings(id, settings) {
                Ok(()) => self.worker_settings_editor = None,
                Err(error) => self.worker_settings_editor.as_mut().unwrap().error = Some(error),
            }
            return;
        }
        let catalog = self.engine.models.catalog.models.clone();
        let editor = self.worker_settings_editor.as_mut().unwrap();
        editor.error = None;
        match key.code {
            KeyCode::Up => editor.row = editor.row.saturating_sub(1),
            KeyCode::Down => editor.row = (editor.row + 1).min(editor.settings.executors.len() + 1),
            KeyCode::Tab => editor.field = (editor.field + 1) % 3,
            KeyCode::Char('+') if editor.settings.capacity() < 16 => {
                if let Some(model) = catalog.iter().find(|m| {
                    !editor
                        .settings
                        .executors
                        .iter()
                        .any(|a| a.choice.provider == m.provider && a.choice.model == m.model)
                }) {
                    editor.settings.executors.push(Allocation {
                        choice: ModelChoice {
                            provider: model.provider.clone(),
                            model: model.model.clone(),
                            effort: model.default_effort.clone(),
                        },
                        count: 1,
                    });
                    editor.row = editor.settings.executors.len();
                }
            }
            KeyCode::Char('-')
                if editor.row > 0
                    && editor.row <= editor.settings.executors.len()
                    && editor.settings.executors.len() > 1 =>
            {
                editor.settings.executors.remove(editor.row - 1);
                editor.row = editor.row.min(editor.settings.executors.len());
            }
            KeyCode::Left | KeyCode::Right | KeyCode::Enter => {
                let up = key.code != KeyCode::Left;
                if editor.field == 0
                    && editor.row > 0
                    && editor.row <= editor.settings.executors.len()
                {
                    let total = editor.settings.capacity();
                    let count = &mut editor.settings.executors[editor.row - 1].count;
                    if up && total < 16 {
                        *count += 1;
                    } else if !up {
                        *count = count.saturating_sub(1).max(1);
                    }
                } else if editor.field == 2 {
                    let choice = editor.choice_mut();
                    if let Some(model) = catalog
                        .iter()
                        .find(|m| m.model == choice.model && m.provider == choice.provider)
                    {
                        let index = model
                            .efforts
                            .iter()
                            .position(|e| e == &choice.effort)
                            .unwrap_or(0);
                        let next = if up {
                            (index + 1) % model.efforts.len()
                        } else {
                            (index + model.efforts.len() - 1) % model.efforts.len()
                        };
                        choice.effort = model.efforts[next].clone();
                    }
                } else if !catalog.is_empty() {
                    let choice = editor.choice_mut();
                    let index = catalog
                        .iter()
                        .position(|m| m.model == choice.model && m.provider == choice.provider)
                        .unwrap_or(0);
                    let next = if up {
                        (index + 1) % catalog.len()
                    } else {
                        (index + catalog.len() - 1) % catalog.len()
                    };
                    let model = &catalog[next];
                    *choice = ModelChoice {
                        provider: model.provider.clone(),
                        model: model.model.clone(),
                        effort: model.default_effort.clone(),
                    };
                }
            }
            _ => {}
        }
    }
}
