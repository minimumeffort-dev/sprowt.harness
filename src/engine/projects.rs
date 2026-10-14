use super::{Destination, Engine, View, ownership};
use crate::{git_mod, store::Store};
use std::path::Path;

impl Engine {
    pub(crate) fn set_creation_draft(&mut self, text: String) -> rusqlite::Result<()> {
        self.store.save_creation_draft(self.project_id, &text)?;
        self.creation_draft = text.clone();
        if self.view == View::NewMod {
            self.composer = text;
        }
        Ok(())
    }

    pub(crate) fn toggle_project(&mut self, id: i64) -> Result<(), String> {
        if !self.projects.iter().any(|p| p.id == id) {
            return Err("Project no longer exists.".into());
        }
        self.expanded_project = (self.expanded_project != Some(id)).then_some(id);
        Ok(())
    }

    pub(crate) fn select_project(
        &mut self,
        id: i64,
        required_mod: Option<i64>,
    ) -> Result<(), String> {
        if id == self.project_id {
            return Ok(());
        }
        let path = self
            .projects
            .iter()
            .find(|p| p.id == id)
            .ok_or("Project no longer exists.")?
            .path
            .clone();
        self.switch_project(Path::new(&path), required_mod)
    }

    pub(crate) fn open_project(&mut self, path: &Path) -> Result<(), String> {
        self.switch_project(path, None)
    }

    fn switch_project(&mut self, path: &Path, required_mod: Option<i64>) -> Result<(), String> {
        let path = path
            .canonicalize()
            .map_err(|e| format!("Could not open project: {e}"))?;
        if !path.is_dir() {
            return Err("Choose a project folder.".into());
        }
        let path = git_mod::project_root(&path).unwrap_or(path);
        if path == self.project {
            self.expanded_project = Some(self.project_id);
            return Ok(());
        }
        if self.workers.values().any(|w| w.enabled || w.busy())
            || !self.auto_plans.is_empty()
            || !self.auto_runs.is_empty()
            || !self.auto_reviews.is_empty()
            || !self.executing_mods.is_empty()
            || self.mods.iter().any(|m| {
                !m.closed
                    && !m.user_paused
                    && !m.queue.is_empty()
                    && m.execution.as_ref().is_some_and(|e| {
                        matches!(e.status.as_str(), "review" | "applied" | "blocked")
                    })
            })
        {
            return Err("Pause running work in this project before switching projects.".into());
        }
        if !self.git_jobs.is_empty() || self.project_job.is_some() || self.preparations.warming() {
            return Err(
                "Wait for the current project operation to finish, then switch projects.".into(),
            );
        }
        self.save_before_selection().map_err(|e| e.to_string())?;
        let database = Path::new(
            self.store
                .0
                .path()
                .ok_or("Project database is unavailable.")?,
        )
        .to_owned();
        let ownership = ownership::acquire(&database, &path).map_err(|e| e.to_string())?;
        let mut next = Self::load_with_preparations(
            path,
            Store::open(&database).map_err(|e| e.to_string())?,
            self.preparations.empty_peer(),
        )
        .map_err(|e| e.to_string())?;
        if required_mod.is_some_and(|id| !next.mods.iter().any(|m| m.id == id)) {
            return Err("Codemod no longer exists.".into());
        }
        next._ownership = Some(ownership);
        next.muse = self.muse;
        next.project_navigation = self.project_navigation.clone();
        next.project_navigation
            .insert(self.project_id, self.navigation.clone());
        if let Some(mut navigation) = next.project_navigation.remove(&next.project_id) {
            navigation.retain_mods(&next.mods);
            let destination = navigation.current();
            match destination {
                Destination::NewCodemod => {
                    next.navigation = navigation;
                    next.view = View::NewMod;
                }
                Destination::Codemod { mod_id } => {
                    if let Some(index) = next.mods.iter().position(|m| m.id == mod_id) {
                        next.store
                            .select_mod(next.project_id, mod_id)
                            .map_err(|e| e.to_string())?;
                        next.active = Some(index);
                        next.navigation = navigation;
                        next.view = View::Chat;
                    }
                }
            }
            next.restore_input();
        }
        *self = next;
        Ok(())
    }
}
