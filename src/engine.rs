use crate::{
    git_mod::{self, GitMod, Job},
    plan::Role,
    store::{CodeMod, Store, WorkerRecord, source_id},
    tools::{Context, Dispatcher, Request},
    worker::{Status, Worker},
    workspace::{self, Review},
};
use rusqlite::Result;
use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::PathBuf,
    time::{Duration, Instant},
};

mod actions;
pub mod agents;
pub mod api;
mod git;
mod git_errors;
mod interaction;
mod navigation;
pub(crate) mod ownership;
mod poll;
pub(crate) mod preparation;
mod projects;
mod review;
pub(crate) mod snapshot;
pub(crate) mod state;
mod sync;
mod tasks;
mod worker_settings;
mod workers;
pub mod workspace_view;
pub use actions::{Action, ActionDock, ActionItem, Tone};
pub use navigation::{Destination, NavigationSnapshot};
pub use tasks::CheckOrigin;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Chat,
    Actions(Action),
    Failure(u16),
    Sync(u16),
    Mods(usize),
    DeleteMod(usize),
    CloseMod(usize),
    NewMod,
    Queue(usize),
    EditQueue(usize),
    Review(u16),
    Findings(u16),
    PrFeedback(usize),
    Checks(u16),
    History(u16),
    Tasks(usize),
    Task(i64, u16),
    TaskHistory(i64, u16),
    Publish,
    ProjectSetup(bool, u16),
    Repository(bool),
    ConfirmRepository(bool),
    Network(i64, u16),
}

pub struct Engine {
    pub(crate) models: worker_settings::Models,
    pub(crate) appearance: crate::appearance::Appearance,
    pub(crate) network_requests: Vec<crate::network::Access>,
    pub(crate) muse: bool,
    pub(crate) project: PathBuf,
    pub(crate) mods: Vec<CodeMod>,
    pub(crate) view: View,
    pub(crate) action_origin: Option<View>,
    pub(crate) review: Option<Review>,
    pub(crate) publication: Option<crate::publication::Draft>,
    pub(crate) publish_after_review: bool,
    pub(crate) setup_files: Vec<String>,
    pub(crate) setup_root: Option<PathBuf>,
    pub(crate) workers: BTreeMap<i64, Worker>,
    pub(crate) preparations: preparation::Preparations,
    pub(crate) worker_waits: BTreeSet<i64>,
    pub(crate) slot_waits: BTreeMap<(i64, String), Instant>,
    pub(crate) steer_target: Option<i64>,
    pub(crate) auto_plans: BTreeSet<i64>,
    pub(crate) auto_runs: BTreeSet<i64>,
    pub(crate) auto_reviews: BTreeSet<i64>,
    pub(crate) executing_mods: BTreeSet<i64>,
    pub(crate) git_jobs: BTreeMap<i64, Job>,
    pub(crate) git_errors: git_errors::Errors,
    pub(crate) project_job: Option<Job>,
    pub(crate) project_check: Option<Instant>,
    pub(crate) project_error: Option<String>,
    pub(crate) project_sync_root: PathBuf,
    pub(crate) git_states: BTreeMap<i64, GitMod>,
    pub(crate) targets: BTreeMap<i64, crate::mod_sync::Target>,
    pub(crate) target_checks: BTreeMap<i64, Instant>,
    pub(crate) target_errors: BTreeMap<i64, String>,
    pub(crate) update_errors: BTreeSet<i64>,
    pub(crate) notice: Option<String>,
    pub(crate) poll_error: Option<String>,
    pub(crate) store: Store,
    pub(crate) project_id: i64,
    pub(crate) active: Option<usize>,
    pub(crate) composer: String,
    pub(crate) creation_draft: String,
    pub(crate) navigation: navigation::Navigation,
    pub(crate) celebrations: u64,
    pub(crate) _ownership: Option<ownership::Ownership>,
}

