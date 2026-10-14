use crate::{
    engine::{
        Engine,
        api::{Command, Response},
        ownership,
    },
    git_mod,
    store::{ProjectActivity, ProjectSummary, Store},
};
use serde::Serialize;
use std::{
    collections::BTreeMap,
    io,
    ops::{Deref, DerefMut},
    path::{Path, PathBuf},
};

// Each runtime retains its workers, jobs, navigation, drafts and ownership lock.
pub struct Session {
    pub(crate) foreground: Engine,
    background: BTreeMap<i64, Engine>,
    pub(crate) projects: Vec<ProjectSummary>,
    pub(crate) expanded_project: Option<i64>,
}

#[derive(Serialize)]
pub struct Snapshot {
    pub projects: Vec<ProjectSummary>,
    pub expanded_project: Option<i64>,
    #[serde(flatten)]
    pub workspace: crate::engine::snapshot::Snapshot,
}

impl Deref for Session {
    type Target = Engine;
    fn deref(&self) -> &Engine {
        &self.foreground
    }
}
impl DerefMut for Session {
    fn deref_mut(&mut self) -> &mut Engine {
        &mut self.foreground
    }
}

impl Session {
    pub fn new(project: PathBuf, data_dir: Option<&Path>) -> io::Result<Self> {
        Self::from_engine(Engine::new(project, data_dir)?).map_err(io::Error::other)
    }

    pub(crate) fn from_engine(engine: Engine) -> rusqlite::Result<Self> {
        let projects = engine.store.projects()?;
        Ok(Self {
            expanded_project: Some(engine.project_id),
            foreground: engine,
            background: BTreeMap::new(),
            projects,
        })
    }

    pub fn response(&self, request_id: Option<u64>, error: Option<String>) -> Response {
        Response {
            protocol_version: 1,
            request_id,
            error,
            snapshot: Snapshot {
                projects: self.projects.clone(),
                expanded_project: self.expanded_project,
                workspace: self.foreground.snapshot(),
            },
        }
    }

    pub(crate) fn command(&mut self, command: Command) -> Result<(), String> {
        let result = self.execute(command);
        self.refresh_projects().map_err(|e| e.to_string())?;
        result
    }

    fn execute(&mut self, command: Command) -> Result<(), String> {
        match command {
            Command::ToggleProject { project_id } => {
                self.project_path(project_id)?;
                self.expanded_project =
                    (self.expanded_project != Some(project_id)).then_some(project_id);
                Ok(())
            }
            Command::OpenProject { path } => self.open_project(Path::new(&path), None),
            Command::SelectProject { project_id } => self.select_project(project_id, None),
            Command::BeginProjectCreation { project_id } => {
                if !self.can_begin_creation() {
                    return Err("Finish or cancel the open dialog first.".into());
                }
                self.select_project(project_id, None)?;
                self.foreground.command(Command::BeginCreation {})
            }
            Command::SelectMod { mod_id } => {
                let project_id = self
                    .store
                    .mod_project(mod_id)
                    .map_err(|e| e.to_string())?
                    .ok_or("Codemod no longer exists.")?;
                self.select_project(project_id, Some(mod_id))?;
                self.foreground.command(Command::SelectMod { mod_id })
            }
            _ => self.foreground.command(command),
        }
    }

    fn project_path(&self, id: i64) -> Result<&Path, String> {
        self.projects
            .iter()
            .find(|p| p.id == id)
            .map(|p| Path::new(&p.path))
            .ok_or_else(|| "Project no longer exists.".into())
    }

    fn select_project(&mut self, id: i64, required_mod: Option<i64>) -> Result<(), String> {
        if id == self.project_id {
            self.expanded_project = Some(id);
            return Ok(());
        }
        self.save_before_selection().map_err(|e| e.to_string())?;
        if let Some(next) = self.background.get(&id) {
            Self::require_destination(next, required_mod)?;
            let appearance = self.store.appearance().map_err(|e| e.to_string())?;
            let mut next = self.background.remove(&id).unwrap();
            next.appearance = appearance;
            self.activate(next);
            Ok(())
        } else {
            let path = self.project_path(id)?.to_owned();
            self.open_project(&path, required_mod)
        }
    }

