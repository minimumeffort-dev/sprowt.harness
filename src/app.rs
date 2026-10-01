use std::{
    collections::{BTreeMap, BTreeSet},
    io,
    path::PathBuf,
    time::{Duration, Instant},
};

use ratatui::{
    DefaultTerminal,
    crossterm::event::{self, Event, KeyCode, KeyEvent, KeyEventKind, KeyModifiers},
};
use ratatui_textarea::TextArea;
use rusqlite::Result;
use tachyonfx::Effect;

use crate::{
    git_mod::{self, GitMod, Job},
    plan::Role,
    router::Router,
    sprout,
    store::{CodeMod, Store, source_id},
    tools::{Context, Dispatcher, Request},
    ui,
    worker::{Status, Worker},
    workspace::{self, Review},
};

#[derive(Clone, Copy)]
pub enum View {
    Chat,
    Mods(usize),
    DeleteMod(usize),
    CloseMod(usize),
    NewMod,
    Queue(usize),
    EditQueue(usize),
    Review(u16),
    Publish,
    ProjectSetup(bool, u16),
    Repository(bool),
    ConfirmRepository(bool),
}

pub struct App {
    pub project: PathBuf,
    pub input: TextArea<'static>,
    pub mods: Vec<CodeMod>,
    pub view: View,
    pub history_offset: u16,
    pub page_size: u16,
    pub plan_details: bool,
    pub focus_plan: bool,
    pub review: Option<Review>,
    publish_after_review: bool,
    pub queue_selection: BTreeSet<i64>,
    pub show_closed: bool,
    pub setup_files: Vec<String>,
    setup_root: Option<PathBuf>,
    workers: BTreeMap<i64, Worker>,
    auto_plans: BTreeSet<i64>,
    auto_runs: BTreeSet<i64>,
    git_jobs: BTreeMap<i64, Job>,
    git_states: BTreeMap<i64, GitMod>,
    router: Option<Router>,
    notice: Option<String>,
    store: Store,
    project_id: i64,
    active: Option<usize>,
    welcome: Effect,
    motion: bool,
    happy_since: Option<Instant>,
    quit: bool,
}

impl App {
    pub fn new(project: PathBuf, motion: bool) -> io::Result<Self> {
        let project = project.canonicalize()?;
        let project = git_mod::project_root(&project).unwrap_or(project);
        Self::load(project, motion, Store::local()?).map_err(io::Error::other)
    }

    fn load(project: PathBuf, motion: bool, store: Store) -> Result<Self> {
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
            project,
            input: ui::input(),
            mods: state.mods,
            view: if active.is_some() {
                View::Chat
            } else {
                View::NewMod
            },
            history_offset: 0,
            page_size: 1,
            plan_details: false,
            focus_plan: false,
            review: None,
            publish_after_review: false,
            queue_selection: BTreeSet::new(),
            show_closed: false,
            setup_files: Vec::new(),
            setup_root: None,
            workers: BTreeMap::new(),
            auto_plans: BTreeSet::new(),
            auto_runs: BTreeSet::new(),
            git_jobs: BTreeMap::new(),
            git_states,
            router: None,
            notice: None,
            store,
            project_id: state.id,
            active,
            welcome: ui::welcome_effect(motion),
            motion,
            happy_since: None,
            quit: false,
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

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let started = Instant::now();
        let mut last_frame = started;
        let mut companion = (None, sprout::Mood::Idle);
        let mut mood_since = started;
        while !self.quit {
            self.poll_workers().map_err(io::Error::other)?;
            let now = Instant::now();
            let elapsed = now.duration_since(last_frame);
            last_frame = now;
            let animation_time = self.motion.then_some(now.duration_since(started));
            let next = (self.current_mod().map(|m| m.id), self.companion_mood());
            if next != companion {
                companion = next;
                mood_since = now;
            }
            let (pose, next_pose) = sprout::animation(
                now.duration_since(mood_since),
                companion.1,
                self.happy_since.map(|since| now.duration_since(since)),
            );

            terminal.draw(|frame| {
                let header = ui::draw(
                    frame,
                    self,
                    if self.motion {
                        pose
                    } else {
                        sprout::Pose::IDLE
                    },
                    animation_time,
                );
                self.welcome
                    .process(elapsed.into(), frame.buffer_mut(), header);
            })?;

            if self.welcome.running()
                || self.motion
                || !self.workers.is_empty()
                || !self.git_jobs.is_empty()
            {
                let timeout = if self.welcome.running() {
                    Duration::from_millis(33)
                } else {
                    if self.workers.is_empty() && self.git_jobs.is_empty() {
                        next_pose
                    } else {
                        next_pose.min(Duration::from_millis(50))
                    }
                };
                if !event::poll(timeout)? {
                    continue;
                }
            }
            self.handle(event::read()?).map_err(io::Error::other)?;
        }
        Ok(())
    }