impl Engine {
    pub fn new(project: PathBuf, data_dir: Option<&std::path::Path>) -> io::Result<Self> {
        let project = project.canonicalize()?;
        let project = git_mod::project_root(&project).unwrap_or(project);
        let database = match data_dir {
            Some(root) => root.join("state.db"),
            None => Store::local_path()?,
        };
        let ownership = ownership::acquire(&database, &project)?;
        let muse = crate::agents::detect()?;
        let mut app = Self::load_with_preparations(
            project,
            Store::open(&database)?,
            preparation::Preparations::production(),
        )
        .map_err(io::Error::other)?;
        app.muse = muse;
        app._ownership = Some(ownership);
        Ok(app)
    }

    #[cfg(test)]
    pub(crate) fn load(project: PathBuf, store: Store) -> Result<Self> {
        Self::load_with_preparations(project, store, preparation::Preparations::fake())
    }

    pub(crate) fn load_with_preparations(
        project: PathBuf,
        store: Store,
        preparations: preparation::Preparations,
    ) -> Result<Self> {
        let mut state = store.load_project(&project)?;
        let git_states = state
            .mods
            .iter()
            .filter_map(|m| {
                m.git_root
                    .as_ref()
                    .and_then(|root| git_mod::load(root).ok())
                    .map(|state| (m.id, state))
            })
            .collect::<BTreeMap<_, _>>();
        for code_mod in &mut state.mods {
            if let Some(review) = &code_mod.agent_review {
                store.review_status(code_mod.id, &review.source, "paused")?;
                code_mod.agent_review = store.review_state(code_mod.id)?;
            }
            if let Some(git) = git_states.get(&code_mod.id)
                && git.published()
                && let Some(url) = &git.pr
            {
                if code_mod
                    .execution
                    .as_ref()
                    .is_some_and(|e| e.status == "applied")
                {
                    store.execution_status(code_mod.id, "review")?;
                    code_mod.execution = store.execution(code_mod.id)?;
                }
                let item_id = format!("pr:{}", code_mod.id);
                if !code_mod
                    .messages
                    .iter()
                    .any(|message| message.item_id.as_deref() == Some(&item_id))
                {
                    let message = crate::store::Message {
                        source: None,
                        task: None,
                        item_id: Some(item_id),
                        role: "harness".into(),
                        body: format!("PR ready · {url}"),
                        model: None,
                        effort: None,
                    };
                    store.save_message(code_mod.id, &message)?;
                    code_mod.messages.push(message);
                }
            }
        }
        let active = state
            .mods
            .iter()
            .position(|code_mod| Some(code_mod.id) == state.active_mod_id && !code_mod.closed)
            .or_else(|| state.mods.iter().position(|m| !m.closed));
        if let Some(index) = active {
            store.select_mod(state.id, state.mods[index].id)?;
        }
        let mut app = Self {
            models: Default::default(),
            appearance: store.appearance()?,
            composer: String::new(),
            creation_draft: store.creation_draft(state.id)?,
            navigation: navigation::Navigation::new(active.map_or(
                Destination::NewCodemod,
                |index| Destination::Codemod {
                    mod_id: state.mods[index].id,
                },
            )),
            _ownership: None,
            celebrations: 0,
            network_requests: Vec::new(),
            muse: false,
            project_sync_root: store
                .project_path(state.id)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
            project,
            mods: state.mods,
            view: if active.is_some() {
                View::Chat
            } else {
                View::NewMod
            },
            action_origin: None,
            review: None,
            publication: None,
            publish_after_review: false,
            setup_files: Vec::new(),
            setup_root: None,
            workers: BTreeMap::new(),
            preparations,
            worker_waits: BTreeSet::new(),
            slot_waits: BTreeMap::new(),
            steer_target: None,
            auto_plans: BTreeSet::new(),
            auto_runs: BTreeSet::new(),
            auto_reviews: BTreeSet::new(),
            executing_mods: BTreeSet::new(),
            git_jobs: BTreeMap::new(),
            git_errors: git_errors::Errors::default(),
            project_job: None,
            project_check: None,
            project_error: None,
            git_states,
            targets: BTreeMap::new(),
            target_checks: BTreeMap::new(),
            target_errors: BTreeMap::new(),
            update_errors: BTreeSet::new(),
            notice: None,
            poll_error: None,
            store,
            project_id: state.id,
            active,
        };
        app.restore_input();
        Ok(app)
    }