    fn open_project(&mut self, path: &Path, required_mod: Option<i64>) -> Result<(), String> {
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
        if let Some((&id, _)) = self.background.iter().find(|(_, e)| e.project == path) {
            return self.select_project(id, required_mod);
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
        let mut next = Engine::load_with_preparations(
            path,
            Store::open(&database).map_err(|e| e.to_string())?,
            self.preparations.empty_peer(),
        )
        .map_err(|e| e.to_string())?;
        Self::require_destination(&next, required_mod)?;
        next._ownership = Some(ownership);
        next.muse = self.muse;
        self.activate(next);
        Ok(())
    }

    fn require_destination(engine: &Engine, required_mod: Option<i64>) -> Result<(), String> {
        if required_mod.is_some_and(|id| !engine.mods.iter().any(|m| m.id == id)) {
            Err("Codemod no longer exists.".into())
        } else {
            Ok(())
        }
    }

    fn activate(&mut self, next: Engine) {
        let mut previous = std::mem::replace(&mut self.foreground, next);
        previous.view = match previous.navigation.current() {
            crate::engine::Destination::NewCodemod => crate::engine::View::NewMod,
            crate::engine::Destination::Codemod { .. } => crate::engine::View::Chat,
        };
        previous.restore_input();
        self.background.insert(previous.project_id, previous);
        self.expanded_project = Some(self.project_id);
    }

    pub(crate) fn poll_workers(&mut self) -> rusqlite::Result<()> {
        for engine in std::iter::once(&mut self.foreground).chain(self.background.values_mut()) {
            // A failed project must not stop other projects or replace their input.
            engine.poll_error = engine
                .poll_workers()
                .err()
                .map(|error| format!("Project update failed: {error}"));
        }
        self.refresh_projects()
    }

    pub(crate) fn refresh_projects(&mut self) -> rusqlite::Result<()> {
        self.projects = self.store.projects()?;
        for project in &mut self.projects {
            let engine = if project.id == self.foreground.project_id {
                Some(&self.foreground)
            } else {
                self.background.get(&project.id)
            };
            if let Some(engine) = engine {
                project.activity = engine.project_session_activity();
            }
        }
        Ok(())
    }
}

impl Engine {
    fn project_session_activity(&self) -> ProjectActivity {
        if self.poll_error.is_some()
            || self.notice.is_some()
            || self.project_error.is_some()
            || self.dialog().is_some_and(|d| !d.is_inspection())
            || self.mods.iter().any(|m| {
                !m.closed
                    && (m.question().is_some()
                        || self.git_errors.current(m.id).is_some()
                        || self
                            .network_requests
                            .iter()
                            .any(|r| r.mod_id == m.id && r.status == "pending")
                        || (!m.user_paused
                            && (self
                                .workers
                                .values()
                                .any(|w| w.mod_id == m.id && w.error.is_some())
                                || m.execution.as_ref().is_some_and(|e| e.status == "blocked")
                                || m.agent_review.as_ref().is_some_and(|r| {
                                    matches!(r.status.as_str(), "findings" | "blocked" | "paused")
                                }))))
            })
        {
            return ProjectActivity::Attention;
        }
        if self.workers.values().any(|w| w.enabled || w.busy())
            || !self.auto_plans.is_empty()
            || !self.auto_runs.is_empty()
            || !self.auto_reviews.is_empty()
            || !self.executing_mods.is_empty()
            || !self.git_jobs.is_empty()
            || self.project_job.is_some()
            || self.preparations.warming()
        {
            ProjectActivity::Working
        } else {
            ProjectActivity::Idle
        }
    }
}

#[cfg(test)]
mod tests;