    fn handle(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Paste(text) => match self.view {
                View::Chat if self.read_only() => {}
                View::Chat | View::EditQueue(_) => {
                    self.input
                        .insert_str(text.replace("\r\n", "\n").replace('\r', "\n"));
                }
                View::NewMod | View::Repository(_) => {
                    self.input
                        .insert_str(text.replace("\r\n", "\n").replace('\r', "\n"));
                }
                View::Mods(_)
                | View::DeleteMod(_)
                | View::CloseMod(_)
                | View::Queue(_)
                | View::Review(_)
                | View::Publish
                | View::ProjectSetup(_, _)
                | View::ConfirmRepository(_) => {}
            },
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                if ctrl && key.code == KeyCode::Char('c') {
                    self.quit = true;
                    return Ok(());
                }
                match self.view {
                    View::Chat => self.chat_key(key)?,
                    View::Mods(index) => self.picker_key(key, index)?,
                    View::DeleteMod(index) => match key.code {
                        KeyCode::Esc => {
                            self.view = View::Mods(
                                self.picker_indices()
                                    .iter()
                                    .position(|i| *i == index)
                                    .unwrap_or(0),
                            )
                        }
                        KeyCode::Enter
                            if key.modifiers.is_empty() && key.kind == KeyEventKind::Press =>
                        {
                            self.delete_mod(index)?;
                        }
                        _ => {}
                    },
                    View::CloseMod(index) => match key.code {
                        KeyCode::Esc => self.view = View::Mods(0),
                        KeyCode::Enter
                            if key.modifiers.is_empty() && key.kind == KeyEventKind::Press =>
                        {
                            self.finish_mod(index, false, false)?
                        }
                        _ => {}
                    },
                    View::Queue(index) => self.queue_key(key, index)?,
                    View::EditQueue(index) => self.edit_key(key, index)?,
                    View::Review(scroll) => match key.code {
                        KeyCode::Esc => {
                            self.view = View::Chat;
                            self.review = None;
                            self.publish_after_review = false;
                        }
                        KeyCode::Down => self.view = View::Review(scroll.saturating_add(1)),
                        KeyCode::Up => self.view = View::Review(scroll.saturating_sub(1)),
                        KeyCode::PageDown => {
                            self.view = View::Review(scroll.saturating_add(self.page_size))
                        }
                        KeyCode::PageUp => {
                            self.view = View::Review(scroll.saturating_sub(self.page_size))
                        }
                        KeyCode::Char('p') if key.modifiers.is_empty() && self.can_publish() => {
                            self.open_publication()?
                        }
                        _ => {}
                    },
                    View::Publish => match key.code {
                        KeyCode::Esc => self.view = View::Review(0),
                        KeyCode::Enter
                            if key.modifiers.is_empty()
                                && key.kind == KeyEventKind::Press
                                && self.can_publish() =>
                        {
                            self.start_publication(self.active.unwrap());
                        }
                        _ => {}
                    },
                    View::ProjectSetup(saved, scroll) => match key.code {
                        KeyCode::Esc => {
                            if let Some(root) = self.setup_root.take() {
                                let _ = std::fs::remove_dir_all(root);
                            }
                            let closing = self
                                .current_mod()
                                .and_then(|m| m.execution.as_ref())
                                .is_some_and(|e| e.workspace.join("close-request").exists());
                            if closing {
                                if let Some(e) =
                                    self.current_mod().and_then(|m| m.execution.as_ref())
                                {
                                    let _ = std::fs::remove_file(e.workspace.join("close-request"));
                                }
                                self.view = View::Chat;
                            } else {
                                self.view = if saved { View::Review(0) } else { View::NewMod };
                            }
                        }
                        KeyCode::Up => {
                            self.view = View::ProjectSetup(saved, scroll.saturating_sub(1))
                        }
                        KeyCode::Down => {
                            self.view = View::ProjectSetup(saved, scroll.saturating_add(1))
                        }
                        KeyCode::PageUp => {
                            self.view = View::ProjectSetup(saved, scroll.saturating_sub(10))
                        }
                        KeyCode::PageDown => {
                            self.view = View::ProjectSetup(saved, scroll.saturating_add(10))
                        }
                        KeyCode::Enter
                            if key.modifiers.is_empty() && key.kind == KeyEventKind::Press =>
                        {
                            if saved {
                                self.adopt_mod()?;
                            } else {
                                self.create_mod()?;
                            }
                        }
                        _ => {}
                    },
                    View::Repository(create) => match key.code {
                        KeyCode::Esc => {
                            self.view = View::Review(0);
                            self.restore_input();
                        }
                        KeyCode::Tab => self.view = View::Repository(!create),
                        KeyCode::Enter if key.modifiers.is_empty() => {
                            if git_mod::valid_slug(self.input.lines().join("\n").trim()) {
                                self.view = View::ConfirmRepository(create);
                                self.notice = None;
                            } else {
                                self.notice = Some("Use owner/repository.".into());
                            }
                        }
                        _ => {
                            self.input.input(key);
                        }
                    },
                    View::ConfirmRepository(create) => match key.code {
                        KeyCode::Esc => self.view = View::Repository(create),
                        KeyCode::Enter
                            if key.modifiers.is_empty() && key.kind == KeyEventKind::Press =>
                        {
                            self.start_repository(git_mod::RepositoryRequest {
                                slug: self.input.lines().join("\n").trim().to_owned(),
                                create,
                            });
                        }
                        _ => {}
                    },
                    View::NewMod => match key.code {
                        KeyCode::Char('p') if ctrl => self.view = View::Mods(0),
                        KeyCode::Esc => {
                            if self.active.is_some() {
                                self.view = View::Chat;
                                self.restore_input();
                            } else {
                                self.quit = true;
                            }
                        }
                        KeyCode::Enter => self.create_mod()?,
                        KeyCode::Char('j') if ctrl => self.input.insert_newline(),
                        _ => {
                            self.input.input(key);
                        }
                    },
                }
            }
            _ => {}
        }
        if matches!(self.view, View::Chat) && !self.read_only() {
            self.save_draft()?;
        }
        Ok(())
    }

    fn chat_key(&mut self, key: KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.quit = true,
            KeyCode::Char('p') if ctrl => {
                self.show_closed = self.current_mod().is_some_and(|m| m.closed);
                self.view = View::Mods(
                    self.picker_indices()
                        .iter()
                        .position(|i| Some(*i) == self.active)
                        .unwrap_or(0),
                );
            }
            KeyCode::Char('q')
                if ctrl
                    && self
                        .current_mod()
                        .is_some_and(|code_mod| !code_mod.queue.is_empty()) =>
            {
                self.view = View::Queue(0)
            }
            KeyCode::Char('q') if ctrl => {}
            KeyCode::Char('r') if ctrl => self.toggle_worker()?,
            KeyCode::Char('d') if ctrl => self.open_review()?,
            KeyCode::Char('s') if ctrl && self.version_ready() => {
                self.review = None;
                self.publish_after_review = true;
                self.open_review()?;
                if self.review.is_some() && self.can_publish() {
                    self.publish_after_review = false;
                    self.open_publication()?;
                }
            }
            KeyCode::Char('o') if ctrl => {
                if self
                    .current_mod()
                    .and_then(|code_mod| code_mod.planning.as_ref())
                    .is_some_and(|planning| planning.status == "ready" && planning.plan.is_some())
                {
                    self.plan_details = !self.plan_details;
                    self.focus_plan = true;
                }
            }
            KeyCode::Char('j') if ctrl => self.input.insert_newline(),
            KeyCode::Enter if key.modifiers.is_empty() => self.submit()?,
            KeyCode::PageUp => {
                self.history_offset = self.history_offset.saturating_add(self.page_size)
            }
            KeyCode::PageDown => {
                self.history_offset = self.history_offset.saturating_sub(self.page_size)
            }
            _ => {
                if !self.read_only() {
                    self.input.input(key);
                }
            }
        }
        Ok(())
    }

    fn picker_key(&mut self, key: KeyEvent, index: usize) -> Result<()> {
        let indices = self.picker_indices();
        let selected = indices.get(index).copied();
        let count = indices.len();
        match key.code {
            KeyCode::Up => self.view = View::Mods(index.saturating_sub(1)),
            KeyCode::Down => self.view = View::Mods((index + 1).min(count)),
            KeyCode::Tab => {
                self.show_closed = !self.show_closed;
                self.view = View::Mods(0);
            }
            KeyCode::Esc => {
                self.view = if self.active.is_some() {
                    View::Chat
                } else {
                    View::NewMod
                };
                self.restore_input();
            }
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.view = View::Chat
            }
            KeyCode::Char('d') if key.modifiers.is_empty() && selected.is_some() => {
                self.view = View::DeleteMod(selected.unwrap());
            }
            KeyCode::Char('c')
                if key.modifiers.is_empty() && !self.show_closed && selected.is_some() =>
            {
                self.view = View::CloseMod(selected.unwrap());
            }
            KeyCode::Char('r')
                if key.modifiers.is_empty() && selected.is_some_and(|i| self.mods[i].closed) =>
            {
                self.reopen_mod(selected.unwrap());
            }
            KeyCode::Enter if index == count => {
                self.view = View::NewMod;
                self.input = ui::name_input();
            }
            KeyCode::Enter => {
                let index = selected.unwrap();
                self.store
                    .select_mod(self.project_id, self.mods[index].id)?;
                self.active = Some(index);
                self.notice = None;
                self.history_offset = 0;
                self.plan_details = false;
                self.focus_plan = false;
                self.view = View::Chat;
                self.restore_input();
            }
            _ => {}
        }
        Ok(())
    }

    fn restore_input(&mut self) {
        self.input = if matches!(self.view, View::NewMod) {
            ui::name_input()
        } else {
            ui::input()
        };
        if matches!(self.view, View::Chat | View::Queue(_) | View::Review(_))
            && let Some(code_mod) = self.current_mod()
        {
            let draft = code_mod.draft.clone();
            self.input.insert_str(draft);
        }
    }

    fn queue_key(&mut self, key: KeyEvent, index: usize) -> Result<()> {
        if key.code == KeyCode::Esc
            || (key.code == KeyCode::Char('q') && key.modifiers.contains(KeyModifiers::CONTROL))
        {
            self.view = View::Chat;
            self.queue_selection.clear();
            return Ok(());
        }
        let Some(active) = self.active else {
            return Ok(());
        };
        let mod_id = self.mods[active].id;
        let protected = if let Some(message) = self.mods[active].queue.get(index) {
            self.store
                .is_pending(mod_id, &source_id(message.id, false))?
        } else {
            false
        };
        if key.code == KeyCode::Char('s') && key.modifiers.is_empty() {
            let ids = if self.queue_selection.is_empty() {
                self.mods[active]
                    .queue
                    .get(index)
                    .map(|message| vec![message.id])
                    .unwrap_or_default()
            } else {
                self.queue_selection.iter().copied().collect()
            };
            if ids.is_empty() {
                return Ok(());
            }
            for id in &ids {
                if self.store.is_pending(mod_id, &source_id(*id, false))? {
                    return Ok(());
                }
            }
            let messages = self.store.request_steering(mod_id, &ids)?;
            self.mods[active]
                .queue
                .retain(|message| !ids.contains(&message.id));
            self.mods[active].steering.extend(messages);
            self.queue_selection.clear();
            self.clamp_queue(index);
            return Ok(());
        }
        let code_mod = &mut self.mods[active];
        let len = code_mod.queue.len();
        if len == 0 {
            self.view = View::Chat;
            self.queue_selection.clear();
            return Ok(());
        }
        match key.code {
            KeyCode::Up => self.view = View::Queue(index.saturating_sub(1)),
            KeyCode::Down => self.view = View::Queue((index + 1).min(len - 1)),
            KeyCode::Char(' ') if key.modifiers.is_empty() => {
                let id = code_mod.queue[index].id;
                if !self.queue_selection.remove(&id) {
                    self.queue_selection.insert(id);
                }
            }
            KeyCode::Enter if key.modifiers.is_empty() && !protected => {
                self.input = ui::edit_input();
                self.input.insert_str(&code_mod.queue[index].body);
                self.view = View::EditQueue(index);
            }
            KeyCode::Char('d') | KeyCode::Delete | KeyCode::Backspace
                if key.modifiers.is_empty() && !protected =>
            {
                self.queue_selection.remove(&code_mod.queue[index].id);
                self.store
                    .remove_queued(code_mod.id, code_mod.queue[index].id)?;
                code_mod.queue.remove(index);
                self.clamp_queue(index);
            }
            KeyCode::Char('k') | KeyCode::Char('j') if key.modifiers.is_empty() => {
                let target = if key.code == KeyCode::Char('k') {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(len - 1)
                };
                if target != index {
                    self.store.swap_queued(
                        code_mod.id,
                        code_mod.queue[index].id,
                        code_mod.queue[target].id,
                    )?;
                    code_mod.queue.swap(index, target);
                    self.view = View::Queue(target);
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn edit_key(&mut self, key: KeyEvent, index: usize) -> Result<()> {
        match key.code {
            KeyCode::Esc => {
                self.view = View::Queue(index);
                self.restore_input();
            }
            KeyCode::Enter if key.modifiers.is_empty() => {
                let body = self.input.lines().join("\n");
                if !body.trim().is_empty()
                    && let Some(active) = self.active
                {
                    let code_mod = &mut self.mods[active];
                    self.store
                        .edit_queued(code_mod.id, code_mod.queue[index].id, &body)?;
                    code_mod.queue[index].body = body;
                    self.view = View::Queue(index);
                    self.restore_input();
                    self.celebrate();
                }
            }
            KeyCode::Char('j') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.input.insert_newline()
            }
            _ => {
                self.input.input(key);
            }
        }
        Ok(())
    }

    fn create_mod(&mut self) -> Result<()> {
        let name = self.input.lines().join("\n");
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        if !git_mod::has_commit(&self.project) && self.setup_root.is_none() {
            let root = self
                .store
                .workspace_path(0)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            match workspace::create(&self.project, &root) {
                Ok(()) => {
                    self.setup_files = workspace::source_state(&root.join("before"))
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
                        .into_iter()
                        .map(|(path, _, _)| path.display().to_string())
                        .collect();
                    self.setup_root = Some(root);
                    self.view = View::ProjectSetup(false, 0);
                    self.notice = None;
                }
                Err(error) => self.notice = Some(error.to_string()),
            }
            return Ok(());
        }
        let mut code_mod = self.store.create_mod(self.project_id, name)?;
        {
            let root = self
                .store
                .workspace_path(code_mod.id)
                .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
            let initialize = if let Some(preview) = self.setup_root.take() {
                std::fs::rename(preview, &root)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                std::fs::write(root.join("project-setup"), b"new")
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                true
            } else {
                false
            };
            self.store.save_git_root(code_mod.id, &root)?;
            code_mod.git_root = Some(root.clone());
            let context = Context::harness(&self.project, &code_mod);
            self.git_jobs.insert(
                code_mod.id,
                Job::start(
                    if initialize {
                        "setting up Git"
                    } else {
                        "checking project branch"
                    },
                    move |cancelled| {
                        Dispatcher::new(&context, None, cancelled).execute(
                            if initialize {
                                Request::InitializeProject
                            } else {
                                Request::CreateWorktree
                            },
                            |_| {},
                        )?;
                        Ok(git_mod::Result::Prepared)
                    },
                ),
            );
        }
        self.mods.push(code_mod);
        self.active = Some(self.mods.len() - 1);
        self.notice = None;
        self.history_offset = 0;
        self.plan_details = false;
        self.focus_plan = false;
        self.view = View::Chat;
        self.restore_input();
        self.celebrate();
        Ok(())
    }

    fn adopt_mod(&mut self) -> Result<()> {
        let index = self.active.unwrap();
        let code_mod = &mut self.mods[index];
        let root = code_mod.execution.as_ref().unwrap().workspace.clone();
        std::fs::write(root.join("project-setup"), b"saved")
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        self.store.save_git_root(code_mod.id, &root)?;
        code_mod.git_root = Some(root);
        let id = code_mod.id;
        let context = Context::harness(&self.project, code_mod);
        self.git_jobs.insert(
            id,
            Job::start("adopting saved codemod", move |cancelled| {
                Dispatcher::new(&context, None, cancelled)
                    .execute(Request::AdoptSnapshot, |_| {})?;
                Ok(git_mod::Result::Adopted)
            }),
        );
        self.view = View::Chat;
        self.notice = None;
        self.restore_input();
        Ok(())
    }

    fn open_publication(&mut self) -> Result<()> {
        if !self.can_publish() {
            return Ok(());
        }
        let code_mod = &self.mods[self.active.unwrap()];
        if let Some(root) = &code_mod.git_root {
            if self
                .git_states
                .get(&code_mod.id)
                .is_some_and(|state| state.closing)
            {
                git_mod::cancel_closing(root)
                    .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                self.git_states
                    .insert(code_mod.id, git_mod::load(root).unwrap());
            }
            if let Some(request) = git_mod::repository_pending(root) {
                self.open_repository();
                self.input.insert_str(&request.slug);
                self.view = View::Repository(request.create);
            } else if !git_mod::has_origin(&self.project) {
                self.open_repository();
            } else {
                self.view = View::Publish;
            }
        } else {
            self.setup_files = workspace::source_state(
                &code_mod
                    .execution
                    .as_ref()
                    .unwrap()
                    .workspace
                    .join("before"),
            )
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
            .into_iter()
            .map(|(path, _, _)| path.display().to_string())
            .collect();
            self.view = View::ProjectSetup(true, 0);
        }
        Ok(())
    }

    fn open_repository(&mut self) {
        self.input = ui::repository_input();
        self.view = View::Repository(false);
        self.notice = None;
    }

    fn start_repository(&mut self, request: git_mod::RepositoryRequest) {
        let code_mod = self.current_mod().unwrap();
        let id = code_mod.id;
        let context = Context::harness(&self.project, code_mod);
        self.git_jobs.insert(
            id,
            Job::start("connecting GitHub repository", move |cancelled| {
                Dispatcher::new(&context, None, cancelled)
                    .execute(Request::ConnectRepository(request), |_| {})?;
                Ok(git_mod::Result::RepositoryConnected)
            }),
        );
        self.view = View::Chat;
        self.restore_input();
        self.notice = None;
    }

    fn delete_mod(&mut self, index: usize) -> Result<()> {
        self.finish_mod(index, true, true)
    }

    fn finish_mod(&mut self, index: usize, discard: bool, delete: bool) -> Result<()> {
        if !delete && self.mods[index].git_root.is_none() && self.mods[index].execution.is_some() {
            self.active = Some(index);
            let root = &self.mods[index].execution.as_ref().unwrap().workspace;
            std::fs::write(root.join("close-request"), b"close")
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
            self.setup_files = workspace::source_state(&root.join("before"))
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
                .into_iter()
                .map(|(path, _, _)| path.display().to_string())
                .collect();
            self.view = View::ProjectSetup(true, 0);
            return Ok(());
        }
        if let Some(root) = self.mods[index].git_root.clone() {
            let id = self.mods[index].id;
            if self.git_jobs.contains_key(&id) {
                return Ok(());
            }
            let context = Context::harness(&self.project, &self.mods[index]);
            let keys = self
                .workers
                .iter()
                .filter(|(_, worker)| worker.mod_id == id)
                .map(|(id, _)| *id)
                .collect::<Vec<_>>();
            let workers = keys
                .into_iter()
                .filter_map(|key| self.workers.remove(&key))
                .collect::<Vec<_>>();
            self.auto_plans.remove(&id);
            self.auto_runs.remove(&id);
            self.git_jobs.insert(
                id,
                Job::start(
                    if delete {
                        "deleting mod"
                    } else {
                        "closing mod"
                    },
                    move |cancelled| {
                        drop(workers);
                        let mut tools = Dispatcher::new(&context, None, cancelled);
                        if !root.join("git-mod.json").exists() {
                            if !delete {
                                return Err(io::Error::other(
                                    "Finish Git setup before closing. Saved files are retained.",
                                ));
                            }
                            if root.join("checkout").exists()
                                || (git_mod::setup_pending(&root).is_none()
                                    && ["work", "vm.json"]
                                        .iter()
                                        .any(|name| root.join(name).exists()))
                            {
                                return Err(io::Error::other(
                                    "Missing worktree metadata; files are retained.",
                                ));
                            }
                            tools.execute(Request::CleanupMod, |_| {})?;
                            if delete {
                                git_mod::remove_files(&root, cancelled)?;
                            }
                            return Ok(if delete {
                                git_mod::Result::Removed
                            } else {
                                git_mod::Result::Closed
                            });
                        }
                        git_mod::mark_closing(&root, discard, delete)?;
                        if git_mod::load(&root)?.phase == "preparing"
                            && git_mod::checkout(&root).exists()
                        {
                            tools.execute(Request::CreateWorktree, |_| {})?;
                        }
                        if !discard && root.join("vm.json").exists() {
                            let mut vm =
                                crate::sandbox::Sandbox::prepare(&root, cancelled, |_| {})?;
                            vm.export(cancelled)?;
                        }
                        tools.execute(
                            if delete {
                                Request::CleanupMod
                            } else {
                                Request::CloseMod
                            },
                            |_| {},
                        )?;
                        if delete {
                            git_mod::remove_files(&root, cancelled)?;
                        }
                        Ok(if delete {
                            git_mod::Result::Removed
                        } else {
                            git_mod::Result::Closed
                        })
                    },
                ),
            );
            self.view = View::Chat;
            self.notice = None;
            self.restore_input();
            return Ok(());
        }
        let mod_id = self.mods[index].id;
        let previous = self.current_mod().map(|code_mod| code_mod.id);
        let workspace = self.mods[index]
            .execution
            .as_ref()
            .map(|execution| execution.workspace.clone());
        self.auto_plans.remove(&mod_id);
        self.auto_runs.remove(&mod_id);
        self.workers.retain(|_, worker| worker.mod_id != mod_id);
        if workspace.is_some()
            && let Err(error) = Dispatcher::new(
                &Context::harness(&self.project, &self.mods[index]),
                None,
                &std::sync::atomic::AtomicBool::new(false),
            )
            .execute(Request::CleanupMod, |_| {})
        {
            self.notice = Some(format!(
                "Could not delete the VM: {error}. The mod is retained."
            ));
            self.view = View::Chat;
            self.restore_input();
            return Ok(());
        }
        let selected = if delete {
            self.store.delete_mod(self.project_id, mod_id)?
        } else {
            self.store.close_mod(self.project_id, mod_id)?
        };
        if delete {
            self.mods.remove(index);
        } else {
            self.mods[index].closed = true;
        }
        let cleanup_error = workspace.and_then(|path| std::fs::remove_dir_all(path).err());
        self.active = self
            .mods
            .iter()
            .position(|code_mod| Some(code_mod.id) == selected);
        if previous != selected {
            self.history_offset = 0;
            self.plan_details = false;
            self.focus_plan = false;
            self.notice = None;
        }
        if let Some(error) = cleanup_error {
            self.notice = Some(format!(
                "Mod {}; working folder cleanup failed: {error}",
                if delete { "deleted" } else { "closed" }
            ));
        }
        self.queue_selection.clear();
        self.view = if self.active.is_some() {
            View::Chat
        } else {
            View::NewMod
        };
        self.restore_input();
        Ok(())
    }

    pub fn has_worker(&self, mod_id: i64) -> bool {
        self.workers.values().any(|worker| worker.mod_id == mod_id)
    }

    pub fn picker_indices(&self) -> Vec<usize> {
        self.mods
            .iter()
            .enumerate()
            .filter(|(_, m)| m.closed == self.show_closed)
            .map(|(i, _)| i)
            .collect()
    }

    pub fn read_only(&self) -> bool {
        self.current_mod().is_some_and(|m| m.closed)
    }

    fn reopen_mod(&mut self, index: usize) {
        if self.git_jobs.contains_key(&self.mods[index].id) {
            return;
        }
        let context = Context::harness(&self.project, &self.mods[index]);
        if self.mods[index].git_root.is_none() {
            self.notice =
                Some("This older codemod has no saved branch. Start a new codemod.".into());
            return;
        }
        if let Err(error) = self.store.select_mod(self.project_id, self.mods[index].id) {
            self.notice = Some(error.to_string());
            return;
        }
        self.active = Some(index);
        self.git_jobs.insert(
            self.mods[index].id,
            Job::start("reopening codemod", move |cancelled| {
                Dispatcher::new(&context, None, cancelled).execute(Request::ReopenMod, |_| {})?;
                Ok(git_mod::Result::Reopened)
            }),
        );
        self.view = View::Chat;
        self.restore_input();
    }

    fn start_continuation(&mut self, index: usize) {
        let id = self.mods[index].id;
        self.workers.retain(|_, worker| worker.mod_id != id);
        if self.mods[index].git_root.is_none() {
            if let Err(error) = self.finish_edit(index) {
                self.notice = Some(error.to_string());
            }
            return;
        }
        let context = Context::harness(&self.project, &self.mods[index]);
        self.git_jobs.insert(
            id,
            Job::start("preparing edits", move |cancelled| {
                Dispatcher::new(&context, None, cancelled)
                    .execute(Request::PrepareEdits, |_| {})?;
                Ok(git_mod::Result::Continued)
            }),
        );
    }

    fn finish_edit(&mut self, index: usize) -> Result<()> {
        let id = self.mods[index].id;
        if self.mods[index]
            .execution
            .as_ref()
            .is_some_and(|e| e.status == "planning")
        {
            self.auto_plans.insert(id);
            return Ok(());
        }
        let Some(message) = self.mods[index].queue.first().cloned() else {
            return Ok(());
        };
        self.store.follow_up(id, &message.body, Some(message.id))?;
        let state = self.store.load_project(&self.project)?;
        self.mods[index] = state.mods.into_iter().find(|m| m.id == id).unwrap();
        self.auto_plans.insert(id);
        if self.current_mod().is_some_and(|m| m.id == id) {
            self.review = None;
            self.plan_details = false;
        }
        Ok(())
    }

    pub fn git_state(&self) -> Option<&GitMod> {
        self.current_mod().and_then(|m| self.git_states.get(&m.id))
    }

    pub fn git_activity(&self) -> Option<&str> {
        self.current_mod()
            .and_then(|m| self.git_jobs.get(&m.id))
            .map(|job| job.label)
    }

    pub fn git_retry_pending(&self) -> bool {
        self.current_mod().is_some_and(|m| {
            m.git_root.as_ref().is_some_and(|root| {
                git_mod::setup_pending(root).is_some()
                    || git_mod::repository_pending(root).is_some()
            }) || m.git_root.is_some()
                && self.git_states.get(&m.id).is_none_or(|state| {
                    state.removing
                        || (state.closing && !m.closed)
                        || state.continuing
                        || state.publishing
                        || !["ready", "published", "cleaned", "closed", "pruned"]
                            .contains(&state.phase.as_str())
                })
        })
    }

    pub fn published(&self) -> bool {
        self.git_state().is_some_and(GitMod::published)
    }

    fn source_project(&self, index: usize) -> PathBuf {
        self.mods[index]
            .git_root
            .as_ref()
            .map(|root| {
                if root.join("work").exists() {
                    root.join("work")
                } else {
                    git_mod::checkout(root)
                }
            })
            .or_else(|| {
                self.mods[index]
                    .execution
                    .as_ref()
                    .map(|e| e.workspace.join("work"))
            })
            .unwrap_or_else(|| self.project.clone())
    }

    pub fn current_worker(&self) -> Option<&Worker> {
        let mod_id = self.current_mod()?.id;
        let workers = || {
            self.workers
                .values()
                .filter(|worker| worker.mod_id == mod_id)
        };
        workers()
            .find(|worker| worker.role == Role::Planner && worker.enabled)
            .or_else(|| workers().find(|worker| worker.role == Role::Executor))
            .or_else(|| workers().next())
    }

    pub fn worker_error(&self) -> Option<&str> {
        self.current_worker()
            .and_then(|worker| worker.error.as_deref())
            .or(self.notice.as_deref())
    }

    fn companion_mood(&self) -> sprout::Mood {
        use sprout::Mood;

        let Some(code_mod) = self.current_mod().filter(|m| !m.closed) else {
            return Mood::Idle;
        };
        if self.git_activity().is_some() {
            return Mood::Working;
        }
        let workers = || self.workers.values().filter(|w| w.mod_id == code_mod.id);
        if workers().any(|w| w.role == Role::Planner && (w.enabled || w.busy())) {
            return Mood::Planning;
        }
        if workers().any(|w| w.enabled || w.busy()) {
            return Mood::Working;
        }
        if self.worker_error().is_some()
            || workers().any(|w| w.status == Status::Failed)
            || code_mod
                .planning
                .as_ref()
                .is_some_and(|p| p.status == "failed")
            || code_mod
                .execution
                .as_ref()
                .is_some_and(|e| e.status == "blocked")
        {
            return Mood::Blocked;
        }
        if code_mod
            .execution
            .as_ref()
            .is_some_and(|e| e.complete() && matches!(e.status.as_str(), "review" | "applied"))
        {
            return Mood::Finished;
        }
        Mood::Idle
    }

    fn toggle_worker(&mut self) -> Result<()> {
        let Some(active) = self.active else {
            return Ok(());
        };
        let id = self.mods[active].id;
        if self.git_jobs.contains_key(&id) {
            return Ok(());
        }
        if self.mods[active].git_root.is_some() {
            let root = self.mods[active].git_root.clone().unwrap();
            if let Some(saved) = git_mod::setup_pending(&root) {
                let context = Context::harness(&self.project, &self.mods[active]);
                self.git_jobs.insert(
                    id,
                    Job::start("setting up Git", move |cancelled| {
                        Dispatcher::new(&context, None, cancelled).execute(
                            if saved {
                                Request::AdoptSnapshot
                            } else {
                                Request::InitializeProject
                            },
                            |_| {},
                        )?;
                        Ok(if saved {
                            git_mod::Result::Adopted
                        } else {
                            git_mod::Result::Prepared
                        })
                    }),
                );
                return Ok(());
            }
            if let Some(request) = git_mod::repository_pending(&root) {
                self.start_repository(request);
                return Ok(());
            }
            match self.git_states.get(&id) {
                Some(state) if state.removing => return self.delete_mod(active),
                Some(state) if state.continuing => {
                    self.start_continuation(active);
                    return Ok(());
                }
                Some(state) if state.closing && !self.mods[active].closed => {
                    return self.finish_mod(active, state.discarding, false);
                }
                Some(_) if self.mods[active].closed => {
                    self.reopen_mod(active);
                    return Ok(());
                }
                Some(state) if state.published() && self.mods[active].queue.is_empty() => {
                    return Ok(());
                }
                state if state.is_none_or(|state| state.phase == "preparing") => {
                    let context = Context::harness(&self.project, &self.mods[active]);
                    self.git_jobs.insert(
                        id,
                        Job::start("creating worktree", move |cancelled| {
                            Dispatcher::new(&context, None, cancelled)
                                .execute(Request::CreateWorktree, |_| {})?;
                            Ok(git_mod::Result::Prepared)
                        }),
                    );
                    return Ok(());
                }
                Some(state)
                    if state.publishing
                        || !["ready", "published", "cleaned", "closed", "pruned"]
                            .contains(&state.phase.as_str()) =>
                {
                    self.start_publication(active);
                    return Ok(());
                }
                Some(state) if state.phase == "cleaned" => return Ok(()),
                _ => {}
            }
        }
        if self.mods[active].closed {
            self.reopen_mod(active);
            return Ok(());
        }
        if self.mods[active].execution.is_none()
            && self.mods[active].git_root.as_ref().is_some_and(|root| {
                !root.join("work").exists() && !root.join("snapshot-ready").exists()
            })
            && self.mods[active]
                .planning
                .as_ref()
                .is_some_and(|plan| matches!(plan.status.as_str(), "failed" | "paused"))
            && !self.execution_busy()
        {
            self.workers.retain(|_, worker| worker.mod_id != id);
            let context = Context::harness(&self.project, &self.mods[active]);
            self.git_jobs.insert(
                id,
                Job::start("refreshing starting source", move |cancelled| {
                    Dispatcher::new(&context, None, cancelled)
                        .execute(Request::RefreshWorktree, |_| {})?;
                    Ok(git_mod::Result::Refreshed)
                }),
            );
            self.notice = None;
            return Ok(());
        }
        let role = if self.mods[active]
            .planning
            .as_ref()
            .is_some_and(|p| p.status != "ready")
        {
            Role::Planner
        } else {
            Role::Executor
        };
        let mod_id = self.mods[active].id;
        if role == Role::Executor
            && self.mods[active]
                .planning
                .as_ref()
                .is_some_and(|p| p.plan.is_some())
        {
            if self.current_worker().is_some_and(|worker| {
                worker.status == Status::Stopping
                    || (!worker.enabled && worker.status == Status::Checking)
            }) {
                self.notice =
                    Some("The current command is stopping; retry when it finishes.".into());
                return Ok(());
            }
            if self.mods[active].execution.is_none()
                || self.mods[active]
                    .execution
                    .as_ref()
                    .is_some_and(|e| e.status == "planning")
            {
                return self.start_execution(active);
            }
            if self.mods[active]
                .execution
                .as_ref()
                .is_some_and(|e| matches!(e.status.as_str(), "review" | "applied" | "blocked"))
                && !self.mods[active].queue.is_empty()
                && !self.execution_busy()
            {
                self.start_continuation(active);
                return Ok(());
            }
            if self.mods[active]
                .execution
                .as_ref()
                .is_some_and(|execution| {
                    execution.backend == "local" && execution.status != "applied"
                })
            {
                self.workers
                    .retain(|_, worker| worker.mod_id != mod_id || worker.role != Role::Executor);
                self.store.move_to_vm(mod_id)?;
                self.mods[active].execution = self.store.execution(mod_id)?;
            }
            if self.current_worker().is_none_or(|worker| !worker.enabled) {
                self.store.retry_tasks(mod_id)?;
                self.mods[active].execution = self.store.execution(mod_id)?;
            }
        }
        if let Some(worker) = self
            .workers
            .values_mut()
            .find(|w| w.mod_id == mod_id && w.role == role)
            && worker.status != Status::Failed
        {
            if role == Role::Planner && worker.status == Status::Ready && !worker.enabled {
                self.store.retry_plan(mod_id)?;
                self.mods[active].planning = self.store.planning(mod_id)?;
            }
            worker.toggle();
            return Ok(());
        }
        self.start_worker(active, role)
    }

    fn start_execution(&mut self, index: usize) -> Result<()> {
        let id = self.mods[index].id;
        if self.mods[index]
            .execution
            .as_ref()
            .is_some_and(|e| e.status == "planning")
        {
            let root = self.mods[index]
                .execution
                .as_ref()
                .unwrap()
                .workspace
                .clone();
            let plan = self.mods[index]
                .planning
                .as_ref()
                .unwrap()
                .plan
                .as_ref()
                .unwrap();
            self.store.create_execution(id, &root, plan)?;
            self.mods[index].execution = self.store.execution(id)?;
            return self.start_worker(index, Role::Executor);
        }
        if self.mods[index].execution.is_none() {
            if let Some(root) = self.mods[index].git_root.clone() {
                let project = self.source_project(index);
                self.git_jobs.insert(
                    id,
                    Job::start("preparing source", move |_| {
                        if !root.join("snapshot-ready").exists() {
                            workspace::create(&project, &root)?;
                            std::fs::write(root.join("snapshot-ready"), b"ready")?;
                        }
                        Ok(git_mod::Result::Snapshot)
                    }),
                );
                return Ok(());
            }
            if let Err(error) = self.prepare_execution(index) {
                self.notice = Some(error.to_string());
                return Ok(());
            }
        }
        self.start_worker(index, Role::Executor)
    }

    fn ready_plans(&self) -> Vec<usize> {
        self.mods
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                !m.closed
                    && self.auto_runs.contains(&m.id)
                    && !self.git_jobs.contains_key(&m.id)
                    && m.planning.as_ref().is_some_and(|p| p.status == "ready")
                    && m.execution.as_ref().is_none_or(|e| e.status == "planning")
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn start_worker(&mut self, index: usize, role: Role) -> Result<()> {
        let project = self.source_project(index);
        let mod_id = self.mods[index].id;
        let record = if role == Role::Executor {
            self.store.worker(mod_id)?
        } else {
            self.store.worker_for(mod_id, role)?
        };
        self.workers.remove(&record.id);
        let routing = if role == Role::Planner {
            if record.pending.is_none()
                && self.mods[index]
                    .planning
                    .as_ref()
                    .is_some_and(|p| p.status == "failed" || p.status == "paused")
            {
                self.store.retry_plan(mod_id)?;
                self.mods[index].planning = self.store.planning(mod_id)?;
            }
            if self.router.is_none() {
                self.router = Router::start().ok();
            }
            self.router.as_ref().map(|router| {
                router.request(crate::router::context(
                    &project,
                    &self.mods[index].description,
                ))
            })
        } else {
            None
        };
        match Worker::start(&project, &self.mods[index], record, role, routing) {
            Ok(worker) => {
                self.workers.insert(worker.id, worker);
                if role == Role::Planner {
                    self.auto_runs.insert(mod_id);
                }
                self.notice = None;
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
        Ok(())
    }

    fn prepare_execution(&mut self, index: usize) -> io::Result<()> {
        let code_mod = &self.mods[index];
        let root = self.store.workspace_path(code_mod.id)?;
        workspace::create(&self.project, &root)?;
        let plan = code_mod.planning.as_ref().unwrap().plan.as_ref().unwrap();
        if let Err(error) = self.store.create_execution(code_mod.id, &root, plan) {
            let _ = std::fs::remove_dir_all(&root);
            return Err(io::Error::other(error));
        }
        self.workers
            .retain(|_, worker| worker.mod_id != code_mod.id || worker.role != Role::Executor);
        self.mods[index].execution = self
            .store
            .execution(code_mod.id)
            .map_err(io::Error::other)?;
        Ok(())
    }

    pub fn execution_busy(&self) -> bool {
        self.git_activity().is_some()
            || self.current_mod().is_some_and(|m| {
                self.workers
                    .values()
                    .any(|w| w.mod_id == m.id && (w.enabled || w.busy()))
            })
    }

    pub fn version_ready(&self) -> bool {
        !self.published()
            && !self.execution_busy()
            && self.current_mod().is_some_and(|m| {
                !m.closed
                    && m.queue.is_empty()
                    && m.steering.is_empty()
                    && m.execution.as_ref().is_some_and(|e| {
                        e.complete() && matches!(e.status.as_str(), "review" | "applied")
                    })
            })
    }

    pub fn can_publish(&self) -> bool {
        self.version_ready()
            && self
                .current_mod()
                .is_some_and(|m| !m.closed && m.queue.is_empty() && m.steering.is_empty())
            && self.current_mod().is_none_or(|m| {
                m.git_root
                    .as_ref()
                    .is_none_or(|root| git_mod::setup_pending(root).is_none())
            })
            && !self.execution_busy()
            && self.review.as_ref().is_some_and(|review| {
                review.count() > 0 || self.git_state().is_some_and(|s| s.pr.is_some())
            })
            && self
                .current_mod()
                .and_then(|m| m.execution.as_ref())
                .is_some_and(|execution| {
                    execution.complete()
                        && matches!(execution.status.as_str(), "review" | "applied")
                        && self.review.as_ref().is_some_and(|review| {
                            execution.fingerprint.as_deref() == Some(&review.fingerprint)
                        })
                })
    }

    fn open_review(&mut self) -> Result<()> {
        let Some(execution) = self.current_mod().and_then(|m| m.execution.as_ref()) else {
            return Ok(());
        };
        if self.execution_busy() {
            self.notice = Some("Pause the executor before reviewing its changes.".into());
            return Ok(());
        }
        if self.current_mod().is_some_and(|m| m.git_root.is_some()) {
            let id = self.current_mod().unwrap().id;
            let root = execution.workspace.clone();
            self.git_jobs.insert(
                id,
                Job::start("loading changes", move |_| {
                    workspace::review(&root).map(git_mod::Result::Reviewed)
                }),
            );
            return Ok(());
        }
        match workspace::review(&execution.workspace) {
            Ok(review) => {
                if execution.status == "review"
                    && execution.fingerprint.as_deref() != Some(&review.fingerprint)
                {
                    self.notice = Some(
                        "Working files changed since verification. Esc, then Ctrl+R to recheck."
                            .into(),
                    );
                    let index = self.active.unwrap();
                    self.store
                        .execution_status(self.mods[index].id, "blocked")?;
                    self.mods[index].execution.as_mut().unwrap().status = "blocked".into();
                } else {
                    self.notice = None;
                }
                self.review = Some(review);
                self.view = View::Review(0);
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
        Ok(())
    }

    fn start_publication(&mut self, index: usize) {
        let code_mod = &self.mods[index];
        let root = code_mod.git_root.clone().unwrap();
        let context = Context::harness(&self.project, code_mod);
        let id = code_mod.id;
        if let Err(error) = git_mod::mark_publishing(&root) {
            self.notice = Some(error.to_string());
            return;
        }
        let keys = self
            .workers
            .iter()
            .filter(|(_, worker)| worker.mod_id == id)
            .map(|(key, _)| *key)
            .collect::<Vec<_>>();
        let workers = keys
            .into_iter()
            .filter_map(|key| self.workers.remove(&key))
            .collect::<Vec<_>>();
        self.git_jobs.insert(
            id,
            Job::start("publishing PR", move |cancelled| {
                drop(workers);
                let mut tools = Dispatcher::new(&context, None, cancelled);
                tools.execute(Request::PublishPr { draft: false }, |_| {})?;
                Ok(git_mod::Result::Published)
            }),
        );
        self.review = None;
        self.view = View::Chat;
        self.notice = None;
    }

    fn poll_workers(&mut self) -> Result<()> {
        self.poll_git_jobs()?;
        for mod_id in std::mem::take(&mut self.auto_plans) {
            if let Some(index) = self.mods.iter().position(|m| m.id == mod_id) {
                self.start_worker(index, Role::Planner)?;
            }
        }
        let focused = match self.view {
            View::Queue(index) | View::EditQueue(index) => self
                .current_mod()
                .and_then(|code_mod| code_mod.queue.get(index))
                .map(|message| message.id),
            _ => None,
        };
        let editing_mod = matches!(self.view, View::EditQueue(_))
            .then(|| self.current_mod().map(|code_mod| code_mod.id))
            .flatten();
        let deleting_mod = match self.view {
            View::DeleteMod(index) | View::CloseMod(index) => Some(self.mods[index].id),
            _ => None,
        };
        let reviewing_mod = matches!(
            self.view,
            View::Review(_)
                | View::Publish
                | View::ProjectSetup(true, _)
                | View::Repository(_)
                | View::ConfirmRepository(_)
        )
        .then(|| self.current_mod().map(|m| m.id))
        .flatten();
        for worker in self.workers.values_mut() {
            let code_mod = self
                .mods
                .iter_mut()
                .find(|code_mod| code_mod.id == worker.mod_id)
                .unwrap();
            worker.poll(
                &mut self.store,
                code_mod,
                editing_mod != Some(code_mod.id)
                    && deleting_mod != Some(code_mod.id)
                    && reviewing_mod != Some(code_mod.id)
                    && !self.git_jobs.contains_key(&code_mod.id)
                    && !code_mod.closed,
                &code_mod
                    .git_root
                    .as_ref()
                    .map(|root| git_mod::checkout(root))
                    .unwrap_or_else(|| self.project.clone()),
            )?;
        }
        for index in self.ready_plans() {
            self.auto_runs.remove(&self.mods[index].id);
            self.start_execution(index)?;
        }
        let edits = self
            .mods
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                !m.closed
                    && !m.queue.is_empty()
                    && !self.git_jobs.contains_key(&m.id)
                    && self
                        .git_states
                        .get(&m.id)
                        .is_none_or(|state| !state.continuing)
                    && editing_mod != Some(m.id)
                    && deleting_mod != Some(m.id)
                    && reviewing_mod != Some(m.id)
                    && m.execution.as_ref().is_some_and(|e| {
                        matches!(e.status.as_str(), "review" | "applied" | "blocked")
                    })
                    && !self
                        .workers
                        .values()
                        .any(|w| w.mod_id == m.id && (w.enabled || w.busy()))
            })
            .map(|(i, _)| i)
            .collect::<Vec<_>>();
        for index in edits {
            self.start_continuation(index);
        }
        if let Some(code_mod) = self.current_mod() {
            let ids = code_mod
                .queue
                .iter()
                .map(|message| message.id)
                .collect::<BTreeSet<_>>();
            self.queue_selection.retain(|id| ids.contains(id));
        }
        match self.view {
            View::Queue(index) => {
                let next = self
                    .current_mod()
                    .and_then(|code_mod| {
                        code_mod
                            .queue
                            .iter()
                            .position(|message| Some(message.id) == focused)
                    })
                    .unwrap_or(index);
                self.clamp_queue(next);
            }
            View::EditQueue(_) => {
                if let Some(index) = self.current_mod().and_then(|code_mod| {
                    code_mod
                        .queue
                        .iter()
                        .position(|message| Some(message.id) == focused)
                }) {
                    self.view = View::EditQueue(index);
                } else {
                    self.view = View::Chat;
                    self.restore_input();
                }
            }
            _ => {}
        }
        Ok(())
    }

    fn poll_git_jobs(&mut self) -> Result<()> {
        let finished = self
            .git_jobs
            .iter()
            .filter_map(|(id, job)| job.poll().map(|result| (*id, result)))
            .collect::<Vec<_>>();
        for (id, result) in finished {
            self.git_jobs.remove(&id);
            let Some(index) = self.mods.iter().position(|m| m.id == id) else {
                continue;
            };
            let root = self.mods[index].git_root.clone().unwrap();
            if let Ok(state) = git_mod::load(&root) {
                if state.published()
                    && self.mods[index]
                        .execution
                        .as_ref()
                        .is_some_and(|e| e.status == "applied")
                {
                    self.store.execution_status(id, "review")?;
                    self.mods[index].execution = self.store.execution(id)?;
                }
                if let Some(url) = &state.pr {
                    let item_id = format!("pr:{id}");
                    if !self.mods[index]
                        .messages
                        .iter()
                        .any(|message| message.item_id.as_deref() == Some(&item_id))
                    {
                        let message = crate::store::Message {
                            item_id: Some(item_id),
                            role: "harness".into(),
                            body: format!("PR ready · {url}"),
                            model: None,
                            effort: None,
                        };
                        self.store.save_message(id, &message)?;
                        self.mods[index].messages.push(message);
                    }
                }
                self.git_states.insert(id, state);
            }
            match result {
                Ok(git_mod::Result::Adopted) => {
                    if self.mods[index]
                        .execution
                        .as_ref()
                        .is_some_and(|e| e.status == "applied")
                    {
                        self.store.execution_status(id, "review")?;
                        self.mods[index].execution = self.store.execution(id)?;
                    }
                    self.notice = None;
                    if root.join("close-request").exists() {
                        git_mod::mark_closing(&root, false, false)
                            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                        std::fs::remove_file(root.join("close-request")).ok();
                        self.finish_mod(index, false, false)?;
                    } else if self.current_mod().is_some_and(|m| m.id == id) {
                        self.open_publication()?;
                    }
                }
                Ok(git_mod::Result::RepositoryConnected) => {
                    self.notice = None;
                    if self.current_mod().is_some_and(|m| m.id == id) {
                        if self.git_states.get(&id).is_some_and(|state| state.closing) {
                            self.finish_mod(index, false, false)?;
                        } else if self.can_publish() {
                            self.view = View::Publish;
                        }
                    }
                }
                Ok(git_mod::Result::Prepared) => {
                    if self.mods[index]
                        .planning
                        .as_ref()
                        .is_some_and(|p| p.status == "pending")
                    {
                        self.auto_plans.insert(id);
                    }
                    self.notice = None;
                }
                Ok(git_mod::Result::Refreshed) => {
                    self.store.restart_plan(id)?;
                    self.mods[index].planning = self.store.planning(id)?;
                    self.auto_plans.insert(id);
                    self.notice = None;
                }
                Ok(git_mod::Result::Snapshot) => {
                    let plan = self.mods[index]
                        .planning
                        .as_ref()
                        .unwrap()
                        .plan
                        .as_ref()
                        .unwrap();
                    self.store.create_execution(id, &root, plan)?;
                    self.mods[index].execution = self.store.execution(id)?;
                    self.start_worker(index, Role::Executor)?;
                }
                Ok(git_mod::Result::Reviewed(review)) => {
                    if self.current_mod().is_some_and(|m| m.id == id) {
                        if self.mods[index].execution.as_ref().is_some_and(|e| {
                            e.status == "review"
                                && e.fingerprint.as_deref() != Some(&review.fingerprint)
                        }) {
                            self.store.execution_status(id, "blocked")?;
                            self.mods[index].execution = self.store.execution(id)?;
                            self.notice = Some(
                                "Working files changed since verification. Ctrl+R rechecks.".into(),
                            );
                        }
                        self.review = Some(review);
                        self.view = View::Review(0);
                        if std::mem::take(&mut self.publish_after_review) && self.can_publish() {
                            self.open_publication()?;
                        }
                    }
                }
                Ok(git_mod::Result::Pruned) => {}
                Ok(git_mod::Result::Published) => {
                    self.notice = None;
                }
                Ok(git_mod::Result::Continued) => {
                    self.finish_edit(index)?;
                    git_mod::finish_continuation(&root)
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                    self.git_states.insert(id, git_mod::load(&root).unwrap());
                    self.notice = None;
                }
                Ok(git_mod::Result::Reopened) => {
                    self.store.reopen_mod(id)?;
                    self.mods[index].closed = false;
                    if self.current_mod().is_some_and(|m| m.id == id) {
                        self.show_closed = false;
                        self.restore_input();
                    }
                    self.notice = None;
                }
                Ok(git_mod::Result::Closed) => {
                    let selected = self.store.close_mod(self.project_id, id)?;
                    self.mods[index].closed = true;
                    self.mods[index].planning = self.store.planning(id)?;
                    self.mods[index].execution = self.store.execution(id)?;
                    if self.current_mod().is_some_and(|m| m.id == id) {
                        self.active = self.mods.iter().position(|m| Some(m.id) == selected);
                        self.view = if self.active.is_some() {
                            View::Chat
                        } else {
                            View::NewMod
                        };
                        self.restore_input();
                    }
                    self.notice = None;
                }
                Ok(git_mod::Result::Removed) => {
                    let was_active = self.current_mod().is_some_and(|m| m.id == id);
                    let selected = self.store.delete_mod(self.project_id, id)?;
                    self.mods.remove(index);
                    self.git_states.remove(&id);
                    self.active = self.mods.iter().position(|m| Some(m.id) == selected);
                    if was_active {
                        self.view = if self.active.is_some() {
                            View::Chat
                        } else {
                            View::NewMod
                        };
                        self.queue_selection.clear();
                        self.history_offset = 0;
                        self.plan_details = false;
                        self.restore_input();
                    } else if let View::Mods(focused) | View::DeleteMod(focused) = self.view {
                        self.view = View::Mods(
                            focused
                                .saturating_sub(usize::from(index < focused))
                                .min(self.mods.len()),
                        );
                    }
                    self.notice = std::fs::remove_dir_all(root)
                        .err()
                        .filter(|error| error.kind() != io::ErrorKind::NotFound)
                        .map(|error| error.to_string());
                }
                Err(error) => {
                    if self.current_mod().is_some_and(|m| m.id == id) {
                        self.publish_after_review = false;
                    }
                    self.notice = Some(if error.kind() == io::ErrorKind::Unsupported {
                        error.to_string()
                    } else if self.current_mod().is_some_and(|m| m.id == id) {
                        format!("{error} Work is retained; Ctrl+R retries.")
                    } else {
                        format!(
                            "{}: {error} Work is retained; select this mod with Ctrl+P to retry.",
                            self.mods[index].name
                        )
                    });
                }
            }
        }
        Ok(())
    }

    fn clamp_queue(&mut self, index: usize) {
        let len = self
            .current_mod()
            .map_or(0, |code_mod| code_mod.queue.len());
        if len == 0 {
            self.view = View::Chat;
            self.queue_selection.clear();
        } else {
            self.view = View::Queue(index.min(len - 1));
        }
    }

    fn celebrate(&mut self) {
        self.happy_since = self.motion.then(Instant::now);
    }

    fn save_draft(&mut self) -> Result<()> {
        if let Some(index) = self.active {
            let draft = self.input.lines().join("\n");
            let code_mod = &mut self.mods[index];
            if draft != code_mod.draft {
                self.store.save_draft(code_mod.id, &draft)?;
                code_mod.draft = draft;
            }
        }
        Ok(())
    }

    fn submit(&mut self) -> Result<()> {
        if self.read_only()
            || self.git_activity().is_some_and(|activity| {
                matches!(
                    activity,
                    "closing mod" | "deleting mod" | "reopening codemod"
                )
            })
        {
            self.notice = Some("Wait for this operation to finish.".into());
            return Ok(());
        }
        let message = self.input.lines().join("\n");
        if let Some(index) = self.active
            && !message.trim().is_empty()
        {
            let code_mod = &mut self.mods[index];
            let queued = self.store.enqueue(code_mod.id, &message)?;
            code_mod.queue.push(queued);
            code_mod.draft.clear();
            self.input.clear();
            self.celebrate();
        }
        Ok(())
    }
}

impl Drop for App {
    fn drop(&mut self) {
        if let Some(root) = self.setup_root.take() {
            let _ = std::fs::remove_dir_all(root);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_support::TestData;

    fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.handle(Event::Key(KeyEvent::new(code, modifiers)))
            .unwrap();
        wait_git(app);
    }

    fn test_project(data: &TestData, name: &str) -> PathBuf {
        let path = data.0.join(name);
        std::fs::create_dir_all(&path).unwrap();
        commit_project(&path);
        path
    }

    #[test]
    fn new_mods_fetch_the_merged_source_before_planning() {
        let (data, repo, _root, target) = crate::git_sync::tests::remote_change();
        std::fs::write(repo.join("new.txt"), "local edit\n").unwrap();
        let repo = repo.canonicalize().unwrap();
        let mut app = App::load(repo.clone(), false, data.store()).unwrap();
        paste(&mut app, "Edit existing items");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.worker_error().is_none());
        let code_mod = &app.mods[0];
        let root = code_mod.git_root.as_ref().unwrap();
        assert_eq!(git_mod::load(root).unwrap().base, target);
        assert_eq!(
            std::fs::read_to_string(app.source_project(0).join("new.txt")).unwrap(),
            "incoming\n"
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("new.txt")).unwrap(),
            "local edit\n"
        );
        assert!(app.auto_plans.contains(&code_mod.id));
    }

    #[test]
    fn failed_planning_refreshes_an_unstarted_worktree_and_keeps_messages_and_drafts() {
        let (data, repo, root, target) = crate::git_sync::tests::remote_change();
        let repo = repo.canonicalize().unwrap();
        let flag = std::sync::atomic::AtomicBool::new(false);
        git_mod::prepare(&repo, &root, &flag).unwrap();
        let mut store = data.store();
        let project = store.load_project(&repo).unwrap();
        let code_mod = store.create_mod(project.id, "Edit existing items").unwrap();
        let first_source = code_mod.planning.as_ref().unwrap().source.clone();
        store.save_git_root(code_mod.id, &root).unwrap();
        store.planning_status(code_mod.id, "failed").unwrap();
        store.enqueue(code_mod.id, "Also check cancel").unwrap();
        store.save_draft(code_mod.id, "keep the draft").unwrap();
        let planner = store.worker_for(code_mod.id, Role::Planner).unwrap();
        store
            .save_thread(planner.id, "planner-that-saw-empty-source")
            .unwrap();
        let mut app = App::load(repo, false, store).unwrap();
        let history = app.mods[0]
            .messages
            .iter()
            .map(|message| message.body.clone())
            .collect::<Vec<_>>();
        key(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(app.worker_error().is_none());
        assert_eq!(git_mod::load(&root).unwrap().base, target);
        assert_eq!(
            std::fs::read_to_string(app.source_project(0).join("new.txt")).unwrap(),
            "incoming\n"
        );
        assert_eq!(app.mods[0].planning.as_ref().unwrap().status, "pending");
        assert_ne!(app.mods[0].planning.as_ref().unwrap().source, first_source);
        assert!(
            app.store
                .worker_for(code_mod.id, Role::Planner)
                .unwrap()
                .thread_id
                .is_none()
        );
        assert_eq!(
            app.mods[0]
                .messages
                .iter()
                .map(|message| message.body.clone())
                .collect::<Vec<_>>(),
            history
        );
        assert_eq!(app.input.lines(), ["keep the draft"]);
        assert_eq!(queued(&app), ["Also check cancel"]);
        assert!(app.auto_plans.contains(&code_mod.id));
        assert!(app.mods[0].execution.is_none());
    }

    #[test]
    fn project_setup_reviews_files_and_cancellation_keeps_the_description() {
        let data = TestData::new();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("hello.txt"), "starting code\n").unwrap();
        std::fs::write(project.join(".gitattributes"), "*.txt text eol=lf\n").unwrap();
        std::fs::write(project.join("windows.txt"), "line one\r\nline two\r\n").unwrap();
        std::fs::write(project.join(".gitignore"), "private.db\n").unwrap();
        std::fs::write(project.join("private.db"), "local data").unwrap();
        std::fs::write(project.join(".env"), "secret").unwrap();
        let mut app = App::load(project.clone(), false, data.store()).unwrap();
        paste(&mut app, "Improve the greeting");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.view, View::ProjectSetup(false, _)) && app.mods.is_empty());
        assert!(!project.join(".git").exists());
        assert_eq!(
            app.setup_files,
            [".gitattributes", ".gitignore", "hello.txt", "windows.txt"]
        );
        let preview = app.setup_root.clone().unwrap();
        for width in [48, 100] {
            let rendered = rows(&screen(&mut app, width, 30)).join("\n");
            assert!(rendered.contains("set up Git") && rendered.contains("hello.txt"));
            assert!(!rendered.contains("private.db") && !rendered.contains(".env"));
        }
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(!preview.exists());
        assert_eq!(app.input.lines(), ["Improve the greeting"]);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.worker_error().is_none());
        let root = app.mods[0].git_root.as_ref().unwrap();
        assert!(git_mod::has_commit(&project) && root.join("snapshot-ready").exists());
        assert_eq!(
            workspace::source_state(&git_mod::checkout(root))
                .unwrap()
                .len(),
            4
        );
        assert_eq!(
            std::fs::read(git_mod::checkout(root).join("windows.txt")).unwrap(),
            b"line one\r\nline two\r\n"
        );
        assert_eq!(
            std::fs::read_to_string(project.join("private.db")).unwrap(),
            "local data"
        );
        assert!(app.auto_plans.contains(&app.mods[0].id));
    }

    #[test]
    fn changed_starting_files_block_setup_and_the_failed_mod_can_be_deleted() {
        let data = TestData::new();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("hello.txt"), "before").unwrap();
        let mut app = App::load(project.clone(), false, data.store()).unwrap();
        paste(&mut app, "Build greeting");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        std::fs::write(project.join("hello.txt"), "outside edit").unwrap();
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            app.worker_error()
                .unwrap()
                .contains("Starting files changed")
        );
        assert!(!project.join(".git").exists() && app.auto_plans.is_empty());
        let root = app.mods[0].git_root.clone().unwrap();
        app.delete_mod(0).unwrap();
        wait_git(&mut app);
        assert!(app.mods.is_empty() && !root.exists());
        assert_eq!(
            std::fs::read_to_string(project.join("hello.txt")).unwrap(),
            "outside edit"
        );
    }

    #[test]
    fn an_applied_snapshot_can_be_adopted_without_rerunning_or_overwriting_local_files() {
        let (_data, mut app, root) = execution_app();
        let id = app.mods[0].id;
        app.open_review().unwrap();
        workspace::review(&root)
            .unwrap()
            .apply(&app.project, &root)
            .unwrap();
        app.store.execution_status(id, "applied").unwrap();
        app.mods[0].execution = app.store.execution(id).unwrap();
        assert_eq!(app.mods[0].execution.as_ref().unwrap().status, "applied");
        key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(app.can_publish());
        key(&mut app, KeyCode::Char('p'), KeyModifiers::NONE);
        assert!(matches!(app.view, View::ProjectSetup(true, _)));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.worker_error().is_none());
        assert!(matches!(app.view, View::Repository(false)));
        assert_eq!(app.mods[0].id, id);
        assert_eq!(app.mods[0].execution.as_ref().unwrap().status, "review");
        assert_eq!(
            std::fs::read_to_string(app.project.join("hello.sh")).unwrap(),
            "new\n"
        );
        assert_eq!(
            std::fs::read_to_string(git_mod::checkout(&root).join("hello.sh")).unwrap(),
            "old\n"
        );
        assert!(app.workers.is_empty() && app.auto_plans.is_empty());
        paste(&mut app, "fixture/project");
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.view, View::ConfirmRepository(true)));
        let rendered = rows(&screen(&mut app, 100, 30)).join("\n");
        assert!(rendered.contains("Create private repository github.com/fixture/project"));
        assert!(!git_mod::has_origin(&app.project));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    fn paste(app: &mut App, text: &str) {
        app.handle(Event::Paste(text.to_owned())).unwrap();
    }

    fn queued(app: &App) -> Vec<&str> {
        app.current_mod()
            .unwrap()
            .queue
            .iter()
            .map(|message| message.body.as_str())
            .collect()
    }

    fn screen(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
        screen_at(app, width, height, None)
    }

    fn screen_at(
        app: &mut App,
        width: u16,
        height: u16,
        elapsed: Option<Duration>,
    ) -> ratatui::buffer::Buffer {
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                ui::draw(frame, app, sprout::Pose::IDLE, elapsed);
            })
            .unwrap();
        terminal.backend().buffer().clone()
    }

    fn rows(buffer: &ratatui::buffer::Buffer) -> Vec<String> {
        buffer
            .content
            .chunks(buffer.area.width as usize)
            .map(|row| row.iter().map(|cell| cell.symbol()).collect())
            .collect()
    }

    fn execution_app() -> (TestData, App, PathBuf) {
        let data = TestData::new();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("hello.sh"), "old\n").unwrap();
        let mut store = data.store();
        let project_id = store.load_project(&project).unwrap().id;
        let code_mod = store.create_mod(project_id, "Build greeting").unwrap();
        let plan = crate::plan::Plan::parse(r#"{"summary":"Greeting","tasks":[{"id":"one","title":"Greeting","outcome":"Print hello","files":["hello.sh"],"depends_on":[],"worker":"codex","checks":["prints hello"]}]}"#).unwrap();
        store
            .save_plan(
                code_mod.id,
                &code_mod.planning.as_ref().unwrap().source,
                &plan,
            )
            .unwrap();
        store.save_draft(code_mod.id, "keep this draft").unwrap();
        let root = data.0.join(data.0.file_name().unwrap());
        workspace::create(&project, &root).unwrap();
        store.create_execution(code_mod.id, &root, &plan).unwrap();
        std::fs::write(root.join("work/hello.sh"), "new\n").unwrap();
        let run = store.execution(code_mod.id).unwrap().unwrap().tasks[0]
            .source
            .clone();
        let checks = vec![crate::execution::CheckResult {
            check: "prints hello".into(),
            command: vec!["/usr/bin/true".into()],
            exit_code: Some(0),
            output: String::new(),
        }];
        store
            .finish_task(code_mod.id, &run, "done", "Done", &checks)
            .unwrap();
        let fingerprint =
            workspace::fingerprint(&workspace::source_state(&root.join("work")).unwrap()).unwrap();
        store
            .execution_checks(code_mod.id, "review", &checks, Some(&fingerprint))
            .unwrap();
        (data, App::load(project, false, store).unwrap(), root)
    }

    fn commit_project(project: &std::path::Path) {
        for args in [
            vec!["init", "--initial-branch", "main"],
            vec!["add", "."],
            vec!["commit", "--allow-empty", "-m", "Baseline"],
        ] {
            let output = workspace::trusted_git()
                .arg("-C")
                .arg(project)
                .args(args)
                .output()
                .unwrap();
            assert!(
                output.status.success(),
                "{}",
                String::from_utf8_lossy(&output.stderr)
            );
        }
    }

    fn wait_git(app: &mut App) {
        let start = Instant::now();
        while !app.git_jobs.is_empty() {
            assert!(start.elapsed() < Duration::from_secs(15));
            std::thread::sleep(Duration::from_millis(10));
            app.poll_git_jobs().unwrap();
        }
    }

    #[test]
    fn new_git_mods_keep_local_edits_and_reopen_without_starting_workers() {
        let data = TestData::new();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("hello.txt"), "committed\n").unwrap();
        commit_project(&project);
        std::fs::write(project.join("hello.txt"), "local edit\n").unwrap();
        let mut app = App::load(project.clone(), false, data.store()).unwrap();
        paste(&mut app, "Build a greeting");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let root = app.mods[0].git_root.clone().unwrap();
        wait_git(&mut app);
        assert!(app.worker_error().is_none());
        assert!(app.auto_plans.contains(&app.mods[0].id));
        assert_eq!(
            std::fs::read_to_string(git_mod::checkout(&root).join("hello.txt")).unwrap(),
            "committed\n"
        );
        assert_eq!(
            std::fs::read_to_string(project.join("hello.txt")).unwrap(),
            "local edit\n"
        );
        paste(&mut app, "saved draft");
        drop(app);
        let mut app = App::load(project, false, data.store()).unwrap();
        assert!(app.git_jobs.is_empty() && app.workers.is_empty() && app.auto_plans.is_empty());
        assert_eq!(app.input.lines(), ["saved draft"]);
        app.delete_mod(0).unwrap();
        wait_git(&mut app);
        assert!(app.mods.is_empty() && !root.exists());
    }

    #[test]
    fn background_git_mod_deletion_preserves_another_mods_queue_edit() {
        let data = TestData::new();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("hello.txt"), "hello\n").unwrap();
        commit_project(&project);
        let mut app = App::load(project, false, data.store()).unwrap();
        for description in ["First goal", "Second goal"] {
            app.view = View::NewMod;
            app.input.clear();
            paste(&mut app, description);
            key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            wait_git(&mut app);
        }
        let removed = app.mods[0].git_root.clone().unwrap();
        let remaining = app.mods[1].id;
        app.delete_mod(0).unwrap();
        paste(&mut app, "instruction");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "composer draft");
        key(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, " edited");
        let input = app.input.lines().to_vec();
        wait_git(&mut app);
        assert_eq!(app.current_mod().unwrap().id, remaining);
        assert!(matches!(app.view, View::EditQueue(0)));
        assert_eq!(app.input.lines(), input);
        assert_eq!(app.current_mod().unwrap().draft, "composer draft");
        assert!(!removed.exists());
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        app.delete_mod(0).unwrap();
        wait_git(&mut app);
    }

    #[test]
    fn saved_pr_recovers_history_and_stays_editable() {
        let (data, app, root) = execution_app();
        let project = app.project.clone();
        let id = app.mods[0].id;
        commit_project(&project);
        git_mod::prepare(&project, &root, &std::sync::atomic::AtomicBool::new(false)).unwrap();
        app.store.save_git_root(id, &root).unwrap();
        let mut state = git_mod::load(&root).unwrap();
        state.pr = Some("https://github.com/fixture/project/pull/1".into());
        state.phase = "published".into();
        std::fs::write(
            root.join("git-mod.json"),
            serde_json::to_vec(&state).unwrap(),
        )
        .unwrap();
        drop(app);
        let mut app = App::load(project.clone(), false, data.store()).unwrap();
        let display = rows(&screen(&mut app, 116, 40)).join("\n");
        assert!(display.contains("PR published") && display.contains("request edits"));
        assert_eq!(app.mods[0].execution.as_ref().unwrap().status, "review");
        assert!(!app.read_only() && git_mod::checkout(&root).exists());
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(queued(&app), ["keep this draft"]);
        drop(app);
        let app = App::load(project, false, data.store()).unwrap();
        assert_eq!(
            app.mods[0]
                .messages
                .iter()
                .filter(|m| m.item_id.as_deref() == Some(&format!("pr:{id}")))
                .count(),
            1
        );
    }

    #[test]
    fn closing_archives_history_and_the_picker_separates_closed_mods() {
        let (data, mut app, root) = execution_app();
        let id = app.mods[0].id;
        let history = app.mods[0]
            .messages
            .iter()
            .map(|m| m.body.clone())
            .collect::<Vec<_>>();
        key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        for width in [48, 100] {
            let display = rows(&screen(&mut app, width, 30)).join("\n");
            assert!(display.contains("No closed codemods"));
            assert!(display.contains("new codemod"));
            assert!(!display.contains("↑↓ select") && !display.contains("↵ open"));
        }
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        assert!(matches!(app.view, View::CloseMod(0)));
        assert!(
            rows(&screen(&mut app, 100, 30))
                .join("\n")
                .contains("Save a checkpoint")
        );
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.view, View::ProjectSetup(true, _)));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(root.exists() && git_mod::checkout(&root).exists());
        assert!(app.mods[0].closed);
        assert!(matches!(app.view, View::NewMod));
        let project = app.project.clone();
        drop(app);
        let mut app = App::load(project, false, data.store()).unwrap();
        assert!(app.current_mod().is_none());
        key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        assert!(app.picker_indices().is_empty());
        assert!(
            rows(&screen(&mut app, 100, 30))
                .join("\n")
                .contains("No active codemods")
        );
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.picker_indices(), [0]);
        assert!(
            rows(&screen(&mut app, 100, 30))
                .join("\n")
                .contains("closed codemods (1)")
        );
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.current_mod().unwrap().id, id);
        assert_eq!(
            app.mods[0]
                .messages
                .iter()
                .map(|m| m.body.clone())
                .collect::<Vec<_>>(),
            history
        );
        let display = rows(&screen(&mut app, 100, 30)).join("\n");
        assert!(display.contains("closed · history saved"));
        assert!(!display.contains("Describe a feature"));
        paste(&mut app, "ignored in closed history");
        assert_eq!(app.mods[0].draft, "keep this draft");
        key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.mods.is_empty());
    }

    #[test]
    fn background_edits_preserve_the_selected_mod_and_its_draft() {
        let (data, mut app, _root) = execution_app();
        let selected = app.mods[0].id;
        let plan = app.mods[0]
            .planning
            .as_ref()
            .unwrap()
            .plan
            .as_ref()
            .unwrap()
            .clone();
        let other = app
            .store
            .create_mod(app.project_id, "Another goal")
            .unwrap();
        app.store
            .save_plan(other.id, &other.planning.as_ref().unwrap().source, &plan)
            .unwrap();
        app.store
            .create_execution(other.id, &data.0.join("other"), &plan)
            .unwrap();
        app.store.execution_status(other.id, "blocked").unwrap();
        app.store.enqueue(other.id, "Fix the failed check").unwrap();
        app.store.select_mod(app.project_id, selected).unwrap();
        let state = app.store.load_project(&app.project).unwrap();
        app.mods = state.mods;
        app.poll_workers().unwrap();
        assert_eq!(app.current_mod().unwrap().id, selected);
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert_eq!(
            app.store.load_project(&app.project).unwrap().active_mod_id,
            Some(selected)
        );
        assert!(app.auto_plans.contains(&other.id));
        assert_eq!(app.mods[1].execution.as_ref().unwrap().status, "planning");
        assert!(app.mods[1].queue.is_empty());
    }

    #[test]
    fn completed_plans_schedule_execution_only_for_the_active_round() {
        let (_data, mut app, _root) = execution_app();
        let id = app.mods[0].id;
        app.auto_runs.insert(id);
        assert!(app.ready_plans().is_empty());
        app.mods[0].execution.as_mut().unwrap().status = "planning".into();
        assert_eq!(app.ready_plans(), [0]);
        app.mods[0].closed = true;
        assert!(app.ready_plans().is_empty());
        app.mods[0].closed = false;
        app.mods[0].planning.as_mut().unwrap().status = "paused".into();
        assert!(app.ready_plans().is_empty());
    }

    #[test]
    fn direct_publish_does_not_use_a_stale_diff_after_source_changes() {
        let (_data, mut app, root) = execution_app();
        commit_project(&app.project);
        let flag = std::sync::atomic::AtomicBool::new(false);
        git_mod::prepare(&app.project, &root, &flag).unwrap();
        let id = app.mods[0].id;
        app.store.save_git_root(id, &root).unwrap();
        app.mods[0].git_root = Some(root.clone());
        app.git_states.insert(id, git_mod::load(&root).unwrap());
        app.open_review().unwrap();
        wait_git(&mut app);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        app.review = Some(workspace::review(&root).unwrap());
        std::fs::write(root.join("work/hello.sh"), "not checked\n").unwrap();
        key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(!app.can_publish());
        assert!(matches!(app.view, View::Review(_)));
        assert_eq!(app.mods[0].execution.as_ref().unwrap().status, "blocked");
        assert!(!app.publish_after_review);
    }

    #[test]
    fn follow_up_keeps_conversation_and_prepares_a_fresh_plan() {
        let (_data, mut app, _root) = execution_app();
        let id = app.mods[0].id;
        let previous_source = app.mods[0].planning.as_ref().unwrap().source.clone();
        let count = app.mods[0].messages.len();
        app.store.follow_up(id, "Add due dates", None).unwrap();
        let state = app.store.load_project(&app.project).unwrap();
        let code_mod = &state.mods[0];
        assert!(!code_mod.closed && code_mod.execution.as_ref().unwrap().status == "planning");
        assert_eq!(code_mod.draft, "keep this draft");
        assert_eq!(state.active_mod_id, Some(id));
        let planning = code_mod.planning.as_ref().unwrap();
        assert_eq!(planning.status, "pending");
        assert!(planning.plan.is_none());
        assert_ne!(planning.source, previous_source);
        assert_eq!(code_mod.messages.len(), count + 1);
        assert_eq!(code_mod.messages.last().unwrap().body, "Add due dates");
        assert!(code_mod.description.contains("regressions:\nAdd due dates"));
    }

    #[test]
    fn a_closed_published_mod_reopens_and_queued_edits_keep_history_and_source() {
        let (data, project, root, program) = git_mod::tests::fixture();
        let flag = std::sync::atomic::AtomicBool::new(false);
        git_mod::prepare(&project, &root, &flag).unwrap();
        workspace::create(&git_mod::checkout(&root), &root).unwrap();
        std::fs::write(root.join("work/a.txt"), "published\n").unwrap();
        let pr = git_mod::publish(&root, "Greeting", false, &program, &flag).unwrap();
        let mut store = data.store();
        let project_id = store.load_project(&project).unwrap().id;
        let code_mod = store.create_mod(project_id, "Greeting").unwrap();
        store.save_git_root(code_mod.id, &root).unwrap();
        let mut app = App::load(project.clone(), false, store).unwrap();
        assert!(!app.read_only());
        app.finish_mod(0, false, false).unwrap();
        wait_git(&mut app);
        assert!(app.mods[0].closed && git_mod::checkout(&root).exists());
        git_mod::prune(&root, &flag).unwrap();
        assert!(!git_mod::checkout(&root).exists());
        app.reopen_mod(0);
        wait_git(&mut app);
        assert!(!app.mods[0].closed && app.published());
        paste(&mut app, "Add sorting");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "keep another draft");
        let restored_root = root.clone();
        app.git_jobs.insert(
            code_mod.id,
            Job::start("preparing edits", move |cancelled| {
                git_mod::prepare_edits(&restored_root, &program, cancelled)?;
                Ok(git_mod::Result::Continued)
            }),
        );
        wait_git(&mut app);
        assert!(!app.published());
        assert_eq!(app.mods[0].planning.as_ref().unwrap().status, "pending");
        assert!(app.auto_plans.contains(&code_mod.id));
        assert!(queued(&app).is_empty());
        assert_eq!(app.mods[0].messages.last().unwrap().body, "Add sorting");
        assert!(app.mods[0].messages.iter().any(|m| m.body.contains(&pr)));
        assert_eq!(
            std::fs::read_to_string(root.join("work/a.txt")).unwrap(),
            "published\n"
        );
        assert!(!root.join("vm.json").exists());
        assert_eq!(app.input.lines(), ["keep another draft"]);
        app.finish_mod(0, false, false).unwrap();
        wait_git(&mut app);
    }

    #[test]
    fn closing_saves_unpublished_work_without_github_and_retains_history() {
        let data = TestData::new();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("a.txt"), "original\n").unwrap();
        commit_project(&project);
        let mut app = App::load(project.clone(), false, data.store()).unwrap();
        paste(&mut app, "Change greeting");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let root = app.mods[0].git_root.clone().unwrap();
        workspace::create(&git_mod::checkout(&root), &root).unwrap();
        std::fs::write(root.join("work/a.txt"), "unpublished\n").unwrap();
        key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('c'), KeyModifiers::NONE);
        let display = rows(&screen(&mut app, 100, 30)).join("\n");
        assert!(display.contains("Save a checkpoint") && !display.contains("draft PR"));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.mods[0].closed && git_mod::checkout(&root).exists());
        assert!(git_mod::load(&root).unwrap().pr.is_none());
        assert_eq!(
            std::fs::read_to_string(git_mod::checkout(&root).join("a.txt")).unwrap(),
            "unpublished\n"
        );
        assert_eq!(app.mods[0].messages[0].body, "Change greeting");
        assert_eq!(
            std::fs::read_to_string(project.join("a.txt")).unwrap(),
            "original\n"
        );
        app.reopen_mod(0);
        wait_git(&mut app);
        assert!(!app.mods[0].closed && !app.read_only());
        app.delete_mod(0).unwrap();
        wait_git(&mut app);
        assert!(!root.exists());
    }

    #[test]
    fn unborn_git_gets_an_initial_commit_and_its_mod_can_be_removed() {
        let data = TestData::new();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        assert!(
            workspace::trusted_git()
                .arg("-C")
                .arg(&project)
                .arg("init")
                .status()
                .unwrap()
                .success()
        );
        let mut app = App::load(project, false, data.store()).unwrap();
        paste(&mut app, "First goal");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        wait_git(&mut app);
        assert!(matches!(app.view, View::ProjectSetup(false, _)));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        wait_git(&mut app);
        assert!(app.worker_error().is_none() && git_mod::has_commit(&app.project));
        let root = app.mods[0].git_root.clone().unwrap();
        app.delete_mod(0).unwrap();
        wait_git(&mut app);
        assert!(app.mods.is_empty() && !root.exists());
    }

    #[test]
    fn direct_publish_reviews_source_and_requires_confirmation_without_apply() {
        let (_data, mut app, _root) = execution_app();
        key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(matches!(app.view, View::ProjectSetup(true, _)));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Review(_)));
        assert!(app.can_publish());
        let rendered = rows(&screen(&mut app, 100, 30)).join("\n");
        assert!(rendered.contains("publish PR") && !rendered.contains("apply"));
        key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(matches!(app.view, View::Review(_)));
        assert_eq!(
            std::fs::read_to_string(app.project.join("hello.sh")).unwrap(),
            "old\n"
        );
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn changes_after_final_verification_require_rechecking_even_after_reopening() {
        let (data, app, root) = execution_app();
        let project = app.project.clone();
        drop(app);
        std::fs::write(root.join("work/another.txt"), "not verified").unwrap();
        let mut app = App::load(project, false, data.store()).unwrap();
        key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(!app.can_publish());
        key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        assert!(matches!(app.view, View::Review(_)));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(
            rows(&screen(&mut app, 116, 40))
                .join("\n")
                .contains("ctrl+r run")
        );
    }

    #[test]
    fn closing_a_legacy_snapshot_can_be_cancelled_without_discarding_source() {
        let (_data, mut app, root) = execution_app();
        app.finish_mod(0, false, false).unwrap();
        assert!(root.join("close-request").exists());
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Chat) && !app.mods[0].closed);
        assert!(!root.join("close-request").exists());
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert_eq!(
            std::fs::read_to_string(root.join("work/hello.sh")).unwrap(),
            "new\n"
        );
    }

    #[test]
    fn edit_round_consumes_one_request_once_and_keeps_the_execution_workspace() {
        let (data, mut app, root) = execution_app();
        let id = app.mods[0].id;
        let first = app.store.enqueue(id, "Add sorting").unwrap();
        let next = app.store.enqueue(id, "Add filtering").unwrap();
        app.mods[0].queue = vec![first, next];
        app.store.save_draft(id, "keep this draft").unwrap();
        app.finish_edit(0).unwrap();
        assert_eq!(app.mods[0].execution.as_ref().unwrap().workspace, root);
        assert_eq!(app.mods[0].execution.as_ref().unwrap().status, "planning");
        assert_eq!(queued(&app), ["Add filtering"]);
        let count = app.mods[0].messages.len();
        drop(app);
        let mut app = App::load(data.0.join("project"), false, data.store()).unwrap();
        app.finish_edit(0).unwrap();
        assert_eq!(app.mods[0].messages.len(), count);
        assert_eq!(queued(&app), ["Add filtering"]);
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert!(app.auto_plans.contains(&id));
    }

    #[test]
    fn active_worker_animates_while_replies_keep_their_model_and_effort() {
        let (_data, mut app, _root) = execution_app();
        let code_mod = app.current_mod().unwrap();
        let record = app.store.worker(code_mod.id).unwrap();
        let mut worker =
            Worker::start(&app.project, code_mod, record, Role::Planner, None).unwrap();
        worker.role = Role::Executor;
        worker.status = Status::Running;
        worker.model = Some("current-model".into());
        worker.effort = Some("high".into());
        app.workers.insert(worker.id, worker);
        let execution = app.mods[0].execution.as_mut().unwrap();
        execution.status = "running".into();
        execution.tasks[0].status = "running".into();
        app.mods[0].messages.push(crate::store::Message {
            item_id: Some("previous-reply".into()),
            role: "codex".into(),
            body: "Previous reply.".into(),
            model: Some("previous-model".into()),
            effort: Some("low".into()),
        });
        for (elapsed, glyph) in [(0, "⠋"), (80, "⠙"), (160, "⠹")] {
            let screen = rows(&screen_at(
                &mut app,
                116,
                40,
                Some(Duration::from_millis(elapsed)),
            ))
            .join("\n");
            assert!(screen.contains(&format!(
                "{glyph} codex · executor · current-model · high · running"
            )));
            assert!(screen.contains("◆ codex · executor · previous-model · low"));
            assert!(screen.contains(&format!("{glyph} 1. Greeting")));
        }
        let still = rows(&screen(&mut app, 116, 40)).join("\n");
        assert!(still.contains("⠿ codex · executor · current-model · high"));
        assert!(still.contains("⠿ 1. Greeting"));
        app.workers.values_mut().next().unwrap().status = Status::Checking;
        app.mods[0].execution.as_mut().unwrap().tasks[0].status = "checking".into();
        let checking = rows(&screen_at(
            &mut app,
            116,
            40,
            Some(Duration::from_millis(80)),
        ))
        .join("\n");
        assert!(checking.contains("⠙ 1. Greeting"));
        app.workers.values_mut().next().unwrap().status = Status::Ready;
        let idle = rows(&screen_at(
            &mut app,
            116,
            40,
            Some(Duration::from_millis(80)),
        ))
        .join("\n");
        assert!(idle.contains("◆ codex · executor · current-model · high · ready"));
        assert!(!idle.contains('⠙'));
        app.workers.values_mut().next().unwrap().role = Role::Planner;
        let planner = rows(&screen(&mut app, 116, 40)).join("\n");
        assert!(planner.contains("▤ codex · planner · current-model · high · ready"));
    }

    #[test]
    fn companion_follows_the_selected_mod_and_settles_when_stopped() {
        let (_data, mut app, _root) = execution_app();
        assert_eq!(app.companion_mood(), sprout::Mood::Finished);
        let other = app
            .store
            .create_mod(app.project_id, "Another goal")
            .unwrap();
        let record = app.store.worker(other.id).unwrap();
        let worker = Worker::start(&app.project, &other, record, Role::Planner, None).unwrap();
        app.workers.insert(worker.id, worker);
        app.mods.push(other);
        assert_eq!(app.companion_mood(), sprout::Mood::Finished);

        app.active = Some(1);
        assert_eq!(app.companion_mood(), sprout::Mood::Planning);
        let worker = app.workers.values_mut().next().unwrap();
        worker.role = Role::Executor;
        assert_eq!(app.companion_mood(), sprout::Mood::Working);
        let worker = app.workers.values_mut().next().unwrap();
        worker.enabled = false;
        worker.status = Status::Ready;
        assert_eq!(app.companion_mood(), sprout::Mood::Idle);
        app.workers.values_mut().next().unwrap().status = Status::Failed;
        assert_eq!(app.companion_mood(), sprout::Mood::Blocked);
        app.mods[1].closed = true;
        assert_eq!(app.companion_mood(), sprout::Mood::Idle);
        app.active = Some(0);
        app.mods[0].execution.as_mut().unwrap().status = "blocked".into();
        assert_eq!(app.companion_mood(), sprout::Mood::Blocked);
    }

    #[test]
    fn missing_recorded_effort_is_visible_without_guessing_a_level() {
        let (_data, mut app, _root) = execution_app();
        app.mods[0].messages.push(crate::store::Message {
            item_id: Some("old-reply".into()),
            role: "codex".into(),
            body: "Earlier reply.".into(),
            model: Some("gpt-6.1-sol".into()),
            effort: None,
        });
        let screen = rows(&screen(&mut app, 116, 40)).join("\n");
        assert!(screen.contains("◆ codex · executor · gpt-6.1-sol · effort unknown"));
    }

    #[test]
    fn expanded_checks_keep_code_indentation_and_the_toggle_in_view() {
        let (_data, mut app, _root) = execution_app();
        let result = &mut app.mods[0].execution.as_mut().unwrap().tasks[0].checks[0];
        result.command = vec!["/workspace/.venv/bin/python".into(), "-c".into(), "with app.test_client() as client:\n    response = client.get('/health')\n    assert response.status_code == 200, 'the server must return a successful response'".into()];
        for width in [48, 116] {
            app.plan_details = true;
            app.focus_plan = true;
            let expanded = rows(&screen(&mut app, width, 42));
            assert!(
                expanded
                    .iter()
                    .any(|row| row.contains("ctrl+o ▾ hide plan details"))
            );
            assert!(expanded.iter().any(|row| row.contains("│     response")));
            let start = expanded
                .iter()
                .position(|row| row.contains("   checks"))
                .unwrap();
            let end = expanded
                .iter()
                .position(|row| row.contains("   Done"))
                .unwrap();
            let code: Vec<_> = expanded[start..end]
                .iter()
                .filter(|row| row.contains('│'))
                .collect();
            assert!(code.len() >= 4);
            assert!(code.iter().all(|row| row.find('│') == Some(7)), "{code:#?}");
            assert!(
                code.iter()
                    .all(|row| row.trim_end().chars().count() <= width as usize - 2)
            );
            key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
            let collapsed = rows(&screen(&mut app, width, 42)).join("\n");
            assert!(collapsed.contains("ctrl+o ▸ show plan details"));
            assert!(!collapsed.contains("/workspace/.venv"));
            assert!(!collapsed.contains("execution stays"));
            assert_eq!(app.input.lines(), ["keep this draft"]);
        }
    }

    #[test]
    fn plan_review_preserves_data_and_keeps_the_outline_in_view() {
        use crate::{plan::Plan, router::Selection, store::Message};

        let data = TestData::new();
        let project = PathBuf::from("/review");
        let mut store = data.store();
        let project_id = store.load_project(&project).unwrap().id;
        let code_mod = store
            .create_mod(project_id, "build a local to-do app")
            .unwrap();
        let source = code_mod.planning.as_ref().unwrap().source.clone();
        store
            .save_message(
                code_mod.id,
                &Message {
                    item_id: Some("preface".into()),
                    role: "planner".into(),
                    body: "I'll inspect the source.".into(),
                    model: None,
                    effort: None,
                },
            )
            .unwrap();
        let plan = Plan::parse(r#"{"summary":"Build a local to-do app.","tasks":[
            {"id":"backend","title":"Build the API","outcome":"Persist tasks locally.","files":["app/main.py"],"depends_on":[],"worker":"codex","checks":["Adding and deleting works."]},
            {"id":"interface","title":"Build the page","outcome":"Add and remove tasks in the browser.","files":["app/static/index.html"],"depends_on":["backend"],"worker":"codex","checks":["Reload keeps saved tasks."]}
        ]}"#).unwrap();
        store
            .planning_model(
                code_mod.id,
                &Selection::fallback("Laya uncertain · Sol high fallback"),
            )
            .unwrap();
        store.save_plan(code_mod.id, &source, &plan).unwrap();
        let mut app = App::load(project.clone(), false, store).unwrap();
        paste(&mut app, "queued question");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "keep this draft\nsecond line");

        let compact = rows(&screen(&mut app, 100, 42)).join("\n");
        assert!(compact.contains("1. Build the API") && compact.contains("after task 1"));
        assert!(compact.contains("1. Build the API"));
        assert!(!compact.contains("app/main.py") && !compact.contains("gpt-6.1-sol"));
        assert!(!compact.contains("I'll inspect"));
        assert!(compact.contains("ctrl+q manage"));

        key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        let expanded = rows(&screen(&mut app, 100, 48)).join("\n");
        assert!(
            expanded.contains("app/main.py") && expanded.contains("Adding and deleting works.")
        );
        assert!(expanded.contains("gpt-6.1-sol · high") && expanded.contains("Laya uncertain"));
        assert_eq!(app.input.lines(), ["keep this draft", "second line"]);
        assert_eq!(queued(&app), ["queued question"]);
        assert_eq!(app.current_mod().unwrap().messages.len(), 3);

        key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        let narrow = rows(&screen(&mut app, 48, 24)).join("\n");
        assert!(narrow.contains("▤ codex · planner") && narrow.contains("1. Build the API"));
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        screen(&mut app, 48, 24);
        key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        let collapsed = rows(&screen(&mut app, 48, 24)).join("\n");
        assert!(collapsed.contains("1. Build the API") && !collapsed.contains("app/main.py"));
        let reopened = App::load(project, false, data.store()).unwrap();
        assert!(!reopened.plan_details);
        assert_eq!(
            reopened.current_mod().unwrap().messages[1].body,
            "I'll inspect the source."
        );
        assert_eq!(reopened.input.lines(), ["keep this draft", "second line"]);
    }

    #[test]
    fn wrapped_messages_keep_their_owner_spacing_and_highlight() {
        use crate::store::Message;

        let data = TestData::new();
        let project = PathBuf::from("/wrapped-review");
        let mut store = data.store();
        let project_id = store.load_project(&project).unwrap().id;
        let code_mod = store.create_mod(project_id, "café 界 with enough words to wrap across multiple terminal rows and keep the sender aligned").unwrap();
        for (role, body) in [("user", "another message"), ("codex", "an executor reply")] {
            store
                .save_message(
                    code_mod.id,
                    &Message {
                        item_id: None,
                        role: role.into(),
                        body: body.into(),
                        model: None,
                        effort: None,
                    },
                )
                .unwrap();
        }
        let mut app = App::load(project, false, store).unwrap();
        paste(&mut app, "draft stays");
        key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert!(!app.plan_details);
        assert_eq!(app.input.lines(), ["draft stays"]);
        for width in [36, 60, 100] {
            app.history_offset = u16::MAX;
            let buffer = screen(&mut app, width, 40);
            let rows = rows(&buffer);
            let first = rows.iter().position(|row| row.contains("> café")).unwrap() as u16;
            let next = rows
                .iter()
                .position(|row| row.contains("> another message"))
                .unwrap() as u16;
            let agent = rows
                .iter()
                .position(|row| row.contains("◆ codex · executor"))
                .unwrap() as u16;
            let background = buffer[(2, first)].bg;
            assert_ne!(background, ratatui::style::Color::Reset);
            assert_eq!(buffer[(4, first)].fg, ratatui::style::Color::White);
            assert!(
                !buffer[(4, first)]
                    .modifier
                    .contains(ratatui::style::Modifier::BOLD)
            );
            for y in first..next - 1 {
                assert_eq!(buffer[(2, y)].symbol(), if y == first { ">" } else { " " });
                assert_eq!(buffer[(width - 3, y)].bg, background);
            }
            assert!(rows[(next - 1) as usize].trim().is_empty());
            assert!(rows[(agent - 1) as usize].trim().is_empty());
            assert_ne!(buffer[(2, agent)].bg, background);
            assert!(rows[(agent + 1) as usize].contains("an executor reply"));
        }
    }

    #[test]
    fn descriptions_schedule_planning_but_reopening_does_not_start_workers() {
        let data = TestData::new();
        let project = test_project(&data, "project");
        let mut app = App::load(project.clone(), false, data.store()).unwrap();
        paste(&mut app, "Add greeting flag\nKeep the default greeting");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let code_mod = app.current_mod().unwrap();
        assert_eq!(code_mod.name, "Add greeting flag");
        assert_eq!(code_mod.messages[0].body, code_mod.description);
        assert!(code_mod.queue.is_empty());
        assert_eq!(code_mod.planning.as_ref().unwrap().status, "pending");
        assert!(app.auto_plans.contains(&code_mod.id));
        drop(app);
        let reopened = App::load(project, false, data.store()).unwrap();
        assert!(reopened.auto_plans.is_empty());
        assert!(reopened.workers.is_empty());
        assert_eq!(reopened.current_mod().unwrap().messages.len(), 1);
    }

    #[test]
    fn switching_and_restarting_restores_separate_queues_and_drafts() {
        let data = TestData::new();
        let project = test_project(&data, "project");
        let mut app = App::load(project.clone(), false, data.store()).unwrap();
        assert!(matches!(app.view, View::NewMod));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.mods.is_empty());
        paste(&mut app, "feature A");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "message A");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "draft A\nsecond line");
        key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "feature B");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.input.lines(), [""]);
        paste(&mut app, "message B");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "draft B");
        key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(queued(&app), ["message A"]);
        assert_eq!(app.current_mod().unwrap().messages.len(), 1);
        assert_eq!(app.input.lines(), ["draft A", "second line"]);
        drop(app);

        let reopened = App::load(project, false, data.store()).unwrap();
        assert_eq!(reopened.current_mod().unwrap().name, "feature A");
        assert_eq!(reopened.input.lines(), ["draft A", "second line"]);
        assert_eq!(reopened.mods[1].queue[0].body, "message B");
        assert_eq!(reopened.mods[1].draft, "draft B");
    }

    #[test]
    fn cancelling_creation_preserves_the_current_draft() {
        let data = TestData::new();
        let mut app = App::load(test_project(&data, "project"), false, data.store()).unwrap();
        paste(&mut app, "feature");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "keep this draft");
        key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "discard this name");
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Chat));
        assert_eq!(app.mods.len(), 1);
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn managing_queue_preserves_the_composer_and_persists_changes() {
        let data = TestData::new();
        let project = test_project(&data, "project");
        let mut app = App::load(project.clone(), false, data.store()).unwrap();
        paste(&mut app, "feature");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        for message in ["first", "second", "third"] {
            paste(&mut app, message);
            key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        }
        paste(&mut app, "keep this draft");
        key(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
        for code in [
            KeyCode::Char('k'),
            KeyCode::Char('j'),
            KeyCode::Char('j'),
            KeyCode::Char('j'),
        ] {
            key(&mut app, code, KeyModifiers::NONE);
        }
        assert_eq!(queued(&app), ["second", "third", "first"]);
        assert!(matches!(app.view, View::Queue(2)));

        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        app.input.clear();
        paste(&mut app, "discarded edit");
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(queued(&app), ["second", "third", "first"]);
        assert_eq!(app.input.lines(), ["keep this draft"]);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        app.input.clear();
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.view, View::EditQueue(2)));
        paste(&mut app, "edited");
        key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        paste(&mut app, "second line");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let reopened = App::load(project, false, data.store()).unwrap();
        assert_eq!(
            queued(&reopened),
            ["second", "third", "edited\nsecond line"]
        );
        assert_eq!(reopened.input.lines(), ["keep this draft"]);
        assert_eq!(reopened.current_mod().unwrap().messages.len(), 1);

        for _ in 0..3 {
            key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        }
        assert!(queued(&app).is_empty());
        assert!(matches!(app.view, View::Chat));
        key(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
        assert!(matches!(app.view, View::Chat));
        assert_eq!(app.input.lines(), ["keep this draft"]);
        drop(app);
        assert!(
            queued(&App::load(data.0.join("project"), false, data.store()).unwrap()).is_empty()
        );
    }
    #[test]
    fn marked_instructions_steer_in_queue_order_and_close_an_empty_manager() {
        let data = TestData::new();
        let project = test_project(&data, "steering-project");
        let mut app = App::load(project.clone(), false, data.store()).unwrap();
        paste(&mut app, "feature");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        for body in ["first", "second"] {
            paste(&mut app, body);
            key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        }
        paste(&mut app, "keep draft");
        key(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(matches!(app.view, View::Chat));
        assert!(app.queue_selection.is_empty());
        assert_eq!(app.input.lines(), ["keep draft"]);
        let reopened = App::load(project, false, data.store()).unwrap();
        assert!(queued(&reopened).is_empty());
        assert_eq!(
            reopened.current_mod().unwrap().steering,
            ["first", "second"]
        );
        assert_eq!(reopened.current_mod().unwrap().messages.len(), 1);
    }
    #[test]
    fn cancelling_mod_deletion_preserves_drafts_and_ignores_repeat_or_paste() {
        let data = TestData::new();
        let mut app = App::load(test_project(&data, "cancel-delete"), false, data.store()).unwrap();
        paste(&mut app, "keep mod");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "keep queue");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "keep draft");
        key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(matches!(app.view, View::DeleteMod(0)));
        paste(&mut app, "ignored paste");
        app.handle(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Enter,
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        )))
        .unwrap();
        assert!(matches!(app.view, View::DeleteMod(0)));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Mods(0)));
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        assert!(matches!(app.view, View::Mods(1)));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.input.lines(), ["keep draft"]);
        assert_eq!(queued(&app), ["keep queue"]);
        assert_eq!(app.mods.len(), 1);
    }

    #[test]
    fn deleting_active_inactive_and_last_mod_restores_the_right_composer() {
        let data = TestData::new();
        let project = test_project(&data, "delete-mods");
        let mut app = App::load(project.clone(), false, data.store()).unwrap();
        paste(&mut app, "a");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "queue a");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "draft a");
        key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "b");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "draft b");
        key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Chat));
        assert_eq!(app.current_mod().unwrap().name, "a");
        assert_eq!(app.input.lines(), ["draft a"]);
        drop(app);
        let mut app = App::load(project.clone(), false, data.store()).unwrap();
        assert_eq!(queued(&app), ["queue a"]);
        assert_eq!(app.input.lines(), ["draft a"]);
        key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "c");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "draft c");
        key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Up, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.current_mod().unwrap().name, "c");
        assert_eq!(app.input.lines(), ["draft c"]);
        key(&mut app, KeyCode::Char('p'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('d'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.view, View::NewMod));
        assert!(app.mods.is_empty());
        assert!(app.input.lines()[0].is_empty());
        let mut reopened = App::load(project, false, data.store()).unwrap();
        assert!(matches!(reopened.view, View::NewMod));
        paste(&mut reopened, "fresh mod");
        key(&mut reopened, KeyCode::Enter, KeyModifiers::NONE);
        let code_mod = reopened.current_mod().unwrap();
        assert!(code_mod.queue.is_empty());
        assert_eq!(code_mod.messages.len(), 1);
        assert!(code_mod.steering.is_empty());
    }
}