    pub fn prune_closed(&mut self, days: u32) -> Result<()> {
        let expired = self.store.expired_worktrees(self.project_id, days)?;
        for (id, root) in expired {
            if self.git_jobs.contains_key(&id)
                || git_mod::load(&root).map_or(true, |s| s.phase == "pruned")
            {
                continue;
            }
            let Some(code_mod) = self.mods.iter().find(|m| m.id == id) else {
                continue;
            };
            let context = Context::harness(&self.project, code_mod);
            self.git_jobs.insert(
                id,
                Job::start("pruning closed worktree", move |cancelled| {
                    Dispatcher::new(&context, None, cancelled)
                        .execute(Request::PruneMod, |_| {})?;
                    Ok(git_mod::Result::Pruned)
                }),
            );
        }
        Ok(())
    }

    pub fn current_mod(&self) -> Option<&CodeMod> {
        self.active.map(|index| &self.mods[index])
    }

    pub(crate) fn restore_input(&mut self) {
        self.composer = if matches!(self.view, View::Chat | View::Queue(_) | View::Review(_)) {
            self.current_mod()
                .map(|m| m.draft.clone())
                .unwrap_or_default()
        } else if matches!(self.view, View::NewMod) {
            self.creation_draft.clone()
        } else {
            String::new()
        };
    }

    pub(crate) fn clamp_queue(&mut self, index: usize) {
        let len = self
            .current_mod()
            .map_or(0, |code_mod| code_mod.queue.len());
        if len == 0 {
            self.view = View::Chat;
        } else {
            self.view = View::Queue(index.min(len - 1));
        }
    }

    pub(crate) fn celebrate(&mut self) {
        self.celebrations += 1;
    }

    pub(crate) fn save_draft(&mut self) -> Result<()> {
        if let Some(index) = self.active {
            let draft = self.composer.clone();
            let code_mod = &mut self.mods[index];
            if draft != code_mod.draft {
                self.store.save_draft(code_mod.id, &draft)?;
                code_mod.draft = draft;
            }
        }
        Ok(())
    }

    pub(crate) fn submission_error(&self, message: &str) -> Option<&'static str> {
        if self.read_only()
            || self.git_activity().is_some_and(|activity| {
                matches!(
                    activity,
                    "closing mod" | "deleting mod" | "reopening codemod"
                )
            })
        {
            Some("Wait for this operation to finish.")
        } else if message.len() > 4000 && self.current_mod().and_then(CodeMod::question).is_some() {
            Some("Keep the answer under 4,000 bytes.")
        } else {
            None
        }
    }

    pub(crate) fn submit(&mut self) -> Result<()> {
        if let Some(error) = self.submission_error(&self.composer) {
            self.notice = Some(error.into());
            return Ok(());
        }
        let message = self.composer.clone();
        if let Some(index) = self.active
            && !message.trim().is_empty()
        {
            let code_mod = &mut self.mods[index];
            if let Some(question) = code_mod.question() {
                let reply = self.store.user_answer(code_mod.id, question.id, &message)?;
                code_mod.messages.push(reply);
                code_mod.coordination = self.store.mailbox(code_mod.id)?;
            } else {
                let queued = self.store.enqueue(code_mod.id, &message)?;
                code_mod.queue.push(queued);
            }
            code_mod.draft.clear();
            self.composer.clear();
            self.celebrate();
        }
        Ok(())
    }
}
impl Drop for Engine {
    fn drop(&mut self) {
        self.workers.clear();
        if let Some(root) = self.setup_root.take() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

#[cfg(test)]
mod tests;
