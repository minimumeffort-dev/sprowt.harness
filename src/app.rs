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
    sprout,
    store::{CodeMod, Store, WorkerRecord, source_id},
    tools::{Context, Dispatcher, Request},
    ui,
    worker::{Status, Worker},
    workspace::{self, Review},
};

mod actions;
mod inspector;
mod scroll;
mod state;
mod tasks;
pub mod timeline;
pub use actions::{Action, ActionDock, ActionItem, Tone};
pub use scroll::Scroll;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum View {
    Chat,
    Actions(Action),
    Failure(u16),
    Mods(usize),
    DeleteMod(usize),
    CloseMod(usize),
    NewMod,
    Queue(usize),
    EditQueue(usize),
    Review(u16),
    Findings(u16),
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

pub struct App {
    pub network_requests: Vec<crate::network::Access>,
    pub muse: bool,
    pub project: PathBuf,
    pub input: TextArea<'static>,
    pub mods: Vec<CodeMod>,
    pub view: View,
    pub scroll: Scroll,
    action_origin: Option<View>,
    history_origin: Option<View>,
    pub history_offset: u16,
    pub page_size: u16,
    pub plan_details: bool,
    pub show_evidence: bool,
    pub focus_plan: bool,
    pub timeline: timeline::Timeline,
    pub review: Option<Review>,
    publish_after_review: bool,
    pub queue_selection: BTreeSet<i64>,
    pub show_closed: bool,
    pub setup_files: Vec<String>,
    setup_root: Option<PathBuf>,
    workers: BTreeMap<i64, Worker>,
    pub steer_target: Option<i64>,
    auto_plans: BTreeSet<i64>,
    auto_runs: BTreeSet<i64>,
    auto_reviews: BTreeSet<i64>,
    executing_mods: BTreeSet<i64>,
    git_jobs: BTreeMap<i64, Job>,
    project_job: Option<Job>,
    project_check: Option<Instant>,
    project_error: Option<String>,
    project_sync_root: PathBuf,
    git_states: BTreeMap<i64, GitMod>,
    targets: BTreeMap<i64, crate::mod_sync::Target>,
    target_checks: BTreeMap<i64, Instant>,
    update_errors: BTreeSet<i64>,
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
        let muse = crate::agents::detect()?;
        let project = project.canonicalize()?;
        let project = git_mod::project_root(&project).unwrap_or(project);
        let mut app = Self::load(project, motion, Store::local()?).map_err(io::Error::other)?;
        app.muse = muse;
        Ok(app)
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
            network_requests: Vec::new(),
            muse: false,
            project_sync_root: store
                .project_path(state.id)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?,
            project,
            input: ui::input(),
            mods: state.mods,
            view: if active.is_some() {
                View::Chat
            } else {
                View::NewMod
            },
            action_origin: None,
            history_origin: None,
            scroll: Scroll::default(),
            history_offset: 0,
            page_size: 1,
            plan_details: false,
            show_evidence: false,
            focus_plan: false,
            timeline: timeline::Timeline::default(),
            review: None,
            publish_after_review: false,
            queue_selection: BTreeSet::new(),
            show_closed: false,
            setup_files: Vec::new(),
            setup_root: None,
            workers: BTreeMap::new(),
            steer_target: None,
            auto_plans: BTreeSet::new(),
            auto_runs: BTreeSet::new(),
            auto_reviews: BTreeSet::new(),
            executing_mods: BTreeSet::new(),
            git_jobs: BTreeMap::new(),
            project_job: None,
            project_check: None,
            project_error: None,
            git_states,
            targets: BTreeMap::new(),
            target_checks: BTreeMap::new(),
            update_errors: BTreeSet::new(),
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

            let timeout = if self.welcome.running() {
                Duration::from_millis(33)
            } else if self.workers.is_empty()
                && self.git_jobs.is_empty()
                && self.project_job.is_none()
            {
                if self.motion {
                    next_pose.min(Duration::from_secs(1))
                } else {
                    Duration::from_secs(1)
                }
            } else {
                next_pose.min(Duration::from_millis(50))
            };
            if !event::poll(timeout)? {
                continue;
            }
            self.handle(event::read()?).map_err(io::Error::other)?;
        }
        Ok(())
    }

    fn handle(&mut self, event: Event) -> Result<()> {
        match event {
            Event::Mouse(mouse) => self.mouse_scroll(mouse)?,
            Event::Paste(text) => match self.view {
                View::Chat if self.read_only() => {}
                View::Chat | View::EditQueue(_) => {
                    self.timeline.focus = timeline::Focus::Composer;
                    self.input
                        .insert_str(text.replace("\r\n", "\n").replace('\r', "\n"));
                }
                View::NewMod | View::Repository(_) => {
                    self.input
                        .insert_str(text.replace("\r\n", "\n").replace('\r', "\n"));
                }
                View::Mods(_)
                | View::Actions(_)
                | View::Failure(_)
                | View::DeleteMod(_)
                | View::CloseMod(_)
                | View::Queue(_)
                | View::Review(_)
                | View::Checks(_)
                | View::Findings(_)
                | View::History(_)
                | View::Tasks(_)
                | View::Task(_, _)
                | View::TaskHistory(_, _)
                | View::Publish
                | View::ProjectSetup(_, _)
                | View::Network(_, _)
                | View::ConfirmRepository(_) => {}
            },
            Event::Key(key) if key.kind != KeyEventKind::Release => {
                let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
                if ctrl && key.code == KeyCode::Char('c') {
                    self.quit = true;
                    return Ok(());
                }
                if ctrl
                    && key.code == KeyCode::Char('u')
                    && matches!(self.view, View::Chat | View::NewMod | View::Mods(_))
                {
                    self.perform_action(Action::Update)?;
                    return Ok(());
                }
                if self.inspector_key(key)? {
                    return Ok(());
                }
                match self.view {
                    View::Checks(_) => {}
                    View::Actions(selected) => self.actions_key(key, selected)?,
                    View::Failure(scroll) => match key.code {
                        KeyCode::Esc => self.view = self.action_origin.take().unwrap_or(View::Chat),
                        KeyCode::Down => self.view = View::Failure(scroll.saturating_add(1)),
                        KeyCode::Up => self.view = View::Failure(scroll.saturating_sub(1)),
                        KeyCode::PageDown => {
                            self.view = View::Failure(scroll.saturating_add(self.page_size))
                        }
                        KeyCode::PageUp => {
                            self.view = View::Failure(scroll.saturating_sub(self.page_size))
                        }
                        _ => {}
                    },
                    View::Network(id, scroll) => match key.code {
                        KeyCode::Down => self.view = View::Network(id, scroll.saturating_add(1)),
                        KeyCode::Up => self.view = View::Network(id, scroll.saturating_sub(1)),
                        KeyCode::Esc => self.view = View::Chat,
                        KeyCode::Char('a' | 'd')
                            if key.modifiers.is_empty() && key.kind == KeyEventKind::Press =>
                        {
                            if let Some(mod_id) = self.current_mod().map(|m| m.id) {
                                self.store.decide_network(
                                    mod_id,
                                    id,
                                    key.code == KeyCode::Char('a'),
                                )?;
                                if key.code == KeyCode::Char('a') {
                                    self.set_paused(self.active.unwrap(), false)?;
                                    self.executing_mods.insert(mod_id);
                                }
                                self.refresh_network_requests()?;
                            }
                        }
                        _ => {}
                    },
                    View::Chat => self.chat_key(key)?,
                    View::Mods(index) => self.picker_key(key, index)?,
                    View::DeleteMod(index) => match key.code {
                        KeyCode::Esc => {
                            self.view = self.action_origin.take().unwrap_or_else(|| {
                                View::Mods(
                                    self.picker_indices()
                                        .iter()
                                        .position(|i| *i == index)
                                        .unwrap_or(0),
                                )
                            });
                        }
                        KeyCode::Enter
                            if key.modifiers.is_empty() && key.kind == KeyEventKind::Press =>
                        {
                            self.action_origin = None;
                            self.delete_mod(index)?;
                        }
                        _ => {}
                    },
                    View::CloseMod(index) => match key.code {
                        KeyCode::Esc => {
                            self.view = self.action_origin.take().unwrap_or(View::Mods(0))
                        }
                        KeyCode::Enter
                            if key.modifiers.is_empty() && key.kind == KeyEventKind::Press =>
                        {
                            self.action_origin = None;
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
                        KeyCode::Char('r') if key.modifiers.is_empty() && self.version_ready() => {
                            if let Some(index) = self.active {
                                self.begin_agent_review(index)?;
                            }
                            self.view = View::Chat;
                        }
                        KeyCode::Char('p') if key.modifiers.is_empty() && self.can_publish() => {
                            self.open_publication()?
                        }
                        _ => {}
                    },
                    View::Findings(scroll) => self.findings_key(key, scroll)?,
                    View::Tasks(selected) => self.tasks_key(key, selected)?,
                    View::Task(id, scroll) => self.task_key(key, id, scroll, false)?,
                    View::TaskHistory(id, scroll) => self.task_key(key, id, scroll, true)?,
                    View::History(scroll) => match key.code {
                        KeyCode::Esc => {
                            self.view = self.history_origin.take().unwrap_or(View::Chat)
                        }
                        KeyCode::Char('t') if ctrl => self.view = View::Chat,
                        KeyCode::Down => self.view = View::History(scroll.saturating_add(1)),
                        KeyCode::Up => self.view = View::History(scroll.saturating_sub(1)),
                        KeyCode::PageDown => {
                            self.view = View::History(scroll.saturating_add(self.page_size))
                        }
                        KeyCode::PageUp => {
                            self.view = View::History(scroll.saturating_sub(self.page_size))
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
                        KeyCode::Char('g') if ctrl => self.open_actions(),
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
        if self.timeline_key(key)? {
            return Ok(());
        }
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.quit = true,
            KeyCode::Char('g') if ctrl => self.open_actions(),
            KeyCode::Char(c) if ctrl && Action::has_shortcut(c) => {
                self.action_shortcut(c)?;
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
            self.steer_target = None;
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
        if key.code == KeyCode::Char('t') && key.modifiers.is_empty() {
            let workers = self.running_workers();
            self.steer_target = self
                .steer_target
                .and_then(|id| workers.iter().position(|w| *w == id))
                .map_or_else(|| workers.first().copied(), |i| workers.get(i + 1).copied());
            return Ok(());
        }
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
            let running = self.running_workers();
            let targets = if let Some(id) = self.steer_target {
                if !running.contains(&id) {
                    self.notice =
                        Some("That worker has stopped. Select a running worker or all.".into());
                    return Ok(());
                }
                vec![id]
            } else {
                running
            };
            let messages = self.store.steer_to(mod_id, &ids, &targets)?;
            self.steer_target = None;
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
        self.project_job.take();
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
        if self.current_mod().is_some_and(|m| m.git_root.is_some())
            && git_mod::has_origin(&self.project)
        {
            self.check_target(self.active.unwrap(), true, true);
            return Ok(());
        }
        self.confirm_publication()
    }

    fn confirm_publication(&mut self) -> Result<()> {
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
            self.auto_reviews.remove(&id);
            self.executing_mods.remove(&id);
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
        self.auto_reviews.remove(&mod_id);
        self.executing_mods.remove(&mod_id);
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
        self.auto_reviews.remove(&id);
        self.executing_mods.remove(&id);
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

    pub fn project_activity(&self) -> Option<&str> {
        self.project_job.as_ref().map(|job| job.label)
    }

    pub fn git_retry_pending(&self) -> bool {
        self.current_mod().is_some_and(|m| {
            self.update_errors.contains(&m.id)
                || m.git_root.as_ref().is_some_and(|root| {
                    git_mod::setup_pending(root).is_some()
                        || git_mod::repository_pending(root).is_some()
                })
                || m.git_root.is_some()
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

    fn check_target(&mut self, index: usize, publish: bool, explicit: bool) {
        let code_mod = &self.mods[index];
        if code_mod.closed
            || code_mod.git_root.is_none()
            || self.git_jobs.contains_key(&code_mod.id)
        {
            return;
        }
        let context = Context::harness(&self.project, code_mod);
        self.update_errors.remove(&code_mod.id);
        self.target_checks.insert(code_mod.id, Instant::now());
        self.git_jobs.insert(
            code_mod.id,
            Job::start("checking target branch", move |cancelled| {
                let target = match Dispatcher::new(&context, None, cancelled)
                    .execute(Request::CheckTarget, |_| {})?
                {
                    crate::tools::Output::Target(target) => target,
                    _ => unreachable!(),
                };
                Ok(git_mod::Result::TargetChecked {
                    target,
                    publish,
                    explicit,
                })
            }),
        );
    }

    fn start_update(&mut self, index: usize, target: crate::mod_sync::Target) {
        let id = self.mods[index].id;
        if self.git_jobs.contains_key(&id)
            || self
                .workers
                .values()
                .any(|w| w.mod_id == id && (w.enabled || w.busy()))
        {
            return;
        }
        let Some(plan) = self.mods[index]
            .planning
            .as_ref()
            .and_then(|p| p.plan.clone())
        else {
            return;
        };
        let context = Context::harness(&self.project, &self.mods[index]);
        let workers = self
            .workers
            .keys()
            .filter(|key| self.workers[key].mod_id == id)
            .copied()
            .collect::<Vec<_>>()
            .into_iter()
            .filter_map(|key| self.workers.remove(&key))
            .collect::<Vec<_>>();
        self.executing_mods.remove(&id);
        self.git_jobs.insert(
            id,
            Job::start("updating from target branch", move |cancelled| {
                drop(workers);
                Dispatcher::new(&context, None, cancelled)
                    .execute(Request::UpdateTarget { target, plan }, |_| {})?;
                Ok(git_mod::Result::Updated)
            }),
        );
        if self.current_mod().is_some_and(|m| m.id == id) {
            self.review = None;
            self.publish_after_review = false;
            self.view = View::Chat;
        }
    }

    fn maintain_updates(&mut self) -> Result<()> {
        for index in 0..self.mods.len() {
            let code_mod = &self.mods[index];
            let id = code_mod.id;
            let Some(root) = code_mod.git_root.as_ref() else {
                continue;
            };
            if code_mod.closed || code_mod.execution.is_none() || self.git_jobs.contains_key(&id) {
                continue;
            }
            if self.update_errors.contains(&id) {
                continue;
            }
            if self.current_mod().is_some_and(|m| m.id == id)
                && !matches!(self.view, View::Chat | View::Mods(_))
            {
                continue;
            }
            let busy = self
                .workers
                .values()
                .any(|w| w.mod_id == id && (w.enabled || w.busy()));
            if let Some(update) = crate::mod_sync::load(root)
                .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
            {
                if !busy && (!update.prepared || !update.installed) {
                    self.start_update(
                        index,
                        crate::mod_sync::Target {
                            branch: update.branch,
                            head: update.target,
                            pr_state: None,
                        },
                    );
                } else if !busy
                    && code_mod
                        .execution
                        .as_ref()
                        .is_some_and(|e| e.complete() && e.status == "review")
                {
                    let context = Context::harness(&self.project, code_mod);
                    self.git_jobs.insert(
                        id,
                        Job::start("saving verified merge", move |cancelled| {
                            Dispatcher::new(&context, None, cancelled)
                                .execute(Request::FinishUpdate, |_| {})?;
                            Ok(git_mod::Result::UpdateFinished)
                        }),
                    );
                }
                continue;
            }
            if self
                .targets
                .get(&id)
                .is_some_and(|t| t.pr_state.as_deref().is_some_and(|s| s != "OPEN"))
            {
                continue;
            }
            if !busy
                && !code_mod
                    .agent_review
                    .as_ref()
                    .is_some_and(|r| r.holds_updates(code_mod))
                && code_mod.execution.as_ref().is_some_and(|e| {
                    e.complete() && matches!(e.status.as_str(), "review" | "applied")
                })
                && let Some(target) = self.targets.get(&id)
                && self
                    .git_states
                    .get(&id)
                    .is_some_and(|s| s.base != target.head)
            {
                self.start_update(index, target.clone());
            } else if self
                .target_checks
                .get(&id)
                .is_none_or(|t| t.elapsed() >= Duration::from_secs(30))
                && self.git_states.get(&id).is_some_and(|s| {
                    ["ready", "published"].contains(&s.phase.as_str())
                        && !s.publishing
                        && !s.closing
                })
            {
                self.check_target(index, false, false);
            }
        }
        Ok(())
    }

    fn maintain_project(&mut self) {
        if self.project_job.is_some()
            || !self.git_jobs.is_empty()
            || self
                .project_check
                .is_some_and(|checked| checked.elapsed() < Duration::from_secs(30))
        {
            return;
        }
        self.project_check = Some(Instant::now());
        if !git_mod::has_commit(&self.project) {
            return;
        }
        let context = Context::project(&self.project, self.project_sync_root.clone());
        self.project_job = Some(Job::start("syncing project branch", move |cancelled| {
            Dispatcher::new(&context, None, cancelled).execute(Request::SyncProject, |_| {})?;
            Ok(git_mod::Result::ProjectSynced)
        }));
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

    pub fn active_workers(&self) -> impl Iterator<Item = &Worker> {
        self.workers.values().filter(|worker| {
            self.current_mod().is_some_and(|m| m.id == worker.mod_id) && worker.busy()
        })
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
            .or_else(|| workers().find(|worker| worker.role == Role::Reviewer && worker.busy()))
            .or_else(|| workers().find(|worker| worker.role == Role::Executor && worker.busy()))
            .or_else(|| {
                workers().find(|worker| worker.role == Role::Executor && worker.error.is_some())
            })
            .or_else(|| workers().find(|worker| worker.role == Role::Executor))
            .or_else(|| workers().next())
    }

    pub fn running_workers(&self) -> Vec<i64> {
        self.current_mod().map_or_else(Vec::new, |m| {
            self.workers
                .values()
                .filter(|w| {
                    w.mod_id == m.id && w.role == Role::Executor && w.status == Status::Running
                })
                .map(|w| w.id)
                .collect()
        })
    }

    pub fn worker_activity(&self) -> Vec<String> {
        self.current_mod().map_or_else(Vec::new, |m| {
            self.workers
                .values()
                .filter(|w| w.mod_id == m.id && w.role == Role::Executor && w.busy())
                .map(|w| w.activity(m))
                .collect()
        })
    }

    pub fn worker_error(&self) -> Option<&str> {
        self.current_mod()
            .and_then(|m| {
                self.workers
                    .values()
                    .filter(|w| w.mod_id == m.id)
                    .find_map(|w| w.error.as_deref())
            })
            .or(self.notice.as_deref())
            .or(self.project_error.as_deref())
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
        if self
            .git_jobs
            .get(&id)
            .is_some_and(|job| job.label != "checking target branch")
        {
            return Ok(());
        }
        let stopping = self
            .workers
            .values()
            .any(|w| w.mod_id == id && (w.enabled || w.busy()));
        self.set_paused(active, stopping)?;
        if stopping {
            self.auto_reviews.remove(&id);
            self.auto_plans.remove(&id);
            self.auto_runs.remove(&id);
        }
        if let Some(worker) = self
            .workers
            .values_mut()
            .find(|w| w.mod_id == id && w.role == Role::Reviewer && (w.enabled || w.busy()))
        {
            worker.toggle();
            if let Some(review) = &self.mods[active].agent_review {
                self.store.review_status(id, &review.source, "paused")?;
                self.mods[active].agent_review = self.store.review_state(id)?;
            }
            return Ok(());
        }
        if self.update_errors.remove(&id)
            && self.mods[active].git_root.as_ref().is_none_or(|root| {
                crate::mod_sync::load(root)
                    .ok()
                    .flatten()
                    .is_none_or(|u| !u.installed)
            })
        {
            self.check_target(active, false, false);
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
        if self.retry_verified_update(active)? {
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
            if self.mods[active].execution.as_ref().is_some_and(|e| {
                matches!(e.status.as_str(), "review" | "applied" | "blocked")
                    && !e.tasks.iter().any(|t| t.status == "waiting")
            }) && !self.mods[active].queue.is_empty()
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
        if role == Role::Executor {
            let running = self
                .workers
                .values()
                .any(|w| w.mod_id == mod_id && (w.enabled || w.busy()));
            if running {
                self.executing_mods.remove(&mod_id);
                self.store.pause_repairs(mod_id)?;
                self.mods[active].execution = self.store.execution(mod_id)?;
                for worker in self
                    .workers
                    .values_mut()
                    .filter(|w| w.mod_id == mod_id && w.role == role && w.enabled)
                {
                    worker.toggle();
                }
                return Ok(());
            }
            self.workers
                .retain(|_, w| w.mod_id != mod_id || w.role != role);
            self.store.retry_tasks(mod_id)?;
            self.mods[active].execution = self.store.execution(mod_id)?;
            return self.start_worker(active, role);
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
            if role == Role::Planner && worker.enabled {
                self.auto_runs.insert(mod_id);
            }
            return Ok(());
        }
        self.start_worker(active, role)
    }

    fn retry_verified_update(&mut self, index: usize) -> Result<bool> {
        let code_mod = &self.mods[index];
        let Some(root) = code_mod.git_root.as_ref() else {
            return Ok(false);
        };
        let Some(execution) = code_mod.execution.as_ref() else {
            return Ok(false);
        };
        let Some(last) = execution.tasks.last() else {
            return Ok(false);
        };
        if self.execution_busy()
            || !matches!(execution.status.as_str(), "review" | "blocked")
            || !matches!(last.status.as_str(), "done" | "blocked")
            || execution.tasks[..execution.tasks.len() - 1]
                .iter()
                .any(|task| task.status != "done")
            || execution.checks.is_empty()
            || execution
                .checks
                .iter()
                .any(|check| check.exit_code != Some(0))
            || execution.fingerprint.is_none()
        {
            return Ok(false);
        }
        let checked = (|| -> io::Result<bool> {
            let Some(update) = crate::mod_sync::load(root)? else {
                return Ok(false);
            };
            Ok(update.prepared
                && update.installed
                && update.imported
                && execution.fingerprint.as_ref()
                    == Some(&workspace::fingerprint(&workspace::source_state(
                        &root.join("work"),
                    )?)?))
        })()
        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
        if !checked {
            return Ok(false);
        }
        // Older saves marked the last passed task blocked; retain its verified result.
        let id = code_mod.id;
        self.store.task_status(id, &last.source, "done", None)?;
        self.store.execution_status(id, "review")?;
        self.mods[index].execution = self.store.execution(id)?;
        self.update_errors.remove(&id);
        self.notice = None;
        self.maintain_reviews()?;
        self.maintain_updates()?;
        Ok(true)
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
                    && !m.user_paused
                    && self.auto_runs.contains(&m.id)
                    && !self.git_jobs.contains_key(&m.id)
                    && m.planning.as_ref().is_some_and(|p| p.status == "ready")
                    && m.execution.as_ref().is_none_or(|e| e.status == "planning")
            })
            .map(|(i, _)| i)
            .collect()
    }

    fn start_worker(&mut self, index: usize, role: Role) -> Result<()> {
        let plan = self.mods[index]
            .planning
            .as_ref()
            .and_then(|p| p.plan.as_ref());
        let uses_muse = role == Role::Executor
            && (plan.is_some_and(|p| p.tasks.iter().any(|t| t.worker == "muse"))
                || self.mods[index].execution.as_ref().is_some_and(|e| {
                    e.tasks
                        .iter()
                        .any(|t| t.provider.as_deref() == Some("muse"))
                }));
        if uses_muse && !self.muse {
            self.notice = Some(format!(
                "This plan needs Muse {}. Install it, run muse login and restart the harness.",
                crate::muse::VERSION
            ));
            return Ok(());
        }
        let mod_id = self.mods[index].id;
        if role == Role::Executor {
            self.executing_mods.insert(mod_id);
            return self.schedule_execution(index);
        }
        let record = self.store.worker_provider(mod_id, role, 0, "codex")?;
        self.start_worker_record(index, role, record)
    }

    fn schedule_execution(&mut self, index: usize) -> Result<()> {
        let mod_id = self.mods[index].id;
        for record in self.store.executors(mod_id)? {
            if !self.workers.get(&record.id).is_some_and(Worker::busy) {
                self.store.resume_mail(mod_id, record.id)?;
            }
        }
        let busy = self
            .workers
            .values()
            .filter(|w| w.role == Role::Executor && w.busy())
            .map(|w| w.id)
            .collect::<Vec<_>>();
        let unavailable = self
            .workers
            .values()
            .filter(|w| w.mod_id == mod_id && w.status == Status::Failed)
            .map(|w| w.id)
            .chain(
                self.network_requests
                    .iter()
                    .filter(|r| r.mod_id == mod_id && r.status == "approved")
                    .map(|r| r.worker),
            )
            .collect::<Vec<_>>();
        let plan = self.mods[index]
            .planning
            .as_ref()
            .unwrap()
            .plan
            .as_ref()
            .unwrap();
        let records = self.store.schedule_workers(
            mod_id,
            plan,
            &busy,
            &unavailable,
            if self.muse {
                &["codex", "muse"]
            } else {
                &["codex"]
            },
        )?;
        self.mods[index].execution = self.store.execution(mod_id)?;
        for worker in self
            .workers
            .values_mut()
            .filter(|w| w.mod_id == mod_id && w.role == Role::Executor)
        {
            if !worker.busy() && !records.iter().any(|r| r.id == worker.id) {
                worker.enabled = false;
            }
        }
        for record in records {
            if let Some(worker) = self.workers.get_mut(&record.id) {
                if !worker.enabled && matches!(worker.status, Status::Ready | Status::Complete) {
                    worker.toggle();
                }
            } else {
                self.start_worker_record(index, Role::Executor, record)?;
            }
        }
        if self.mods[index]
            .execution
            .as_ref()
            .is_some_and(|e| matches!(e.status.as_str(), "review" | "applied"))
        {
            self.executing_mods.remove(&mod_id);
            self.auto_reviews.insert(mod_id);
        }
        if self.mods[index]
            .execution
            .as_ref()
            .is_some_and(|e| e.complete() && e.status == "blocked")
        {
            self.executing_mods.remove(&mod_id);
        }
        Ok(())
    }

    fn start_worker_record(
        &mut self,
        index: usize,
        role: Role,
        record: WorkerRecord,
    ) -> Result<()> {
        let project = self.source_project(index);
        let mod_id = self.mods[index].id;
        self.workers.remove(&record.id);
        let selection = if role == Role::Planner {
            if record.pending.is_none()
                && self.mods[index]
                    .planning
                    .as_ref()
                    .is_some_and(|p| p.status == "failed" || p.status == "paused")
            {
                self.store.retry_plan(mod_id)?;
                self.mods[index].planning = self.store.planning(mod_id)?;
            }
            Some(crate::router::Selection::planner())
        } else {
            None
        };
        match Worker::start_with_muse(
            &project,
            &self.mods[index],
            record,
            role,
            selection,
            self.muse,
        ) {
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
                    && m.git_root
                        .as_ref()
                        .is_none_or(|root| !root.join("main-update.json").exists())
                    && self.targets.get(&m.id).is_none_or(|target| {
                        target.pr_state.as_deref().is_none_or(|s| s == "OPEN")
                            && self
                                .git_states
                                .get(&m.id)
                                .is_none_or(|s| s.base == target.head)
                    })
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
        self.executing_mods.remove(&id);
        self.auto_reviews.remove(&id);
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

    fn set_paused(&mut self, index: usize, paused: bool) -> Result<()> {
        self.store.set_user_paused(self.mods[index].id, paused)?;
        self.mods[index].user_paused = paused;
        Ok(())
    }

    fn begin_agent_review(&mut self, index: usize) -> Result<()> {
        let code_mod = &self.mods[index];
        if code_mod.closed
            || !code_mod.queue.is_empty()
            || !code_mod.steering.is_empty()
            || self.git_jobs.contains_key(&code_mod.id)
            || self
                .workers
                .values()
                .any(|w| w.mod_id == code_mod.id && (w.enabled || w.busy()))
        {
            return Ok(());
        }
        let Some(execution) = code_mod
            .execution
            .as_ref()
            .filter(|e| e.complete() && e.status == "review")
        else {
            return Ok(());
        };
        let fingerprint = workspace::review(&execution.workspace)
            .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?
            .fingerprint;
        if execution.fingerprint.as_deref() != Some(&fingerprint) {
            self.store.execution_status(code_mod.id, "blocked")?;
            self.mods[index].execution = self.store.execution(self.mods[index].id)?;
            self.notice =
                Some("Source changed after checks. Ctrl+R reruns checks before review.".into());
            return Ok(());
        }
        self.store.begin_review(code_mod.id, &fingerprint)?;
        self.set_paused(index, false)?;
        let id = self.mods[index].id;
        self.auto_reviews.remove(&id);
        self.mods[index].agent_review = self.store.review_state(id)?;
        let record = self.store.worker_provider(id, Role::Reviewer, 0, "codex")?;
        self.start_worker_record(index, Role::Reviewer, record)
    }

    fn maintain_reviews(&mut self) -> Result<()> {
        for index in 0..self.mods.len() {
            let m = &self.mods[index];
            let id = m.id;
            if m.closed || m.user_paused || !m.queue.is_empty() || !m.steering.is_empty() {
                self.auto_reviews.remove(&id);
            }
            if let Some(r) = m.agent_review.as_ref() {
                let same_plan = m
                    .planning
                    .as_ref()
                    .is_some_and(|p| p.source == r.plan_source);
                if r.status != "stale"
                    && (!same_plan
                        || !m.queue.is_empty()
                        || !m.steering.is_empty()
                        || (r.status != "fixing" && !r.current(m)))
                {
                    self.store
                        .0
                        .execute("UPDATE reviews SET status='stale' WHERE mod_id=?1", [id])?;
                    self.mods[index].agent_review = self.store.review_state(id)?;
                    for worker in self
                        .workers
                        .values_mut()
                        .filter(|w| w.mod_id == id && w.role == Role::Reviewer && w.enabled)
                    {
                        worker.toggle();
                    }
                    continue;
                }
            }
            if self.review_due(index) {
                self.begin_agent_review(index)?;
            }
        }
        Ok(())
    }

    fn review_due(&self, index: usize) -> bool {
        let m = &self.mods[index];
        let busy = self
            .workers
            .values()
            .any(|w| w.mod_id == m.id && (w.enabled || w.busy()));
        let confirming = self.current_mod().is_some_and(|current| current.id == m.id)
            && matches!(
                self.view,
                View::Review(_)
                    | View::Publish
                    | View::DeleteMod(_)
                    | View::CloseMod(_)
                    | View::ProjectSetup(_, _)
                    | View::Repository(_)
                    | View::ConfirmRepository(_)
            );
        !m.closed
            && !m.user_paused
            && !self.git_states.get(&m.id).is_some_and(|s| s.published())
            && !self
                .targets
                .get(&m.id)
                .is_some_and(|t| matches!(t.pr_state.as_deref(), Some("MERGED" | "CLOSED")))
            && !busy
            && !confirming
            && !self.git_jobs.contains_key(&m.id)
            && m.queue.is_empty()
            && m.steering.is_empty()
            && m.execution
                .as_ref()
                .is_some_and(|e| e.complete() && e.status == "review" && e.fingerprint.is_some())
            && match m.agent_review.as_ref() {
                Some(r) if r.status == "fixing" => self.auto_reviews.contains(&m.id),
                Some(r) if r.status != "stale" && r.current(m) => false,
                _ => self.auto_reviews.contains(&m.id),
            }
    }

    fn poll_workers(&mut self) -> Result<()> {
        self.refresh_network_requests()?;
        self.poll_git_jobs()?;
        self.maintain_project();
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
        self.resume_network_workers()?;
        let scheduled = self
            .mods
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                self.executing_mods.contains(&m.id)
                    && !m.closed
                    && !m.user_paused
                    && !self.git_jobs.contains_key(&m.id)
                    && editing_mod != Some(m.id)
                    && deleting_mod != Some(m.id)
                    && reviewing_mod != Some(m.id)
            })
            .map(|(index, _)| index)
            .collect::<Vec<_>>();
        for index in scheduled {
            self.schedule_execution(index)?;
        }
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
                    && (worker.role != Role::Executor
                        || self.executing_mods.contains(&code_mod.id) && worker.enabled)
                    && deleting_mod != Some(code_mod.id)
                    && reviewing_mod != Some(code_mod.id)
                    && !self
                        .network_requests
                        .iter()
                        .any(|r| r.worker == worker.id && r.status == "approved")
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
        self.maintain_reviews()?;
        self.maintain_updates()?;
        let edits = self
            .mods
            .iter()
            .enumerate()
            .filter(|(_, m)| {
                !m.closed
                    && !m.user_paused
                    && !m.queue.is_empty()
                    && !self.git_jobs.contains_key(&m.id)
                    && m.git_root.as_ref().is_none_or(|root| {
                        crate::mod_sync::load(root)
                            .ok()
                            .flatten()
                            .is_none_or(|u| u.installed)
                    })
                    && self
                        .git_states
                        .get(&m.id)
                        .is_none_or(|state| !state.continuing)
                    && editing_mod != Some(m.id)
                    && deleting_mod != Some(m.id)
                    && reviewing_mod != Some(m.id)
                    && m.execution.as_ref().is_some_and(|e| {
                        matches!(e.status.as_str(), "review" | "applied" | "blocked")
                            && !e.tasks.iter().any(|t| t.status == "waiting")
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

    pub fn pending_network(&self) -> Option<&crate::network::Access> {
        let id = self.current_mod()?.id;
        self.network_requests
            .iter()
            .find(|r| r.mod_id == id && r.status == "pending")
    }

    fn refresh_network_requests(&mut self) -> Result<()> {
        self.network_requests.clear();
        for code_mod in self.mods.iter().filter(|m| !m.closed) {
            self.network_requests
                .extend(self.store.network_requests(code_mod.id)?);
        }
        if let View::Network(id, _) = self.view
            && !self
                .network_requests
                .iter()
                .any(|r| r.id == id && r.status == "pending")
        {
            self.view = self
                .pending_network()
                .map_or(View::Chat, |r| View::Network(r.id, 0));
        }
        Ok(())
    }

    fn resume_network_workers(&mut self) -> Result<()> {
        let requests = self
            .network_requests
            .iter()
            .filter(|r| r.status == "approved")
            .cloned()
            .collect::<Vec<_>>();
        for access in requests {
            if !self.executing_mods.contains(&access.mod_id)
                || self.git_jobs.contains_key(&access.mod_id)
                || self.workers.get(&access.worker).is_some_and(Worker::busy)
            {
                continue;
            }
            let Some(index) = self
                .mods
                .iter()
                .position(|m| m.id == access.mod_id && !m.closed && !m.user_paused)
            else {
                continue;
            };
            if let Some(record) = self.store.resume_network(&access)? {
                self.workers.remove(&record.id);
                self.mods[index].execution = self.store.execution(access.mod_id)?;
                self.notice = None;
            }
        }
        self.refresh_network_requests()
    }

    fn poll_git_jobs(&mut self) -> Result<()> {
        if let Some(result) = self.project_job.as_ref().and_then(Job::poll) {
            self.project_job.take();
            self.project_error = match result {
                Ok(git_mod::Result::ProjectSynced) => None,
                Err(error) if error.kind() == io::ErrorKind::WouldBlock => {
                    self.project_error.take()
                }
                Err(error) => Some(format!("Project sync paused: {error} Ctrl+U retries.")),
                _ => unreachable!(),
            };
        }
        let finished = self
            .git_jobs
            .iter()
            .filter_map(|(id, job)| job.poll().map(|result| (*id, result)))
            .collect::<Vec<_>>();
        for (id, result) in finished {
            let label = self.git_jobs.remove(&id).map(|job| job.label);
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
                            source: None,
                            task: None,
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
                Ok(git_mod::Result::ProjectSynced) => unreachable!(),
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
                            self.open_publication()?;
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
                Ok(git_mod::Result::TargetChecked {
                    target,
                    publish,
                    explicit,
                }) => {
                    if explicit
                        && let Some(target) = &target
                        && self
                            .git_states
                            .get(&id)
                            .is_some_and(|s| s.base != target.head)
                        && self.mods[index]
                            .execution
                            .as_ref()
                            .is_some_and(|e| e.complete() && e.status == "review")
                        && target.pr_state.as_deref().is_none_or(|s| s == "OPEN")
                    {
                        self.start_update(index, target.clone());
                    }
                    if let Some(target) = target {
                        self.targets.insert(id, target);
                    } else {
                        self.targets.remove(&id);
                    }
                    if publish
                        && self.current_mod().is_some_and(|m| m.id == id)
                        && self.can_publish()
                    {
                        self.confirm_publication()?;
                    }
                }
                Ok(git_mod::Result::Updated) => {
                    let mut update = crate::mod_sync::load(&root).unwrap().unwrap();
                    self.store.install_update(id, &root, &update)?;
                    update.installed = true;
                    crate::mod_sync::save(&root, &update)
                        .map_err(|e| rusqlite::Error::ToSqlConversionFailure(Box::new(e)))?;
                    let state = self.store.load_project(&self.project)?;
                    self.mods[index] = state.mods.into_iter().find(|m| m.id == id).unwrap();
                    self.start_worker(index, Role::Executor)?;
                    self.notice = None;
                }
                Ok(git_mod::Result::UpdateFinished) => {
                    self.target_checks.remove(&id);
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
                    if matches!(
                        label,
                        Some("updating from target branch" | "saving verified merge")
                    ) {
                        self.update_errors.insert(id);
                    }
                    if self.current_mod().is_some_and(|m| m.id == id) {
                        self.publish_after_review = false;
                    }
                    let detail = error.to_string();
                    let detail = detail.trim_end_matches(" Work is retained.");
                    self.notice = Some(if error.kind() == io::ErrorKind::Unsupported {
                        error.to_string()
                    } else if self.current_mod().is_some_and(|m| m.id == id) {
                        format!("{detail} Work is retained; Ctrl+R retries.")
                    } else {
                        format!(
                            "{}: {detail} Work is retained; select this mod with Ctrl+P to retry.",
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
            if let Some(question) = code_mod.question() {
                if message.len() > 4000 {
                    self.notice = Some("Keep the answer under 4,000 bytes.".into());
                    return Ok(());
                }
                let reply = self.store.user_answer(code_mod.id, question.id, &message)?;
                code_mod.messages.push(reply);
                code_mod.coordination = self.store.mailbox(code_mod.id)?;
            } else {
                let queued = self.store.enqueue(code_mod.id, &message)?;
                code_mod.queue.push(queued);
            }
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
    fn startup_sync_updates_main_without_a_mod_and_preserves_local_edits() {
        let (data, repo, root, target) = crate::git_sync::tests::remote_change();
        let flag = std::sync::atomic::AtomicBool::new(false);
        std::fs::write(repo.join("new.txt"), "local edit\n").unwrap();
        std::fs::write(repo.join("delete.txt"), "staged edit\n").unwrap();
        git_mod::git(&root, &repo, &["add", "delete.txt"], &flag).unwrap();
        let staged = git_mod::git(&root, &repo, &["diff", "--cached", "--raw"], &flag).unwrap();
        let mut app = App::load(repo.clone(), false, data.store()).unwrap();
        app.input.insert_str("Keep this description");
        app.poll_workers().unwrap();
        wait_git(&mut app);
        assert!(app.mods.is_empty() && app.worker_error().is_none());
        assert_eq!(
            git_mod::git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap(),
            target
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("new.txt")).unwrap(),
            "local edit\n"
        );
        assert_eq!(
            git_mod::git(&root, &repo, &["diff", "--cached", "--raw"], &flag).unwrap(),
            staged
        );
        assert_eq!(app.input.lines().join("\n"), "Keep this description");
        let saved = app.project_sync_root.clone();
        drop(app);
        assert_eq!(
            App::load(repo, false, data.store())
                .unwrap()
                .project_sync_root,
            saved
        );
    }

    #[test]
    fn periodic_sync_survives_deleting_the_last_published_mod() {
        let (data, repo, root, target) = crate::git_sync::tests::remote_change();
        let flag = std::sync::atomic::AtomicBool::new(false);
        git_mod::git(
            &root,
            &repo,
            &["push", "--force", "origin", "main:main"],
            &flag,
        )
        .unwrap();
        git_mod::prepare(&repo, &root, &flag).unwrap();
        let mut git = git_mod::load(&root).unwrap();
        git.phase = "published".into();
        git.pr = Some("https://github.com/fixture/project/pull/1".into());
        git.head = Some(git.base.clone());
        git_mod::save(&root, &git).unwrap();
        let mut store = data.store();
        let project = store.load_project(&repo).unwrap().id;
        let code_mod = store.create_mod(project, "Published change").unwrap();
        store.save_git_root(code_mod.id, &root).unwrap();
        let mut app = App::load(repo.clone(), false, store).unwrap();
        app.poll_workers().unwrap();
        wait_git(&mut app);
        app.delete_mod(0).unwrap();
        wait_git(&mut app);
        assert!(app.mods.is_empty() && !root.exists());
        git_mod::git(
            &app.project_sync_root,
            &repo,
            &["push", "origin", &format!("{target}:main")],
            &flag,
        )
        .unwrap();
        app.poll_workers().unwrap();
        assert!(app.project_job.is_none());
        app.project_check = Some(Instant::now() - Duration::from_secs(31));
        app.poll_workers().unwrap();
        wait_git(&mut app);
        assert!(app.worker_error().is_none());
        assert_eq!(
            git_mod::git(&app.project_sync_root, &repo, &["rev-parse", "HEAD"], &flag).unwrap(),
            target
        );
        assert!(app.mods.is_empty() && app.project_sync_root.exists());
    }

    #[test]
    fn unsafe_project_sync_keeps_files_and_ctrl_u_retries_from_new_mod() {
        let (data, repo, root, target) = crate::git_sync::tests::remote_change();
        let flag = std::sync::atomic::AtomicBool::new(false);
        let before = git_mod::git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap();
        std::fs::write(repo.join("a.txt"), "local edit\n").unwrap();
        let mut app = App::load(repo.clone(), false, data.store()).unwrap();
        app.input.insert_str("Next feature");
        app.poll_workers().unwrap();
        wait_git(&mut app);
        assert!(app.worker_error().unwrap().contains("Project sync paused"));
        assert_eq!(
            git_mod::git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap(),
            before
        );
        assert_eq!(
            std::fs::read_to_string(repo.join("a.txt")).unwrap(),
            "local edit\n"
        );
        std::fs::write(repo.join("a.txt"), "original\n").unwrap();
        key(&mut app, KeyCode::Char('u'), KeyModifiers::CONTROL);
        assert!(app.worker_error().is_none());
        assert_eq!(
            git_mod::git(&root, &repo, &["rev-parse", "HEAD"], &flag).unwrap(),
            target
        );
        assert_eq!(app.input.lines().join("\n"), "Next feature");
    }

    #[test]
    fn failed_final_checks_leave_active_scheduling_and_show_retry() {
        let (data, mut app, _) = execution_app();
        let id = app.mods[0].id;
        let mut checks = app.mods[0].execution.as_ref().unwrap().checks.clone();
        checks[0].exit_code = Some(1);
        checks[0].output = "Assertion failed on combined source".into();
        app.store
            .execution_checks(id, "blocked", &checks, None)
            .unwrap();
        app.executing_mods.insert(id);
        app.schedule_execution(0).unwrap();
        assert!(!app.executing_mods.contains(&id));
        assert!(!app.auto_reviews.contains(&id));
        assert!(app.workers.is_empty());
        assert_eq!(app.state_summary().phase, state::Phase::NeedsYou);
        assert!(app.action_dock().primary == Some(Action::Retry));
        key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(matches!(app.view, View::Checks(0)));
        let display = rows(&screen(&mut app, 100, 40)).join("\n");
        assert!(display.contains("Assertion failed on combined source"));
        let project = app.project.clone();
        drop(app);
        let app = App::load(project, false, data.store()).unwrap();
        assert_eq!(app.state_summary().phase, state::Phase::NeedsYou);
        assert!(app.action_dock().primary == Some(Action::Retry));
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert!(app.workers.is_empty());
    }

    #[test]
    fn failed_check_action_is_visible_and_preserves_draft_and_queue() {
        for from_menu in [false, true] {
            let (data, mut app, _) = execution_app();
            let mod_id = app.mods[0].id;
            let worker = app
                .store
                .worker_provider(mod_id, Role::Executor, 0, "codex")
                .unwrap();
            let task_id = app.mods[0].execution.as_ref().unwrap().tasks[0].id;
            app.store
                .0
                .execute(
                    "UPDATE task_runs SET worker_id=?2 WHERE id=?1",
                    rusqlite::params![task_id, worker.id],
                )
                .unwrap();
            let mut checks = app.mods[0].execution.as_ref().unwrap().checks.clone();
            checks[0].task = Some(task_id);
            checks[0].exit_code = Some(1);
            checks[0].output = "AssertionError: static/task-backup.mjs".into();
            let mut skipped = checks[0].clone();
            skipped.check = "Later browser flows".into();
            skipped.exit_code = None;
            skipped.output = "Not run.".into();
            checks.extend(vec![skipped; 22]);
            app.store
                .execution_checks(mod_id, "blocked", &checks, None)
                .unwrap();
            app.store.enqueue(mod_id, "Keep this queued edit").unwrap();
            app.store.save_draft(mod_id, "keep this draft").unwrap();
            let project = app.project.clone();
            drop(app);
            let mut app = App::load(project, false, data.store()).unwrap();
            assert_eq!(app.action_dock().primary, Some(Action::FixCheck(task_id)));
            key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
            assert!(app.view == View::Checks(0));
            let text = rows(&screen(&mut app, 100, 40))
                .join("\n")
                .replace('\u{a0}', " ");
            assert!(text.contains("AssertionError: static/task-backup.mjs"));
            assert!(text.contains("22 commands not run"));
            assert!(!text.contains("Later browser flows"));
            assert!(text.contains("x Fix failed check"), "{text}");
            key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
            assert!(
                rows(&screen(&mut app, 100, 40))
                    .join("\n")
                    .contains("Later browser flows")
            );
            key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
            if from_menu {
                key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
                key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
                assert!(app.view == View::Actions(Action::FixCheck(task_id)));
            }
            key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            assert!(app.view == View::Checks(0));
            assert!(app.mods[0].execution.as_ref().unwrap().tasks[0].check_repair);
            assert_eq!(app.state_summary().status, "Working · fixing failed check");
            assert!(
                rows(&screen(&mut app, 100, 40))
                    .join("\n")
                    .contains("Previous failed checks")
            );
            assert_eq!(app.input.lines(), ["keep this draft"]);
            assert_eq!(app.mods[0].queue[0].body, "Keep this queued edit");
            assert!(app.executing_mods.contains(&mod_id));
            assert!(app.failed_check_owner().is_none());
            let source = app.mods[0].execution.as_ref().unwrap().tasks[0]
                .source
                .clone();
            // The stale menu action cannot reopen or replace a running repair.
            app.view = View::Actions(Action::FixCheck(task_id));
            key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
            assert_eq!(
                app.mods[0].execution.as_ref().unwrap().tasks[0].source,
                source
            );
        }
    }

    #[test]
    fn connecting_repair_is_associated_only_with_its_selected_task() {
        let (_data, mut app, _root) = execution_app();
        let mod_id = app.mods[0].id;
        let record = app.store.worker_for(mod_id, Role::Executor).unwrap();
        // Construct the display fixture without opening a provider connection.
        let mut worker =
            Worker::start(&app.project, &app.mods[0], record, Role::Planner, None).unwrap();
        worker.role = Role::Executor;
        worker.status = Status::Connecting;
        let worker_id = worker.id;
        let m = &mut app.mods[0];
        let plan = m.planning.as_mut().unwrap().plan.as_mut().unwrap();
        let mut repair = plan.tasks[0].clone();
        repair.id = "repair".into();
        repair.title = "Repair offline check".into();
        plan.tasks.push(repair.clone());
        repair.id = "later".into();
        plan.tasks.push(repair);
        let execution = m.execution.as_mut().unwrap();
        execution.status = "running".into();
        execution.checks.clear();
        execution.tasks[0].worker = Some(worker_id);
        let done_id = execution.tasks[0].id;
        let mut repair = execution.tasks[0].clone();
        repair.id += 1;
        repair.task_id = "repair".into();
        repair.status = "pending".into();
        repair.check_repair = true;
        repair.verification_feedback = std::mem::take(&mut repair.checks);
        repair.verification_feedback[0].exit_code = Some(1);
        repair.verification_feedback[0].output = "Database fixture timed out".into();
        let repair_id = repair.id;
        execution.tasks.push(repair.clone());
        repair.id += 1;
        repair.task_id = "later".into();
        let later_id = repair.id;
        execution.tasks.push(repair);
        app.executing_mods.insert(mod_id);
        assert_eq!(
            app.inspect_task(repair_id).unwrap().state,
            "Waiting for a worker"
        );
        app.workers.insert(worker_id, worker);
        assert_eq!(app.inspect_task(done_id).unwrap().state, "Done");
        assert_eq!(
            app.inspect_task(repair_id).unwrap().state,
            "Connecting worker"
        );
        assert_eq!(
            app.inspect_task(later_id).unwrap().state,
            "Waiting for a worker"
        );
        assert_eq!(app.failed_task(), None);
        app.view = View::Checks(0);
        let visible = rows(&screen(&mut app, 120, 40)).join("\n");
        assert!(visible.contains("Repair offline check · Connecting worker"));
        assert!(visible.contains("Previous failed checks"));
        assert!(visible.contains("Database fixture timed out"));

        // Reconnecting a saved delivery takes precedence over pending work.
        app.mods[0].execution.as_mut().unwrap().tasks[2].status = "sending".into();
        assert_eq!(
            app.inspect_task(repair_id).unwrap().state,
            "Waiting for a worker"
        );
        assert_eq!(
            app.inspect_task(later_id).unwrap().state,
            "Connecting worker"
        );
        assert_eq!(app.inspect_task(done_id).unwrap().state, "Done");
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn project_sync_keeps_codemod_controls_available() {
        let (_data, mut app, _root) = execution_app();
        app.project_job = Some(Job::start("syncing project branch", |cancelled| {
            while !cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                std::thread::sleep(Duration::from_millis(1));
            }
            Ok(git_mod::Result::ProjectSynced)
        }));
        assert!(!app.execution_busy());
        assert!(app.version_ready());
        app.open_review().unwrap();
        assert!(matches!(app.view, View::Review(_)) && app.can_publish());
    }

    #[test]
    fn upstream_updates_wait_for_workers_and_keep_drafts_and_queued_edits() {
        for explicit in [false, true] {
            let (data, repo, root, target, plan) = crate::mod_sync::tests::fixture();
            let mut store = data.store();
            let project = store.load_project(&repo).unwrap();
            let code_mod = store.create_mod(project.id, "Improve deletion").unwrap();
            let id = code_mod.id;
            store.save_git_root(id, &root).unwrap();
            store
                .save_plan(id, &code_mod.planning.as_ref().unwrap().source, &plan)
                .unwrap();
            store.create_execution(id, &root, &plan).unwrap();
            std::fs::write(root.join("work/delete.txt"), "codemod version\n").unwrap();
            let run = store.execution(id).unwrap().unwrap().tasks[0]
                .source
                .clone();
            let checks = vec![crate::execution::CheckResult {
                timeout_seconds: None,
                duration_ms: None,
                missing_runtime: None,
                task: None,
                check: plan.tasks[0].checks[0].clone(),
                command: vec!["/usr/bin/true".into()],
                exit_code: Some(0),
                output: String::new(),
            }];
            store
                .finish_task(id, &run, "done", "Done", &checks)
                .unwrap();
            let fingerprint = workspace::review(&root).unwrap().fingerprint;
            store
                .execution_checks(id, "review", &checks, Some(&fingerprint))
                .unwrap();
            store.enqueue(id, "Later edit").unwrap();
            store.save_draft(id, "Keep this draft").unwrap();
            let mut app = App::load(repo, false, store).unwrap();
            app.targets.insert(id, target);
            app.target_checks.insert(id, Instant::now());
            let record = app.store.worker_for(id, Role::Planner).unwrap();
            let mut worker =
                Worker::start(&app.project, &app.mods[0], record, Role::Planner, None).unwrap();
            worker.status = Status::Running;
            worker.enabled = true;
            app.workers.insert(worker.id, worker);
            app.maintain_updates().unwrap();
            assert!(app.git_jobs.is_empty() && !root.join("main-update.json").exists());
            assert!(!app.version_ready());
            let worker = app.workers.values_mut().next().unwrap();
            worker.enabled = false;
            worker.status = Status::Ready;
            let source = app.mods[0].planning.as_ref().unwrap().source.clone();
            for status in [
                "pending", "running", "findings", "fixing", "paused", "blocked", "stale",
            ] {
                app.mods[0].agent_review = Some(crate::review::State {
                    source: "review".into(),
                    plan_source: source.clone(),
                    fingerprint: fingerprint.clone(),
                    status: status.into(),
                    rounds: 1,
                    report: None,
                });
                app.maintain_updates().unwrap();
                assert!(app.git_jobs.is_empty(), "Update started during {status}");
                assert!(!root.join("main-update.json").exists());
            }
            if explicit {
                app.check_target(0, false, true);
                let started = Instant::now();
                loop {
                    app.poll_git_jobs().unwrap();
                    if app
                        .git_jobs
                        .get(&id)
                        .is_some_and(|j| j.label == "updating from target branch")
                    {
                        break;
                    }
                    assert!(started.elapsed() < Duration::from_secs(15));
                    std::thread::sleep(Duration::from_millis(10));
                }
            } else {
                app.mods[0].agent_review.as_mut().unwrap().status = "clean".into();
                app.maintain_updates().unwrap();
            }
            let job = app.git_jobs.remove(&id).unwrap();
            let started = Instant::now();
            loop {
                if let Some(result) = job.poll() {
                    assert!(matches!(result.unwrap(), git_mod::Result::Updated));
                    break;
                }
                assert!(started.elapsed() < Duration::from_secs(15));
                std::thread::sleep(Duration::from_millis(10));
            }
            assert_eq!(app.input.lines(), ["Keep this draft"]);
            assert_eq!(app.mods[0].queue[0].body, "Later edit");
            assert!(app.workers.is_empty() && root.join("main-update.json").exists());
            assert!(matches!(app.view, View::Chat));
        }
    }

    #[test]
    fn failed_merge_saves_retry_verified_source_without_starting_workers() {
        for legacy in [false, true] {
            let (data, repo, root, target, plan) = crate::mod_sync::tests::fixture();
            let flag = std::sync::atomic::AtomicBool::new(false);
            std::fs::write(root.join("work/delete.txt"), "codemod version\n").unwrap();
            crate::mod_sync::prepare(&root, &target, &plan, &flag).unwrap();
            let mut update = crate::mod_sync::load(&root).unwrap().unwrap();
            update.imported = true;
            update.installed = true;
            crate::mod_sync::save(&root, &update).unwrap();
            let mut store = data.store();
            let project = store.load_project(&repo).unwrap();
            let code_mod = store.create_mod(project.id, "Improve deletion").unwrap();
            let id = code_mod.id;
            store.save_git_root(id, &root).unwrap();
            store.install_update(id, &root, &update).unwrap();
            let execution = store.execution(id).unwrap().unwrap();
            let checks = update.plan.tasks[0]
                .checks
                .iter()
                .map(|check| crate::execution::CheckResult {
                    timeout_seconds: None,
                    duration_ms: None,
                    missing_runtime: None,
                    task: Some(execution.tasks[0].id),
                    check: check.clone(),
                    command: vec!["/usr/bin/true".into()],
                    exit_code: Some(0),
                    output: String::new(),
                })
                .collect::<Vec<_>>();
            let source = execution.tasks[0].source.clone();
            store
                .finish_task(id, &source, "done", "Verified", &checks)
                .unwrap();
            let fingerprint = workspace::review(&root).unwrap().fingerprint;
            store
                .execution_checks(id, "review", &checks, Some(&fingerprint))
                .unwrap();
            store.save_draft(id, "Keep this draft").unwrap();
            let mut app = App::load(repo.clone(), false, store).unwrap();
            app.git_jobs.insert(
                id,
                Job::start("saving verified merge", |_| {
                    Err(io::Error::other(
                        "Source changed during saving. Work is retained.",
                    ))
                }),
            );
            wait_git(&mut app);
            assert!(app.mods[0].execution.as_ref().unwrap().complete());
            assert_eq!(app.mods[0].execution.as_ref().unwrap().status, "review");
            assert_eq!(
                app.notice
                    .as_ref()
                    .unwrap()
                    .matches("Work is retained")
                    .count(),
                1
            );
            assert!(app.git_retry_pending() && !app.can_publish());
            if legacy {
                app.store.task_status(id, &source, "blocked", None).unwrap();
                app.store.execution_status(id, "blocked").unwrap();
                drop(app);
                app = App::load(repo, false, data.store()).unwrap();
                std::fs::write(root.join("work/delete.txt"), "later edit\n").unwrap();
                assert!(!app.retry_verified_update(0).unwrap());
                assert!(app.git_jobs.is_empty() && app.workers.is_empty());
                std::fs::write(root.join("work/delete.txt"), "codemod version\n").unwrap();
            }
            key(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
            assert!(app.worker_error().is_none());
            assert!(app.workers.is_empty() && !root.join("main-update.json").exists());
            assert_eq!(
                app.mods[0].execution.as_ref().unwrap().tasks[0].source,
                source
            );
            assert_eq!(
                app.mods[0]
                    .execution
                    .as_ref()
                    .unwrap()
                    .fingerprint
                    .as_deref(),
                Some(fingerprint.as_str())
            );
            assert!(app.mods[0].execution.as_ref().unwrap().complete());
            assert_eq!(app.input.lines(), ["Keep this draft"]);
        }
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

    #[test]
    fn worker_questions_use_the_composer_and_routine_messages_stay_in_history() {
        let (data, mut store, m, workers, _) = crate::mailbox::tests::fixture();
        crate::mailbox::tests::send(
            &mut store, m.id, workers[0], "b", "update", None, "contract",
        );
        let ask = crate::mailbox::tests::send(
            &mut store, m.id, workers[0], "user", "ask", None, "decision",
        );
        let second = crate::mailbox::tests::send(
            &mut store,
            m.id,
            workers[1],
            "user",
            "ask",
            None,
            "other-decision",
        );
        store.save_draft(m.id, "Keep the current behavior").unwrap();
        let mut app = App::load(data.0.join("project"), false, store).unwrap();
        let compact = rows(&screen(&mut app, 110, 40)).join("\n");
        assert!(compact.contains("Message decision"));
        assert!(compact.contains(&format!("Answer #{ask}")));
        assert!(compact.contains("↵ Answer"));
        assert!(!compact.contains("Message contract"));
        assert_eq!(app.input.lines(), ["Keep the current behavior"]);
        key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        assert!(
            !rows(&screen(&mut app, 110, 50))
                .join("\n")
                .contains("Message contract")
        );
        key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        let task = app.mods[0].execution.as_ref().unwrap().tasks[0].id;
        assert_eq!(app.inspect_task(task).unwrap().state, "Needs your answer");
        key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
        let expanded = rows(&screen(&mut app, 110, 50)).join("\n");
        assert!(expanded.contains("Message contract"));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        let request = app
            .timeline_items()
            .into_iter()
            .find(|i| i.key == timeline::Key::Request)
            .unwrap()
            .title;
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(queued(&app).is_empty());
        assert_eq!(
            app.timeline_items()
                .into_iter()
                .find(|i| i.key == timeline::Key::Request)
                .unwrap()
                .title,
            request
        );
        assert!(app.input.lines().join("").is_empty());
        assert_eq!(app.current_mod().unwrap().question().unwrap().id, second);
        assert!(
            app.current_mod()
                .unwrap()
                .messages
                .last()
                .unwrap()
                .body
                .contains("Keep the current behavior")
        );
        paste(&mut app, "Use the simple option");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.current_mod().unwrap().question().is_none());
        assert!(queued(&app).is_empty());
        paste(&mut app, "Next edit");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(queued(&app), ["Next edit"]);
        let reopened = app.store.load_project(&app.project).unwrap();
        assert!(reopened.mods[0].question().is_none());
        assert!(
            reopened.mods[0]
                .messages
                .iter()
                .any(|m| m.body.contains("Use the simple option"))
        );
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

    fn wheel(app: &mut App, up: bool, area: ratatui::layout::Rect) {
        use ratatui::crossterm::event::{MouseEvent, MouseEventKind};
        assert!(area.width > 0 && area.height > 0);
        app.handle(Event::Mouse(MouseEvent {
            kind: if up {
                MouseEventKind::ScrollUp
            } else {
                MouseEventKind::ScrollDown
            },
            column: area.x,
            row: area.y,
            modifiers: KeyModifiers::NONE,
        }))
        .unwrap();
    }

    #[test]
    fn mouse_scrolling_keeps_conversation_and_composer_separate() {
        let (_data, mut app, _root) = execution_app();
        app.plan_details = true;
        app.mods[0]
            .messages
            .extend((0..40).map(|i| crate::store::Message {
                source: None,
                task: None,
                item_id: None,
                role: "user".into(),
                body: format!("Message {i}"),
                model: None,
                effort: None,
            }));
        app.input.clear();
        app.input.insert_str(
            (0..30)
                .map(|i| format!("Draft {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        let draft = app.input.lines().to_vec();
        let first = screen(&mut app, 80, 30);
        let content = app.scroll.content;
        wheel(&mut app, true, content);
        let scrolled = screen(&mut app, 80, 30);
        assert_ne!(first, scrolled);
        assert_eq!(app.history_offset, 3);
        let cursor = app.input.cursor();
        let input = app.scroll.input;
        wheel(&mut app, true, input);
        assert_ne!(app.input.cursor(), cursor);
        assert_eq!(app.history_offset, 3);
        assert_eq!(app.input.lines(), draft);
        wheel(&mut app, false, content);
        assert_eq!(app.history_offset, 0);
        screen(&mut app, 80, 30);
        // A view change cannot reuse the previous screen's hit areas.
        app.view = View::Mods(0);
        wheel(&mut app, false, content);
        assert!(matches!(app.view, View::Mods(0)));
        screen(&mut app, 80, 30);
        wheel(&mut app, false, ratatui::layout::Rect::new(0, 0, 1, 1));
        assert!(matches!(app.view, View::Mods(0)));
        screen(&mut app, 20, 8);
        assert_eq!(app.scroll.content, ratatui::layout::Rect::default());
        assert_eq!(app.scroll.input, ratatui::layout::Rect::default());
    }

    #[test]
    fn mouse_scrolling_reaches_list_items_without_activating_them() {
        let (_data, mut app, _root) = execution_app();
        let id = app.mods[0].id;
        for i in 0..20 {
            app.mods[0]
                .queue
                .push(app.store.enqueue(id, &format!("Instruction {i}")).unwrap());
            let code_mod = app
                .store
                .create_mod(app.project_id, &format!("Codemod {i}"))
                .unwrap();
            app.mods.push(code_mod);
        }
        let queue = queued(&app)
            .into_iter()
            .map(str::to_owned)
            .collect::<Vec<_>>();
        app.view = View::Queue(0);
        let first = screen(&mut app, 80, 24);
        for _ in 0..15 {
            let content = app.scroll.content;
            wheel(&mut app, false, content);
            screen(&mut app, 80, 24);
        }
        assert!(matches!(app.view, View::Queue(15)));
        assert_ne!(first, screen(&mut app, 80, 24));
        assert_eq!(queued(&app), queue);
        assert!(app.queue_selection.is_empty() && app.mods[0].steering.is_empty());
        app.view = View::Mods(0);
        for _ in 0..25 {
            screen(&mut app, 80, 24);
            let content = app.scroll.content;
            wheel(&mut app, false, content);
        }
        let last = rows(&screen(&mut app, 80, 24)).join("\n");
        assert!(matches!(app.view, View::Mods(21)));
        assert!(
            last.contains("Codemod 19") && last.contains("new codemod"),
            "{last}"
        );
        let content = app.scroll.content;
        wheel(&mut app, true, content);
        assert!(matches!(app.view, View::Mods(20)));
        app.view = View::Chat;
        app.open_actions();
        screen(&mut app, 80, 24);
        let content = app.scroll.content;
        wheel(&mut app, false, content);
        assert!(matches!(app.view, View::Actions(_)));
        assert!(app.workers.is_empty() && app.git_jobs.is_empty() && app.review.is_none());
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn mouse_scrolling_clamps_panels_and_preserves_confirmation_actions() {
        let (_data, mut app, root) = execution_app();
        let task = app.mods[0].execution.as_ref().unwrap().tasks[0].id;
        let source = app.mods[0].execution.as_ref().unwrap().tasks[0]
            .source
            .clone();
        app.mods[0].messages.push(crate::store::Message {
            source: Some(source),
            task: Some(task),
            item_id: None,
            role: "codex:executor:1".into(),
            body: (0..60)
                .map(|i| format!("Worker history {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
            model: None,
            effort: None,
        });
        let mut review = workspace::review(&root).unwrap();
        review.patch = (0..60)
            .map(|i| format!("+ Diff line {i}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.review = Some(review);
        app.notice = Some(
            (0..60)
                .map(|i| format!("Failure evidence {i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        );
        app.setup_files = (0..60).map(|i| format!("source/file-{i}.rs")).collect();
        for view in [
            View::Task(task, 0),
            View::TaskHistory(task, 0),
            View::History(0),
            View::Review(0),
            View::Failure(0),
            View::ProjectSetup(true, 0),
        ] {
            app.view = view;
            let first = screen(&mut app, 80, 24);
            assert_eq!(app.scroll.input, ratatui::layout::Rect::default());
            let content = app.scroll.content;
            wheel(&mut app, false, content);
            assert_ne!(first, screen(&mut app, 80, 24));
            for _ in 0..30 {
                let content = app.scroll.content;
                wheel(&mut app, false, content);
                screen(&mut app, 80, 24);
            }
            let last = screen(&mut app, 80, 24);
            let content = app.scroll.content;
            wheel(&mut app, false, content);
            assert_eq!(last, screen(&mut app, 80, 24));
            screen(&mut app, 100, 100);
            assert!(matches!(
                app.view,
                View::Task(_, 0)
                    | View::TaskHistory(_, 0)
                    | View::History(0)
                    | View::Review(0)
                    | View::Failure(0)
                    | View::ProjectSetup(true, _)
            ));
            assert_eq!(app.input.lines(), ["keep this draft"]);
        }
        for view in [View::DeleteMod(0), View::CloseMod(0), View::Publish] {
            app.view = view;
            screen(&mut app, 36, 18);
            let content = app.scroll.content;
            wheel(&mut app, false, content);
            assert!(app.view == view && app.mods.len() == 1 && !app.mods[0].closed);
            assert!(app.git_jobs.is_empty());
        }
        app.input.clear();
        app.input
            .insert_str(format!("owner/{}", "long-repository-name".repeat(20)));
        app.view = View::ConfirmRepository(false);
        let first = screen(&mut app, 36, 24);
        let content = app.scroll.content;
        wheel(&mut app, false, content);
        assert_ne!(first, screen(&mut app, 36, 24));
        assert!(app.scroll.offset > 0 && app.git_jobs.is_empty());
        app.view = View::Repository(false);
        screen(&mut app, 80, 24);
        assert_eq!(app.scroll.offset, 0);
        let input = app.scroll.input;
        wheel(&mut app, true, input);
        assert!(matches!(app.view, View::Repository(false)) && app.git_jobs.is_empty());
        app.view = View::NewMod;
        screen(&mut app, 80, 24);
        let input = app.scroll.input;
        wheel(&mut app, true, input);
        assert!(matches!(app.view, View::NewMod) && app.mods.len() == 1);
    }

    #[test]
    fn action_dock_recommends_review_publish_and_target_updates_from_state() {
        let (_data, mut app, root) = execution_app();
        assert!(app.action_dock().primary == Some(Action::Review));
        assert!(
            app.action_dock()
                .actions
                .iter()
                .any(|a| a.action == Action::Diff && a.label == "View diff")
        );
        let m = &mut app.mods[0];
        m.agent_review = Some(crate::review::State {
            source: "review:test".into(),
            plan_source: m.planning.as_ref().unwrap().source.clone(),
            fingerprint: m.execution.as_ref().unwrap().fingerprint.clone().unwrap(),
            status: "clean".into(),
            rounds: 0,
            report: None,
        });
        assert!(app.action_dock().primary == Some(Action::Publish));
        commit_project(&app.project);
        git_mod::prepare(
            &app.project,
            &root,
            &std::sync::atomic::AtomicBool::new(false),
        )
        .unwrap();
        let state = git_mod::load(&root).unwrap();
        let id = app.mods[0].id;
        app.mods[0].git_root = Some(root);
        app.targets.insert(
            id,
            crate::mod_sync::Target {
                branch: "main".into(),
                head: "new-head".into(),
                pr_state: Some("OPEN".into()),
            },
        );
        app.git_states.insert(id, state);
        assert!(app.action_dock().primary == Some(Action::Update));
        assert!(
            !app.action_dock()
                .actions
                .iter()
                .any(|a| a.action == Action::Publish)
        );
        let state = app.git_states.get_mut(&id).unwrap();
        state.base = "new-head".into();
        state.phase = "published".into();
        state.pr = Some("https://github.com/fixture/project/pull/1".into());
        assert_eq!(app.action_dock().status, "PR published");
        app.targets.get_mut(&id).unwrap().pr_state = Some("MERGED".into());
        assert!(app.action_dock().primary == Some(Action::NewMod));
        app.mods[0].closed = true;
        assert!(app.action_dock().primary == Some(Action::Reopen));
        assert!(
            !app.action_dock()
                .actions
                .iter()
                .any(|a| a.action == Action::Publish || a.action == Action::Review)
        );
    }

    #[test]
    fn menu_shortcuts_keep_typing_and_confirmations_intact() {
        let (_data, mut app, _root) = execution_app();
        for letter in ['n', 'c', 'd', 'f'] {
            key(&mut app, KeyCode::Char(letter), KeyModifiers::NONE);
        }
        assert!(matches!(app.view, View::Chat));
        let draft = app.input.lines().to_vec();
        assert_eq!(draft, ["keep this draftncdf"]);
        for (letter, action) in [('c', Action::Close), ('d', Action::Delete)] {
            app.open_actions();
            app.handle(Event::Key(KeyEvent::new_with_kind(
                KeyCode::Char(letter),
                KeyModifiers::NONE,
                KeyEventKind::Repeat,
            )))
            .unwrap();
            assert!(matches!(app.view, View::Actions(_)));
            key(&mut app, KeyCode::Char(letter), KeyModifiers::NONE);
            assert!(match action {
                Action::Close => matches!(app.view, View::CloseMod(0)),
                _ => matches!(app.view, View::DeleteMod(0)),
            });
            assert!(app.mods.len() == 1 && !app.mods[0].closed && app.git_jobs.is_empty());
            key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert_eq!(app.input.lines(), draft);
        }
        app.open_actions();
        let selected = app.view;
        key(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(app.view == selected);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        app.notice = Some("Check failed".into());
        app.open_actions();
        key(&mut app, KeyCode::Char('f'), KeyModifiers::NONE);
        assert!(matches!(app.view, View::Failure(0)));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.input.lines(), draft);
        app.open_actions();
        key(&mut app, KeyCode::Char('n'), KeyModifiers::NONE);
        assert!(matches!(app.view, View::NewMod) && app.input.lines() == [""]);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.input.lines(), draft);
    }

    #[test]
    fn every_menu_action_has_an_aligned_shortcut_and_the_dock_shows_menu_sequences() {
        let (_data, mut app, _root) = execution_app();
        app.notice = Some("Check failed".into());
        for width in [36, 80, 120] {
            app.open_actions();
            let mut columns = Vec::new();
            let dock = app.action_dock();
            for item in dock.menu_actions() {
                app.view = View::Actions(item.action);
                let rendered = rows(&screen(&mut app, width, 36));
                assert!(item.action.shortcut().is_some() || item.action.menu_shortcut().is_some());
                let line = rendered
                    .iter()
                    .find(|line| line.contains(&item.label))
                    .unwrap();
                let start = line.find(&item.label).unwrap();
                columns.push(line[..start].chars().count());
                if let Some(letter) = item.action.menu_shortcut() {
                    assert!(line[..start].trim().ends_with(letter));
                }
            }
            assert!(columns.windows(2).all(|pair| pair[0] == pair[1]));
            key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            let rendered = rows(&screen(&mut app, width, 36)).join("\n");
            assert!(
                rendered.contains(if width < 60 {
                    "^g f Show full error"
                } else {
                    "ctrl+g f Show full error"
                }),
                "{rendered}"
            );
        }
    }

    #[test]
    fn all_actions_preserves_drafts_and_revalidates_selected_actions() {
        let (_data, mut app, _root) = execution_app();
        let draft = app.input.lines().to_vec();
        let history_offset = app.history_offset;
        key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
        assert!(matches!(app.view, View::Actions(Action::Review)));
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Chat));
        assert_eq!(app.input.lines(), draft);
        assert_eq!(app.history_offset, history_offset);
        key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
        let id = app.mods[0].id;
        app.mods[0]
            .queue
            .push(app.store.enqueue(id, "Keep these edits").unwrap());
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Actions(Action::Review)));
        assert!(app.workers.is_empty() && app.mods[0].agent_review.is_none());
        key(&mut app, KeyCode::Char('s'), KeyModifiers::CONTROL);
        assert!(matches!(app.view, View::Chat));
        assert!(app.git_jobs.is_empty() && app.review.is_none());
        assert_eq!(app.input.lines(), draft);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(queued(&app), ["Keep these edits", "keep this draft"]);
    }

    #[test]
    fn a_stop_selection_cannot_turn_into_a_restart_when_worker_state_changes() {
        let (_data, mut app, _root) = execution_app();
        let m = &app.mods[0];
        let record = app.store.worker_for(m.id, Role::Planner).unwrap();
        let mut worker = Worker::start(&app.project, m, record, Role::Planner, None).unwrap();
        worker.status = Status::Running;
        app.workers.insert(worker.id, worker);
        key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
        app.view = View::Actions(Action::Stop);
        let worker = app.workers.values_mut().next().unwrap();
        worker.enabled = false;
        worker.status = Status::Ready;
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Actions(Action::Stop)));
        assert!(app.workers.values().all(|w| !w.enabled));
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn actions_return_to_new_mod_and_confirmation_cancellation_keeps_the_draft() {
        let data = TestData::new();
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        let mut app = App::load(project, false, data.store()).unwrap();
        paste(&mut app, "Describe a new app");
        key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::NewMod));
        assert_eq!(app.input.lines(), ["Describe a new app"]);
        let (_data, mut app, _root) = execution_app();
        key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
        app.view = View::Actions(Action::Delete);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.view, View::DeleteMod(_)));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Chat));
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert!(app.mods.len() == 1 && !app.quit);
    }

    #[test]
    fn unified_dock_groups_progress_composer_and_actions_without_clipping() {
        let (_data, mut app, _root) = execution_app();
        for width in [36, 48, 116] {
            let rendered = rows(&screen(&mut app, width, 30));
            let position = |text: &str| {
                rendered
                    .iter()
                    .position(|r| r.contains(text))
                    .unwrap_or_else(|| panic!("Missing {text}:\n{}", rendered.join("\n")))
            };
            let top = position("╭");
            let status = position("Checks passed");
            let action = position("Review changes");
            let divider = position("├");
            let composer = position("keep this draft");
            let hints = position("Request edits");
            let menu = position("More");
            let bottom = position("╰");
            let action = rendered
                .iter()
                .enumerate()
                .skip(top + 1)
                .find(|(_, row)| row.contains("Review changes"))
                .map(|(index, _)| index)
                .unwrap_or(action);
            assert!(top < status && status <= action && action < divider);
            assert!(divider < composer && composer < hints && hints <= menu && menu < bottom);
            assert_eq!(rendered.iter().filter(|r| r.contains('╭')).count(), 1);
            for row in &rendered[top + 1..bottom] {
                assert_eq!(
                    row.chars().nth(2),
                    Some(if row.contains('├') { '├' } else { '│' })
                );
                assert_eq!(
                    row.chars().nth(width as usize - 3),
                    Some(if row.contains('┤') { '┤' } else { '│' })
                );
            }
            assert!(rendered[menu].contains(if width < 68 { "^g More" } else { "ctrl+g More" }));
            assert!(rendered[bottom + 1..].iter().all(|r| r.trim().is_empty()));
            if width == 116 {
                assert_eq!(status, action);
                assert_eq!(hints, menu);
                assert_eq!(bottom - top + 1, 11);
                assert!(rendered[menu].ends_with("ctrl+g More │  "));
            }
            let text = rendered.join("\n");
            assert!(text.contains("Timeline"));
            assert!(!text.contains("Next ·") && !text.contains("message ╶"));
        }
        let error = "AssertionError: expected a successful response.\nFull evidence\n".repeat(30);
        app.notice = Some(error.clone());
        assert!(app.action_dock().primary == Some(Action::Failure));
        app.perform_action(Action::Failure).unwrap();
        assert!(matches!(app.view, View::Failure(_)));
        let first = rows(&screen(&mut app, 48, 24)).join("\n");
        assert!(first.contains("error details") && first.contains("AssertionError"));
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Failure(scroll) if scroll > 0));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Chat));
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert_eq!(app.worker_error(), Some(error.as_str()));
    }

    #[test]
    fn composer_grows_for_blank_lines_and_keeps_the_cursor_visible() {
        let (_data, mut app, _root) = execution_app();
        app.input.clear();
        for line in 0..8u16 {
            paste(&mut app, &format!("Draft line {line}"));
            key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
            let buffer = screen(&mut app, 100, 40);
            let input = app.scroll.input;
            assert_eq!(input.height, (line + 2).clamp(4, 8));
            let cursor_row = (line + 1).min(7);
            assert!(
                buffer[(input.x, input.y + cursor_row)]
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED)
            );
            if line < 7 {
                assert!(rows(&buffer).iter().any(|r| r.contains("Draft line 0")));
            }
            assert_eq!(app.input.cursor(), (line as usize + 1, 0));
            assert!(app.current_mod().unwrap().queue.is_empty());
        }
        let draft = app.input.lines().to_vec();
        app.open_actions();
        screen(&mut app, 100, 40);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        let buffer = screen(&mut app, 100, 40);
        let input = app.scroll.input;
        assert!(
            buffer[(input.x, input.bottom() - 1)]
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        assert_eq!(app.input.lines(), draft);

        for (width, height) in [(36, 24), (80, 24), (100, 40)] {
            let buffer = screen(&mut app, width, height);
            let input = app.scroll.input;
            assert!(input.height > 0 && input.height <= 8);
            assert!(rows(&buffer).iter().any(|r| r.contains("More")));
            assert!((input.y..input.bottom()).any(|y| {
                buffer[(input.x, y)]
                    .modifier
                    .contains(ratatui::style::Modifier::REVERSED)
            }));
            assert_eq!(app.input.lines(), draft);
        }
        app.input.clear();
        screen(&mut app, 48, 40);
        paste(&mut app, &"a".repeat(200));
        key(&mut app, KeyCode::Char('j'), KeyModifiers::CONTROL);
        let buffer = screen(&mut app, 48, 40);
        let input = app.scroll.input;
        assert_eq!(input.height, 6);
        assert!(
            buffer[(input.x, input.y + 5)]
                .modifier
                .contains(ratatui::style::Modifier::REVERSED)
        );
        assert_eq!(app.input.cursor(), (1, 0));
    }

    #[test]
    fn unified_dock_keeps_worker_progress_during_target_checks() {
        let (_data, mut app, _root) = execution_app();
        let m = &app.mods[0];
        let id = m.id;
        let record = app.store.worker_for(id, Role::Planner).unwrap();
        let mut worker = Worker::start(&app.project, m, record, Role::Planner, None).unwrap();
        worker.role = Role::Executor;
        worker.status = Status::Running;
        app.workers.insert(worker.id, worker);
        let working = rows(&screen(&mut app, 100, 30)).join("\n");
        assert!(working.contains("1/1 tasks finished"));
        assert!(working.contains("ctrl+r Pause") && working.contains("↵ Queue for next pass"));
        app.git_jobs.insert(
            id,
            Job::start("checking target branch", |_| {
                Ok(git_mod::Result::ProjectSynced)
            }),
        );
        let polling = rows(&screen(&mut app, 100, 30)).join("\n");
        assert!(polling.contains("1/1 tasks finished"));
        assert!(!polling.contains("checking target branch"));
        app.git_jobs.clear();
        app.workers.clear();
        let ready = rows(&screen(&mut app, 100, 30)).join("\n");
        assert!(
            ready.contains("Checks passed · review pending") && ready.contains("↵ Request edits")
        );
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert!(matches!(app.view, View::Chat));

        app.notice = Some("The request failed. ".repeat(30));
        let failed = rows(&screen(&mut app, 48, 30));
        let divider = failed.iter().position(|r| r.contains('├')).unwrap();
        assert!(failed[divider - 1].contains('…'));
        assert!(failed.iter().any(|r| r.contains("^g f Show full error")));
        assert!(failed.iter().any(|r| r.contains("keep this draft")));

        app.notice = None;
        app.mods[0].closed = true;
        let closed = rows(&screen(&mut app, 48, 30)).join("\n");
        assert!(closed.contains("Closed · work saved") && closed.contains("^g More"));
        assert!(!closed.contains("Newline") && !closed.contains("keep this draft"));
        assert_eq!(app.scroll.input, ratatui::layout::Rect::default());
        app.view = View::NewMod;
        app.input = ui::name_input();
        let new_mod = rows(&screen(&mut app, 48, 30)).join("\n");
        assert!(new_mod.contains("New codemod") && new_mod.contains("↵ Create codemod"));
        assert!(app.scroll.input.height > 0);
    }

    #[test]
    fn open_outline_keeps_focus_and_room_for_dialog_actions() {
        let (_data, mut app, _root) = execution_app();
        for width in [36, 80, 160] {
            screen(&mut app, width, 36);
            assert_eq!(app.scroll.input.x, 4);
            assert_eq!(app.scroll.input.height, 4);
            app.open_actions();
            let buffer = screen(&mut app, width, 36);
            let rendered = rows(&buffer).join("\n");
            assert!(app.scroll.content.width <= 64);
            if width > 80 {
                assert_eq!(
                    buffer[(80, app.scroll.content.y - 1)].bg,
                    ratatui::style::Color::Reset
                );
            }
            assert_eq!(app.scroll.input, ratatui::layout::Rect::default());
            assert!(rendered.contains("keep this draft") && !rendered.contains("Next ·"));
            key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
            assert_eq!(app.input.lines(), ["keep this draft"]);
        }
        app.input.insert_str("\nsecond\nthird\nfourth\nfifth");
        screen(&mut app, 160, 36);
        assert_eq!(app.scroll.input.height, 5);
        let id = app.mods[0].id;
        app.mods[0]
            .queue
            .push(app.store.enqueue(id, "Keep the current icons").unwrap());
        app.view = View::Queue(0);
        let rendered = rows(&screen(&mut app, 160, 36));
        let footer = rendered.iter().find(|r| r.contains("focus")).unwrap();
        assert!(footer.contains("move↑") && footer.contains("move↓") && footer.contains("back"));
    }

    #[test]
    fn open_outline_wraps_tasks_and_prose_without_a_left_gutter() {
        let (_data, mut app, _root) = execution_app();
        app.plan_details = true;
        app.mods[0].execution = None;
        let plan = app.mods[0]
            .planning
            .as_mut()
            .unwrap()
            .plan
            .as_mut()
            .unwrap();
        plan.tasks[0].title =
            "Implement completion while preserving drafts and keyboard focus across refreshes"
                .into();
        plan.tasks[0].outcome =
            "Completion stays available and every unfinished edit remains intact across updates."
                .into();
        for width in [48, 100, 160] {
            app.focus_plan = true;
            let rendered = rows(&screen(&mut app, width, 36));
            let start = rendered
                .iter()
                .position(|r| r.contains("1. Implement"))
                .unwrap();
            let end = rendered
                .iter()
                .position(|r| r.trim_start().starts_with('╭'))
                .unwrap();
            for row in &rendered[start + 1..end] {
                let text = row.trim();
                if !text.is_empty() && !text.starts_with(['─', '◇']) {
                    assert!(row.starts_with("       "), "{row}");
                }
            }
            assert!(
                rendered[start..end]
                    .iter()
                    .all(|row| row.trim_end().chars().count() <= width as usize - 2)
            );
            if width >= 100 {
                assert!(rendered[start].contains("keyboard focus across refreshes"));
                assert!(rendered[start + 1].contains("intact across updates."));
            }
        }
        app.mods[0].messages.push(crate::store::Message {
            source: None,
            task: None,
            item_id: None,
            role: "user".into(),
            body: "café 界 ".repeat(25),
            model: None,
            effort: None,
        });
        let buffer = screen(&mut app, 160, 36);
        let rendered = rows(&buffer);
        let user = rendered.iter().position(|r| r.contains("> café")).unwrap();
        assert!(rendered[user].starts_with("  > "));
        assert!((82..158).any(|x| buffer[(x, user as u16)].symbol() != " "));
        assert_ne!(buffer[(157, user as u16)].bg, ratatui::style::Color::Reset);
        assert_eq!(buffer[(158, user as u16)].bg, ratatui::style::Color::Reset);
    }

    #[test]
    fn conflict_inspection_keeps_paths_owners_and_draft_visible() {
        let (_data, mut app, _root) = execution_app();
        let m = &mut app.mods[0];
        let execution = m.execution.as_mut().unwrap();
        execution.status = "running".into();
        execution.checks.clear();
        let run = &mut execution.tasks[0];
        run.status = "pending".into();
        run.checks.clear();
        run.conflict_retries = 1;
        let mut conflict = crate::conflict::tests::conflict("hello.sh", true);
        conflict.files[0].owners = vec!["one".into()];
        run.conflict = Some(conflict);
        let id = run.id;
        app.auto_runs.insert(m.id);
        assert_eq!(
            app.action_dock().status,
            "Working · resolving task 1 conflicts"
        );
        app.view = View::Task(id, 0);
        let rendered = rows(&screen(&mut app, 100, 40)).join("\n");
        assert!(rendered.contains("Conflicting files"));
        assert!(rendered.contains("hello.sh · text · owners: one"));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn task_inspection_keeps_histories_scoped_and_returns_to_the_draft() {
        let (_data, mut app, _root) = execution_app();
        let m = &mut app.mods[0];
        let plan = m.planning.as_mut().unwrap().plan.as_mut().unwrap();
        let mut second = plan.tasks[0].clone();
        second.id = "two".into();
        second.title = "Build the interface".into();
        second.depends_on = vec!["one".into()];
        plan.tasks.push(second);
        let execution = m.execution.as_mut().unwrap();
        execution.checks.clear();
        execution.status = "blocked".into();
        let first = &mut execution.tasks[0];
        first.worker = Some(12);
        first.provider = Some("muse".into());
        first.selection = Some(crate::router::Selection {
            model: "muse-spark-1.3".into(),
            effort: "high".into(),
            reason: String::new(),
            evidence: None,
        });
        first.status = "blocked".into();
        first.checks[0].exit_code = Some(1);
        first.checks[0].output = "AssertionError: expected greeting".into();
        let first_id = first.id;
        let mut second = first.clone();
        second.id += 1;
        second.task_id = "two".into();
        second.status = "pending".into();
        second.checks.clear();
        let second_id = second.id;
        execution.tasks.push(second);
        for (task, text) in [
            (Some(first_id), "First task update"),
            (Some(second_id), "Second task update"),
            (None, "Legacy worker update"),
        ] {
            m.messages.push(crate::store::Message {
                source: task.and_then(|id| {
                    m.execution
                        .as_ref()?
                        .tasks
                        .iter()
                        .find(|r| r.id == id)
                        .map(|r| r.source.clone())
                }),
                task,
                item_id: None,
                role: "muse:12".into(),
                body: text.into(),
                model: Some("muse-spark-1.3".into()),
                effort: Some("high".into()),
            });
        }
        assert_eq!(app.inspect_task(second_id).unwrap().state, "Waiting");
        assert_eq!(
            app.inspect_task(second_id).unwrap().note,
            "Waiting for task 1."
        );
        assert!(app.action_dock().primary == Some(Action::RetryTask(first_id)));
        app.perform_action(Action::Failure).unwrap();
        assert!(matches!(app.view, View::Task(id, 0) if id == first_id));
        for width in [36, 100, 160] {
            let text = rows(&screen(&mut app, width, 40)).join("\n");
            assert!(text.contains("Check failed") && text.contains("AssertionError:"));
            assert!(text.contains("/usr/bin/true"));
            assert!(!text.contains("Second task update") && !text.contains("Legacy worker update"));
        }
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Tasks(0)));
        let text = rows(&screen(&mut app, 100, 30)).join("\n");
        assert!(text.contains("muse · w12 · muse-spark-1.3 · high"));
        let content = app.scroll.content;
        wheel(&mut app, false, content);
        assert!(matches!(app.view, View::Tasks(1)));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
        let text = rows(&screen(&mut app, 100, 30)).join("\n");
        assert!(text.contains("Second task update"));
        assert!(!text.contains("First task update") && !text.contains("Legacy worker update"));
        key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        let text = rows(&screen(&mut app, 100, 40)).join("\n");
        assert!(text.contains("First task update") && text.contains("Legacy worker update"));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::TaskHistory(id, _) if id == second_id));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Tasks(1)));
        key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(matches!(app.view, View::Chat));
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert!(!app.plan_details);
        assert!(app.current_mod().unwrap().queue.is_empty());
    }

    #[test]
    fn task_inspection_prioritizes_final_failures_and_handles_replaced_plans() {
        let (_data, mut app, _root) = execution_app();
        let execution = app.mods[0].execution.as_mut().unwrap();
        let id = execution.tasks[0].id;
        execution.status = "blocked".into();
        execution.checks[0].task = Some(id);
        execution.checks[0].exit_code = Some(1);
        execution.checks[0].output = "final check failed".into();
        assert_eq!(app.inspect_task(id).unwrap().state, "Final checks failed");
        assert!(app.action_dock().primary == Some(Action::Retry));
        key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(matches!(app.view, View::Checks(0)));
        key(&mut app, KeyCode::Char('1'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        let text = rows(&screen(&mut app, 100, 30)).join("\n");
        assert!(
            text.contains("final check failed")
                && text.replace('\u{a0}', " ").contains("Retry final checks"),
            "{text}"
        );
        app.mods[0].execution.as_mut().unwrap().tasks.clear();
        screen(&mut app, 100, 30);
        assert!(matches!(app.view, View::Tasks(0)));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn network_dialog_preserves_drafts_and_requires_an_explicit_decision() {
        let (data, store, m, root, workers) = crate::network::tests::fixture();
        let project = data.0.join("project");
        let request=crate::network::Request::parse(serde_json::json!({"domains":["huggingface.co","cdn.jsdelivr.net"],"reason":"Verify model weights and browser runtime"})).unwrap();
        store
            .request_network(m.id, workers[0], request, false)
            .unwrap();
        let mut app = App::load(project, false, store).unwrap();
        app.input.insert_str("keep this draft");
        app.refresh_network_requests().unwrap();
        let notice = rows(&screen(&mut app, 120, 36))
            .join("\n")
            .replace('\u{a0}', " ");
        assert!(app.action_dock().primary == Some(Action::Network));
        assert!(
            notice.contains("Needs network access") && notice.contains("ctrl+n Review domains"),
            "{notice}"
        );
        app.handle(Event::Key(KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::CONTROL,
        )))
        .unwrap();
        let dialog = rows(&screen(&mut app, 120, 36))
            .join("\n")
            .replace('\u{a0}', " ");
        assert!(dialog.contains("huggingface.co") && dialog.contains("cdn.jsdelivr.net"));
        assert!(dialog.contains("allow for codemod") && dialog.contains("Verify model weights"));
        let narrow = rows(&screen(&mut app, 80, 24))
            .join("\n")
            .replace('\u{a0}', " ");
        assert!(narrow.contains("huggingface.co") && narrow.contains("cdn.jsdelivr.net"));
        assert!(narrow.contains("allow for codemod") && narrow.contains("d deny"));
        app.handle(Event::Paste("a".into())).unwrap();
        app.handle(Event::Key(KeyEvent::new(
            KeyCode::Enter,
            KeyModifiers::NONE,
        )))
        .unwrap();
        assert!(crate::network::grants(&root).unwrap().is_empty());
        app.handle(Event::Key(KeyEvent::new_with_kind(
            KeyCode::Char('a'),
            KeyModifiers::NONE,
            KeyEventKind::Repeat,
        )))
        .unwrap();
        assert!(crate::network::grants(&root).unwrap().is_empty());
        app.handle(Event::Key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)))
            .unwrap();
        assert!(app.pending_network().is_some());
        app.handle(Event::Key(KeyEvent::new(
            KeyCode::Char('n'),
            KeyModifiers::CONTROL,
        )))
        .unwrap();
        app.handle(Event::Key(KeyEvent::new(
            KeyCode::Char('d'),
            KeyModifiers::NONE,
        )))
        .unwrap();
        assert!(matches!(app.view, View::Chat));
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert!(
            app.pending_network().is_none() && crate::network::grants(&root).unwrap().is_empty()
        );
        assert_eq!(
            app.store.load_project(&app.project).unwrap().mods[0].draft,
            "keep this draft"
        );
    }

    #[test]
    fn long_network_requests_scroll_and_retired_requests_return_to_the_draft() {
        let (data, store, m, _root, workers) = crate::network::tests::fixture();
        let reason = format!("{}ENDREASON", "download verification ".repeat(12));
        let request = crate::network::Request::parse(serde_json::json!({
            "domains": (0..8).map(|i| format!("model-cache-{i}.example.com")).collect::<Vec<_>>(),
            "reason": reason
        }))
        .unwrap();
        store
            .request_network(m.id, workers[0], request, false)
            .unwrap();
        let mut app = App::load(data.0.join("project"), false, store).unwrap();
        app.input.insert_str("keep this draft");
        app.refresh_network_requests().unwrap();
        key(&mut app, KeyCode::Char('n'), KeyModifiers::CONTROL);
        let first = rows(&screen(&mut app, 80, 24))
            .join("\n")
            .replace('\u{a0}', " ");
        assert!(first.contains("model-cache-0.example.com") && first.contains("↑↓ scroll"));
        let View::Network(id, _) = app.view else {
            panic!("Network dialog missing")
        };
        let content = app.scroll.content;
        wheel(&mut app, false, content);
        assert!(matches!(app.view, View::Network(_, scroll) if scroll > 0));
        assert!(app.pending_network().is_some());
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert!(crate::network::grants(&_root).unwrap().is_empty());
        app.view = View::Network(id, u16::MAX);
        let last = rows(&screen(&mut app, 80, 24))
            .join("\n")
            .replace('\u{a0}', " ");
        assert!(
            last.contains("ENDREASON") && last.contains("a allow for codemod"),
            "{last}"
        );
        app.store.follow_up(m.id, "A different task", None).unwrap();
        app.refresh_network_requests().unwrap();
        assert!(matches!(app.view, View::Chat));
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    #[ignore = "Runs Codex and Muse domain requests and retries in a disposable VM"]
    fn both_workers_resume_after_codemod_network_approval() {
        use std::fs;
        let data = TestData::new();
        let project = data.0.join("project");
        fs::create_dir_all(&project).unwrap();
        fs::write(project.join("README.md"), "Disposable network fixture.\n").unwrap();
        let mut store = data.store();
        let project_id = store.load_project(&project).unwrap().id;
        let m = store
            .create_mod(project_id, "Check two independent downloads")
            .unwrap();
        let root = store.workspace_path(m.id).unwrap();
        let tasks=[("a","codex"),("b","muse")].map(|(id,provider)|serde_json::json!({"id":id,"title":format!("{provider} download"),"outcome":format!("Network approval fixture. In {id}.txt write before-approval ONLY if it does not already exist. Run /usr/bin/curl -f -I --max-time 15 https://example.com. If blocked by the proxy allowlist, call request_network_access for example.com with reason Verify fixture download, then return completed:false and keep the source file. On retry, require {id}.txt still contains before-approval. Once example.com returns success, verify https://example.org remains blocked-by-allowlist. Then append exactly allowed to {id}.txt once. Never bypass the network policy or change other source files."),"files":[format!("{id}.txt")],"depends_on":[],"worker":provider,"checks":["Marker retains saved work and records allowed download"]}));
        let plan = crate::plan::Plan::parse(
            &serde_json::json!({"summary":"Two network checks","tasks":tasks}).to_string(),
        )
        .unwrap();
        store
            .save_plan(m.id, &m.planning.as_ref().unwrap().source, &plan)
            .unwrap();
        workspace::create(&project, &root).unwrap();
        store.create_execution(m.id, &root, &plan).unwrap();
        let mut app = App::load(project.clone(), false, store).unwrap();
        app.muse = true;
        app.input.insert_str("keep the composer draft");
        app.start_worker(0, Role::Executor).unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let deadline = Instant::now() + Duration::from_secs(600);
            let mut approved = false;
            let mut previous = String::new();
            while Instant::now() < deadline {
                app.poll_workers().unwrap();
                let state = app.mods[0]
                    .execution
                    .as_ref()
                    .unwrap()
                    .tasks
                    .iter()
                    .map(|t| format!("{}:{}", t.task_id, t.status))
                    .collect::<Vec<_>>()
                    .join(" ");
                if state != previous {
                    eprintln!("Network fixture: {state}");
                    previous = state;
                }
                let pending = app
                    .network_requests
                    .iter()
                    .filter(|r| r.status == "pending")
                    .count();
                if pending == 2 && !approved && !app.workers.values().any(Worker::busy) {
                    assert!(
                        !crate::sandbox::network(&root)
                            .unwrap()
                            .contains_key("example.com")
                    );
                    let screen = rows(&screen(&mut app, 120, 36)).join("\n");
                    assert!(screen.contains("ctrl+n Review domains"));
                    app.handle(Event::Key(KeyEvent::new(
                        KeyCode::Char('n'),
                        KeyModifiers::CONTROL,
                    )))
                    .unwrap();
                    app.handle(Event::Key(KeyEvent::new(
                        KeyCode::Char('a'),
                        KeyModifiers::NONE,
                    )))
                    .unwrap();
                    approved = true;
                    assert_eq!(app.input.lines(), ["keep the composer draft"]);
                }
                assert!(
                    app.workers.values().all(|w| w.status != Status::Failed),
                    "{}",
                    app.worker_error().unwrap_or("")
                );
                if app.mods[0].execution.as_ref().unwrap().status == "review" {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert!(
                approved,
                "Both providers must request access through the dispatcher"
            );
            assert_eq!(
                app.mods[0].execution.as_ref().unwrap().status,
                "review",
                "{}",
                app.worker_error().unwrap_or("")
            );
            for id in ["a", "b"] {
                let text = fs::read_to_string(root.join(format!("work/{id}.txt"))).unwrap();
                assert!(text.contains("before-approval") && text.contains("allowed"));
            }
            assert!(
                app.mods[0]
                    .execution
                    .as_ref()
                    .unwrap()
                    .checks
                    .iter()
                    .all(|c| c.exit_code == Some(0))
            );
            assert_eq!(app.input.lines(), ["keep the composer draft"]);
        }));
        app.workers.clear();
        drop(app);
        crate::sandbox::delete(&root).unwrap();
        if let Err(error) = outcome {
            std::panic::resume_unwind(error);
        }
    }

    #[test]
    fn stopping_and_reopening_do_not_reactivate_the_pool() {
        let (data, store, id, _plan) = crate::scheduler::tests::fixture(&["codex", "muse"], false);
        let mut app = App::load(data.0.join("project"), false, store).unwrap();
        let record = app
            .store
            .worker_provider(id, Role::Executor, 0, "codex")
            .unwrap();
        let mut worker =
            Worker::start(&app.project, &app.mods[0], record, Role::Planner, None).unwrap();
        worker.role = Role::Executor;
        worker.status = Status::Ready;
        app.workers.insert(worker.id, worker);
        app.executing_mods.insert(id);
        app.toggle_worker().unwrap();
        assert!(!app.executing_mods.contains(&id));
        app.poll_workers().unwrap();
        assert!(app.workers.values().all(|w| !w.enabled));
        assert!(
            app.mods[0]
                .execution
                .as_ref()
                .unwrap()
                .tasks
                .iter()
                .all(|r| r.status == "pending")
        );
        drop(app);
        let app = App::load(data.0.join("project"), false, data.store()).unwrap();
        assert!(app.executing_mods.is_empty() && app.workers.is_empty());
    }

    #[test]
    fn an_approved_network_retry_stays_paused_after_stop_or_restart() {
        let (data, mut store, m, _root, workers) = crate::network::tests::fixture();
        let request = crate::network::Request::parse(
            serde_json::json!({"domains":["example.com"],"reason":"Download fixture"}),
        )
        .unwrap();
        store
            .request_network(m.id, workers[0], request, false)
            .unwrap();
        let access = store.network_requests(m.id).unwrap().remove(0);
        let source = m.execution.as_ref().unwrap().tasks[0].source.clone();
        store
            .finish_task(m.id, &source, "blocked", "Needs access", &[])
            .unwrap();
        store.decide_network(m.id, access.id, true).unwrap();
        let mut app = App::load(data.0.join("project"), false, store).unwrap();
        app.refresh_network_requests().unwrap();
        app.resume_network_workers().unwrap();
        assert_eq!(
            app.mods[0].execution.as_ref().unwrap().tasks[0].status,
            "blocked"
        );
        assert!(app.workers.is_empty());
        app.executing_mods.insert(m.id);
        app.resume_network_workers().unwrap();
        assert_eq!(
            app.mods[0].execution.as_ref().unwrap().tasks[0].status,
            "pending"
        );
        assert_eq!(
            app.mods[0].execution.as_ref().unwrap().tasks[0].worker,
            Some(workers[0])
        );
        assert!(
            app.workers.is_empty(),
            "Approval queues work through admission"
        );
        app.executing_mods.remove(&m.id);
        app.poll_workers().unwrap();
        assert!(app.workers.is_empty());
    }

    #[test]
    #[ignore = "Runs two Codex then two Muse tasks through the scheduler in a disposable VM"]
    fn worker_slots_follow_provider_waves_in_vm() {
        let (data, mut store, id, mut plan) =
            crate::scheduler::tests::fixture(&["codex", "codex", "muse", "muse"], true);
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("README.md"), "Disposable scheduler fixture.\n").unwrap();
        let root = store.execution(id).unwrap().unwrap().workspace;
        workspace::create(&project, &root).unwrap();
        for (i, task) in plan.tasks.iter_mut().enumerate() {
            task.outcome = format!(
                "Write exactly task{i} followed by a newline in {i}.txt. Run /bin/sleep 8 once before writing, to exercise concurrent execution. Use /bin/sh for the file check. Do not install dependencies or change any other source."
            );
        }
        let source = store.planning(id).unwrap().unwrap().source;
        store.save_plan(id, &source, &plan).unwrap();
        let mut app = App::load(project, false, store).unwrap();
        app.muse = true;
        app.start_worker(0, Role::Executor).unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let deadline = Instant::now() + Duration::from_secs(480);
            let mut overlap = [false; 2];
            let mut previous = String::new();
            while Instant::now() < deadline {
                app.poll_workers().unwrap();
                assert!(
                    app.workers
                        .values()
                        .filter(|w| w.role == Role::Executor && w.busy())
                        .count()
                        <= 2
                );
                let execution = app.mods[0].execution.as_ref().unwrap();
                let state = execution
                    .tasks
                    .iter()
                    .map(|r| format!("{}:{}", r.task_id, r.status))
                    .collect::<Vec<_>>()
                    .join(" ");
                if state != previous {
                    eprintln!("Scheduler fixture: {state}");
                    previous = state;
                }
                for (wave, pair) in execution.tasks.chunks(2).enumerate() {
                    overlap[wave] |= pair.iter().all(|r| r.status == "running");
                }
                assert!(
                    app.workers.values().all(|w| w.status != Status::Failed),
                    "{}",
                    app.worker_error().unwrap_or("")
                );
                if execution.status == "review" {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            assert_eq!(
                app.mods[0].execution.as_ref().unwrap().status,
                "review",
                "{}",
                app.worker_error().unwrap_or("")
            );
            assert_eq!(overlap, [true, true], "Both pairs must actually overlap");
            let records = app.store.executors(id).unwrap();
            assert_eq!(records.iter().filter(|r| r.provider == "codex").count(), 2);
            assert_eq!(records.iter().filter(|r| r.provider == "muse").count(), 2);
            for i in 0..4 {
                assert_eq!(
                    std::fs::read_to_string(root.join(format!("work/{i}.txt"))).unwrap(),
                    format!("task{i}\n")
                );
            }
            assert!(
                app.mods[0]
                    .execution
                    .as_ref()
                    .unwrap()
                    .checks
                    .iter()
                    .all(|c| c.exit_code == Some(0))
            );
        }));
        app.workers.clear();
        drop(app);
        crate::sandbox::delete(&root).unwrap();
        if let Err(error) = outcome {
            std::panic::resume_unwind(error);
        }
    }

    #[test]
    #[ignore = "Runs automatic Codex/Muse assignment and integration in a disposable VM"]
    fn automatic_provider_assignment_runs_in_vm() {
        let (data, mut store, id, mut plan) =
            crate::scheduler::tests::fixture(&["auto", "auto", "auto"], true);
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(
            project.join("README.md"),
            "Disposable assignment fixture.\n",
        )
        .unwrap();
        let root = store.execution(id).unwrap().unwrap().workspace;
        workspace::create(&project, &root).unwrap();
        for (i, task) in plan.tasks.iter_mut().take(2).enumerate() {
            task.outcome = format!(
                "Write exactly task{i} followed by a newline in {i}.txt. Run /bin/sleep 8 before writing to exercise concurrency. Use /bin/sh for checks; no dependencies or other source changes are needed."
            );
        }
        plan.tasks[2].files.clear();
        plan.tasks[2].outcome = "Verify 0.txt contains exactly task0 and 1.txt exactly task1, both followed by a newline. Do not modify source files. Use /bin/sh; no dependencies are needed.".into();
        plan.tasks[2].checks = vec!["Both files contain the expected text".into()];
        let source = store.planning(id).unwrap().unwrap().source;
        store.save_plan(id, &source, &plan).unwrap();
        let mut app = App::load(project, false, store).unwrap();
        app.muse = true;
        app.start_worker(0, Role::Executor).unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let started = Instant::now();
            let mut overlap = false;
            let mut previous = String::new();
            while started.elapsed() < Duration::from_secs(480) {
                app.poll_workers().unwrap();
                let execution = app.mods[0].execution.as_ref().unwrap();
                overlap |= execution.tasks[..2].iter().all(|r| r.status == "running");
                let state = execution
                    .tasks
                    .iter()
                    .map(|r| format!("{}:{}:{:?}", r.task_id, r.status, r.provider))
                    .collect::<Vec<_>>()
                    .join(" ");
                if state != previous {
                    eprintln!("Automatic assignment: {state}");
                    previous = state;
                }
                assert!(
                    app.workers.values().all(|w| w.status != Status::Failed),
                    "{}",
                    app.worker_error().unwrap_or("")
                );
                if execution.status == "review" {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let execution = app.mods[0].execution.as_ref().unwrap();
            assert_eq!(
                execution.status,
                "review",
                "{}",
                app.worker_error().unwrap_or("")
            );
            assert!(overlap, "Implementation tasks must overlap");
            assert_eq!(execution.tasks[0].provider.as_deref(), Some("codex"));
            assert_eq!(execution.tasks[1].provider.as_deref(), Some("muse"));
            assert_eq!(execution.checks.len(), 3);
            assert!(execution.checks.iter().all(|c| c.exit_code == Some(0)));
            assert!(
                execution
                    .tasks
                    .iter()
                    .all(|r| r.assignment_reason.starts_with("equally suitable"))
            );
            eprintln!(
                "Automatic assignment: 3/3 tasks and checks passed in {:.1}s",
                started.elapsed().as_secs_f64()
            );
        }));
        app.workers.clear();
        drop(app);
        crate::sandbox::delete(&root).unwrap();
        if let Err(error) = outcome {
            std::panic::resume_unwind(error);
        }
    }

    #[test]
    #[ignore = "Recovers real failed checks with Codex and Muse in a disposable VM"]
    fn verification_feedback_recovers_both_vm_workers() {
        use std::sync::atomic::AtomicBool;
        let (data, mut store, id, mut plan) =
            crate::scheduler::tests::fixture(&["auto", "auto"], false);
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("check.sh"), "printf '%s\\n' \"$2\" | cmp -s - \"$1\" || { echo 'AssertionError: Saved result is incorrect'; exit 1; }\nprintf 'check passed\\n'\n").unwrap();
        for (i, task) in plan.tasks.iter_mut().enumerate() {
            std::fs::write(project.join(format!("{i}.txt")), "incorrect\n").unwrap();
            task.outcome = format!(
                "Write exactly task{i} followed by a newline in {i}.txt. Use the existing immutable check.sh to verify it. Do not modify check.sh. No dependencies are needed."
            );
        }
        let source = store.planning(id).unwrap().unwrap().source;
        store.save_plan(id, &source, &plan).unwrap();
        let root = store.execution(id).unwrap().unwrap().workspace;
        workspace::create(&project, &root).unwrap();
        let records = store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let flag = AtomicBool::new(false);
            let mut vm = crate::sandbox::Sandbox::prepare(&root, &flag, |_| {}).unwrap();
            let runs = store.execution(id).unwrap().unwrap().tasks;
            vm.prepare_tasks(&runs.iter().map(|run| run.id).collect::<Vec<_>>(), &flag)
                .unwrap();
            for (i, run) in runs.iter().enumerate() {
                let record = records
                    .iter()
                    .find(|record| Some(record.id) == run.worker)
                    .unwrap();
                vm.assign_task(run.id, record.id, &flag).unwrap();
                let check = crate::execution::Check {
                    timeout_seconds: None,
                    task: Some(run.id),
                    check: plan.tasks[i].checks[0].clone(),
                    command: vec![
                        "/bin/sh".into(),
                        format!("/tasks/{}/check.sh", run.id),
                        format!("/tasks/{}/{i}.txt", run.id),
                        format!("task{i}"),
                    ],
                };
                let (_, results) = vm.verify_execution(&run.source, &[check], &flag).unwrap();
                assert_eq!(results[0].exit_code, Some(1), "{}", results[0].output);
                store
                    .finish_task(
                        id,
                        &run.source,
                        "blocked",
                        "Worker claimed success",
                        &results,
                    )
                    .unwrap();
                assert!(
                    store
                        .recover_verification(id, &run.source, record.id)
                        .unwrap()
                );
            }
            drop(vm);
            let mut app = App::load(project.clone(), false, store).unwrap();
            app.muse = true;
            app.start_worker(0, Role::Executor).unwrap();
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(300) {
                app.poll_workers().unwrap();
                assert!(
                    app.workers
                        .values()
                        .all(|worker| worker.status != Status::Failed),
                    "{}",
                    app.worker_error().unwrap_or("")
                );
                if app.mods[0].execution.as_ref().unwrap().status == "review" {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let execution = app.mods[0].execution.as_ref().unwrap();
            assert_eq!(
                execution.status,
                "review",
                "{}",
                app.worker_error().unwrap_or("")
            );
            assert_eq!(execution.checks.len(), 2);
            assert!(
                execution
                    .checks
                    .iter()
                    .all(|check| check.exit_code == Some(0))
            );
            assert!(
                execution
                    .tasks
                    .iter()
                    .zip(&runs)
                    .all(|(now, before)| now.worker == before.worker
                        && now.verification_feedback.is_empty())
            );
            assert_eq!(
                std::fs::read(root.join("work/check.sh")).unwrap(),
                std::fs::read(project.join("check.sh")).unwrap()
            );
            app.workers.clear();
            eprintln!("Codex and Muse recovered their own failures; combined checks passed.");
        }));
        crate::sandbox::delete(&root).unwrap();
        if let Err(error) = outcome {
            std::panic::resume_unwind(error);
        }
    }

    #[test]
    #[ignore = "Resolves real text conflicts with Codex and Muse in a disposable VM"]
    fn merge_conflicts_recover_both_vm_workers() {
        use std::sync::atomic::AtomicBool;
        let (data, mut store, id, mut plan) =
            crate::scheduler::tests::fixture(&["auto", "auto"], false);
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        std::fs::write(project.join("check.sh"), "printf 'upstream%s\\nworker%s\\n' \"$2\" \"$2\" | cmp -s - \"$1\" || { echo 'AssertionError: Both additions must be preserved'; exit 1; }\nprintf 'check passed\\n'\n").unwrap();
        for (i, task) in plan.tasks.iter_mut().enumerate() {
            std::fs::write(project.join(format!("{i}.txt")), "base\n").unwrap();
            task.outcome = format!(
                "Resolve {i}.txt preserving upstream{i} and worker{i}, in that order, each on its own line. Verify with /bin/sh check.sh {i}.txt {i}. Do not edit check.sh. No dependencies are needed."
            );
        }
        let source = store.planning(id).unwrap().unwrap().source;
        store.save_plan(id, &source, &plan).unwrap();
        let root = store.execution(id).unwrap().unwrap().workspace;
        workspace::create(&project, &root).unwrap();
        store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let flag = AtomicBool::new(false);
            let mut vm = crate::sandbox::Sandbox::prepare(&root, &flag, |_| {}).unwrap();
            let runs = store.execution(id).unwrap().unwrap().tasks;
            vm.prepare_tasks(&runs.iter().map(|run| run.id).collect::<Vec<_>>(), &flag)
                .unwrap();
            for run in &runs {
                vm.assign_task(run.id, run.worker.unwrap(), &flag).unwrap();
            }
            vm.guest(&["/bin/sh", "-c", "set -eu; cd /workspace; printf 'upstream0\\n' > 0.txt; printf 'upstream1\\n' > 1.txt; git add .; git -c user.name=Sprowt -c user.email=sprowt@localhost commit -m upstream"], &flag).unwrap();
            for (i, run) in runs.iter().enumerate() {
                vm.assign_task(run.id, run.worker.unwrap(), &flag).unwrap();
                vm.guest(
                    &[
                        "/bin/sh",
                        "-c",
                        &format!("printf 'worker{i}\\n' > /tasks/{}/{i}.txt", run.id),
                    ],
                    &flag,
                )
                .unwrap();
                let error = vm.integrate_task(run.id, &flag).unwrap_err();
                let conflict = error
                    .get_ref()
                    .unwrap()
                    .downcast_ref::<crate::conflict::MergeConflict>()
                    .unwrap()
                    .clone();
                store
                    .0
                    .execute(
                        "UPDATE task_runs SET status='checking' WHERE id=?1",
                        [run.id],
                    )
                    .unwrap();
                assert_eq!(
                    store
                        .record_conflict(id, &run.source, run.worker.unwrap(), conflict, true)
                        .unwrap(),
                    Some(true)
                );
            }
            drop(vm);
            let mut app = App::load(project.clone(), false, store).unwrap();
            app.muse = true;
            app.start_worker(0, Role::Executor).unwrap();
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(300) {
                app.poll_workers().unwrap();
                assert!(
                    app.workers
                        .values()
                        .all(|worker| worker.status != Status::Failed && worker.error.is_none()),
                    "{}",
                    app.worker_error().unwrap_or("")
                );
                if app.mods[0].execution.as_ref().unwrap().status == "review" {
                    break;
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let execution = app.mods[0].execution.as_ref().unwrap();
            assert_eq!(
                execution.status,
                "review",
                "{}",
                app.worker_error().unwrap_or("")
            );
            assert_eq!(execution.checks.len(), 2);
            assert!(
                execution
                    .checks
                    .iter()
                    .all(|check| check.exit_code == Some(0))
            );
            for (i, (now, before)) in execution.tasks.iter().zip(&runs).enumerate() {
                assert_eq!(now.worker, before.worker);
                assert_eq!(now.conflict_retries, 1);
                assert!(now.conflict.is_none());
                assert_eq!(
                    std::fs::read_to_string(root.join(format!("work/{i}.txt"))).unwrap(),
                    format!("upstream{i}\nworker{i}\n")
                );
            }
            assert_eq!(
                std::fs::read(root.join("work/check.sh")).unwrap(),
                std::fs::read(project.join("check.sh")).unwrap()
            );
            app.workers.clear();
            eprintln!(
                "Codex and Muse resolved their own conflicts; combined checks passed in {:.1}s.",
                started.elapsed().as_secs_f64()
            );
        }));
        crate::sandbox::delete(&root).unwrap();
        if let Err(error) = outcome {
            std::panic::resume_unwind(error);
        }
    }

    #[test]
    fn review_findings_wait_for_explicit_fixes_and_preserve_navigation() {
        let (data, mut store, mut m, _) = crate::review::tests::fixture();
        store
            .begin_review(
                m.id,
                m.execution.as_ref().unwrap().fingerprint.as_ref().unwrap(),
            )
            .unwrap();
        m.agent_review = store.review_state(m.id).unwrap();
        let source = m.agent_review.as_ref().unwrap().source.clone();
        store
            .finish_review(&m, &source, &crate::review::tests::finding())
            .unwrap();
        let project = data.0.join("project");
        let mut app = App::load(project.clone(), false, store).unwrap();
        app.maintain_reviews().unwrap();
        assert_eq!(
            app.mods[0].agent_review.as_ref().unwrap().status,
            "findings"
        );
        assert!(app.mods[0].execution.as_ref().unwrap().complete());
        assert!(app.executing_mods.is_empty());
        drop(app);
        let mut app = App::load(project, false, data.store()).unwrap();
        app.maintain_reviews().unwrap();
        assert_eq!(app.mods[0].agent_review.as_ref().unwrap().rounds, 0);
        app.input.insert_str("keep my draft");
        assert!(app.action_dock().primary == Some(Action::FixIssues));
        assert_eq!(app.action_dock().status, "Review · 1 open issue");
        key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('i'), KeyModifiers::NONE);
        assert!(matches!(app.view, View::Findings(0)));
        let contents = rows(&screen(&mut app, 90, 30)).join("\n");
        assert!(contents.contains("P2  Reject empty names"));
        assert!(contents.contains("greet.sh:2 · runtime") && contents.contains("Open"));
        assert!(contents.contains("Evidence") && contents.contains("Proposed fix"));
        assert!(contents.contains("x\u{a0}fix\u{a0}issues"), "{contents}");
        // A narrow pane scrolls with the mouse and clamps after resizing.
        let first = screen(&mut app, 48, 20);
        let content = app.scroll.content;
        wheel(&mut app, false, content);
        assert_ne!(first, screen(&mut app, 48, 20));
        assert!(matches!(app.view, View::Findings(offset) if offset > 0));
        screen(&mut app, 120, 50);
        assert!(matches!(app.view, View::Findings(0)));
        key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
        assert!(matches!(app.view, View::History(0)));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Findings(0)));
        key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(app.mods[0].agent_review.as_ref().unwrap().status, "fixing");
        assert_eq!(app.mods[0].agent_review.as_ref().unwrap().rounds, 1);
        assert!(app.executing_mods.contains(&m.id));
        let runs = &app.mods[0].execution.as_ref().unwrap().tasks;
        assert!(runs.iter().all(|run| run.status == "pending"));
        assert!(
            runs[0]
                .review_feedback
                .as_ref()
                .unwrap()
                .contains("Reject empty names")
        );
        assert!(
            rows(&screen(&mut app, 90, 30))
                .join("\n")
                .contains("Fix queued")
        );
        key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
        assert_eq!(app.mods[0].agent_review.as_ref().unwrap().rounds, 1);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Chat));
        assert_eq!(app.input.lines(), ["keep my draft"]);
        let record = app.store.worker_for(m.id, Role::Planner).unwrap();
        let mut worker =
            Worker::start(&app.project, &app.mods[0], record, Role::Planner, None).unwrap();
        worker.role = Role::Executor;
        worker.status = Status::Running;
        app.workers.insert(worker.id, worker);
        assert!(
            app.action_dock()
                .status
                .starts_with("Working · fixing review issues")
        );
        let task = &app.mods[0]
            .planning
            .as_ref()
            .unwrap()
            .plan
            .as_ref()
            .unwrap()
            .tasks[0];
        assert!(task.checks[0].starts_with("Review regression 1"));
        let rendered = rows(&screen(&mut app, 100, 30)).join("\n");
        assert!(rendered.contains("Working · fixing review issues"));
        assert!(rendered.contains("0/1 issues checked"));
        assert_eq!(app.input.lines(), ["keep my draft"]);
    }

    #[test]
    fn outdated_or_exhausted_reviews_cannot_dispatch_fixes() {
        for reason in ["queue", "source", "rounds"] {
            let (data, mut store, mut m, _) = crate::review::tests::fixture();
            store
                .begin_review(
                    m.id,
                    m.execution.as_ref().unwrap().fingerprint.as_ref().unwrap(),
                )
                .unwrap();
            m.agent_review = store.review_state(m.id).unwrap();
            let source = m.agent_review.as_ref().unwrap().source.clone();
            store
                .finish_review(&m, &source, &crate::review::tests::finding())
                .unwrap();
            let mut app = App::load(data.0.join("project"), false, store).unwrap();
            app.perform_action(Action::Findings).unwrap();
            match reason {
                "queue" => {
                    let message = app.store.enqueue(m.id, "Change the request").unwrap();
                    app.mods[0].queue.push(message);
                    app.maintain_reviews().unwrap();
                }
                "source" => {
                    std::fs::write(
                        m.execution
                            .as_ref()
                            .unwrap()
                            .workspace
                            .join("work/greet.sh"),
                        "changed source\n",
                    )
                    .unwrap();
                }
                _ => {
                    app.store
                        .0
                        .execute("UPDATE reviews SET rounds=2 WHERE mod_id=?1", [m.id])
                        .unwrap();
                    app.mods[0].agent_review = app.store.review_state(m.id).unwrap();
                }
            }
            key(&mut app, KeyCode::Char('x'), KeyModifiers::NONE);
            assert!(app.mods[0].execution.as_ref().unwrap().complete());
            assert!(app.executing_mods.is_empty());
            let contents = rows(&screen(&mut app, 100, 30)).join("\n");
            assert!(contents.contains("Reject empty names"));
            assert!(contents.contains(if reason == "rounds" {
                "Fix limit reached"
            } else {
                "Outdated"
            }));
            assert!(!contents.contains("x\u{a0}fix\u{a0}issues"));
        }
    }

    #[test]
    fn review_status_is_compact_details_are_explicit_and_edits_invalidate_it() {
        let (data, mut store, mut m, _plan) = crate::review::tests::fixture();
        store
            .begin_review(
                m.id,
                m.execution.as_ref().unwrap().fingerprint.as_ref().unwrap(),
            )
            .unwrap();
        m.agent_review = store.review_state(m.id).unwrap();
        let source = m.agent_review.as_ref().unwrap().source.clone();
        store.finish_review(&m,&source,r#"{"status":"clean","summary":"Inspected greeting and usage; no findings.","findings":[]}"#).unwrap();
        let mut app = App::load(data.0.join("project"), false, store).unwrap();
        app.input.insert_str("keep draft");
        let compact = rows(&screen(&mut app, 120, 36)).join("\n");
        assert!(compact.contains("Publish PR") && compact.contains("review passed"));
        assert!(app.action_dock().primary == Some(Action::Publish));
        assert!(
            app.action_dock()
                .actions
                .iter()
                .any(|a| a.action == Action::Review)
        );
        assert!(!compact.contains("Inspected greeting"));
        app.plan_details = true;
        let expanded = rows(&screen(&mut app, 120, 36)).join("\n");
        assert!(expanded.contains("Inspected greeting"));
        assert_eq!(app.input.lines().join("\n"), "keep draft");
        let id = app.mods[0].id;
        app.mods[0]
            .queue
            .push(app.store.enqueue(id, "Request changes").unwrap());
        app.maintain_reviews().unwrap();
        assert_eq!(app.mods[0].agent_review.as_ref().unwrap().status, "stale");
        assert!(
            rows(&screen(&mut app, 120, 36))
                .join("\n")
                .contains("review outdated")
        );
    }

    #[test]
    fn stopping_a_review_before_dispatch_preserves_ready_changes_and_draft() {
        let (data, store, _, _) = crate::review::tests::fixture();
        let mut app = App::load(data.0.join("project"), false, store).unwrap();
        let id = app.mods[0].id;
        let fingerprint = app.mods[0]
            .execution
            .as_ref()
            .unwrap()
            .fingerprint
            .clone()
            .unwrap();
        app.store.begin_review(id, &fingerprint).unwrap();
        app.mods[0].agent_review = app.store.review_state(id).unwrap();
        let record = app.store.worker_for(id, Role::Reviewer).unwrap();
        let mut worker =
            Worker::start(&app.project, &app.mods[0], record, Role::Planner, None).unwrap();
        worker.role = Role::Reviewer;
        worker.status = Status::Ready;
        app.workers.insert(worker.id, worker);
        app.input.insert_str("keep draft");
        app.toggle_worker().unwrap();
        assert!(app.workers.values().all(|w| !w.enabled));
        assert_eq!(app.mods[0].agent_review.as_ref().unwrap().status, "paused");
        assert!(app.mods[0].execution.as_ref().unwrap().complete());
        assert!(app.version_ready());
        assert_eq!(app.input.lines().join("\n"), "keep draft");
    }

    #[test]
    #[ignore = "Reviews a seeded bug, repairs its owner and reviews again in a disposable VM"]
    fn independent_review_repairs_a_seeded_bug_and_rechecks_in_vm() {
        use std::sync::atomic::AtomicBool;
        let (data, mut store, m, plan) = crate::review::tests::fixture();
        let root = m.execution.as_ref().unwrap().workspace.clone();
        let project = data.0.join("project");
        let original = workspace::source_state(&project).unwrap();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            let flag = AtomicBool::new(false);
            let mut vm =
                crate::sandbox::Sandbox::prepare(&root, &flag, |label| eprintln!("{label}"))
                    .unwrap();
            let runs = m.execution.as_ref().unwrap().tasks.clone();
            vm.prepare_tasks(&runs.iter().map(|r| r.id).collect::<Vec<_>>(), &flag)
                .unwrap();
            let mut final_checks = Vec::new();
            for run in &runs {
                vm.assign_task(run.id, run.worker.unwrap(), &flag).unwrap();
                let checks: Vec<_> = run
                    .checks
                    .iter()
                    .map(|r| crate::execution::Check {
                        timeout_seconds: None,
                        task: Some(run.id),
                        check: r.check.clone(),
                        command: r.command.clone(),
                    })
                    .collect();
                let (_, results) = vm.verify_execution(&run.source, &checks, &flag).unwrap();
                assert!(results.iter().all(|r| r.exit_code == Some(0)));
                store
                    .finish_task(m.id, &run.source, "done", "Smoke checks pass", &results)
                    .unwrap();
                final_checks.extend(checks);
            }
            let (_, checks) = vm
                .verify_execution(&format!("final:{}", m.id), &final_checks, &flag)
                .unwrap();
            assert!(checks.iter().all(|r| r.exit_code == Some(0)));
            let fingerprint = workspace::review(&root).unwrap().fingerprint;
            store
                .execution_checks(m.id, "review", &checks, Some(&fingerprint))
                .unwrap();
            store
                .0
                .execute(
                    "UPDATE executions SET backend='apple-container' WHERE mod_id=?1",
                    [m.id],
                )
                .unwrap();
            drop(vm);
            let mut app = App::load(project.clone(), false, store).unwrap();
            app.executing_mods.insert(m.id);
            app.schedule_execution(0).unwrap();
            app.maintain_reviews().unwrap();
            let mut saw_fix = false;
            let mut previous = String::new();
            let started = Instant::now();
            while started.elapsed() < Duration::from_secs(360) {
                app.poll_workers().unwrap();
                let state = app.mods[0].agent_review.as_ref().unwrap();
                saw_fix |= state.status == "fixing";
                if previous != state.status {
                    eprintln!(
                        "Review: {} · {}",
                        state.status,
                        state.report.as_ref().map_or("", |r| r.summary.as_str())
                    );
                    previous = state.status.clone();
                }
                if state.status == "clean"
                    || state.status == "blocked"
                    || state.status == "findings" && state.rounds >= 2
                {
                    break;
                }
                assert!(
                    app.workers.values().all(|w| w.status != Status::Failed),
                    "{}",
                    app.worker_error().unwrap_or("")
                );
                if state.status == "findings" {
                    app.perform_action(Action::FixIssues).unwrap();
                }
                std::thread::sleep(Duration::from_millis(50));
            }
            let state = app.mods[0].agent_review.as_ref().unwrap();
            assert!(
                saw_fix,
                "Review did not identify the seeded defect: {} · {}",
                state.status,
                state.report.as_ref().map_or_else(
                    || app.worker_error().unwrap_or("").to_owned(),
                    |r| r.summary.clone()
                )
            );
            assert_eq!(
                state.status,
                "clean",
                "{}",
                state.report.as_ref().map_or_else(
                    || app.worker_error().unwrap_or("").to_owned(),
                    |r| r.summary.clone()
                )
            );
            let execution = app.mods[0].execution.as_ref().unwrap();
            assert!(
                execution.complete() && execution.checks.iter().all(|r| r.exit_code == Some(0))
            );
            assert_eq!(execution.tasks[0].worker, runs[0].worker);
            assert_eq!(
                app.mods[0]
                    .planning
                    .as_ref()
                    .unwrap()
                    .plan
                    .as_ref()
                    .unwrap()
                    .tasks[0]
                    .files,
                plan.tasks[0].files
            );
            let source = std::fs::read_to_string(root.join("work/greet.sh")).unwrap();
            assert!(source.contains("exit") || source.contains("return"));
            assert_eq!(workspace::source_state(&project).unwrap(), original);
            app.workers.clear();
            eprintln!(
                "Independent reviewer found the bug; its owner repaired it, checks and fresh review passed."
            );
        }));
        crate::sandbox::delete(&root).unwrap();
        if let Err(error) = result {
            std::panic::resume_unwind(error);
        }
    }

    #[test]
    fn verified_work_starts_one_review_and_a_deliberate_pause_survives_restart() {
        let (data, mut app, _) = execution_app();
        let id = app.mods[0].id;
        // Loading a saved version is passive; completing an active run starts review.
        assert!(!app.review_due(0));
        app.executing_mods.insert(id);
        app.schedule_execution(0).unwrap();
        assert!(app.review_due(0));
        app.maintain_reviews().unwrap();
        let source = app.mods[0].agent_review.as_ref().unwrap().source.clone();
        assert!(!app.auto_reviews.contains(&id));
        app.maintain_reviews().unwrap();
        assert_eq!(app.mods[0].agent_review.as_ref().unwrap().source, source);
        assert_eq!(
            app.workers
                .values()
                .filter(|w| w.role == Role::Reviewer)
                .count(),
            1
        );
        assert_eq!(app.state_summary().phase, state::Phase::Reviewing);
        assert!(app.action_dock().primary.is_none());
        key(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(app.mods[0].user_paused);
        assert_eq!(app.state_summary().phase, state::Phase::Paused);
        let project = app.project.clone();
        drop(app);
        let mut app = App::load(project, false, data.store()).unwrap();
        app.maintain_reviews().unwrap();
        assert!(app.workers.is_empty() && app.mods[0].user_paused);
        assert_eq!(app.action_dock().status, "Paused by you");
        assert!(app.action_dock().primary == Some(Action::Review));
        assert_eq!(app.input.lines(), ["keep this draft"]);
        app.perform_action(Action::Review).unwrap();
        assert!(!app.mods[0].user_paused);
        assert_ne!(app.mods[0].agent_review.as_ref().unwrap().source, source);
    }

    #[test]
    fn resumed_planning_restores_continuation_and_publishing_shows_review_state() {
        let (_data, mut app, _) = execution_app();
        app.view = View::Publish;
        let display = rows(&screen(&mut app, 100, 40)).join("\n");
        assert!(display.contains("Checks 1/1 passed") && display.contains("Review not started"));
        app.view = View::Chat;
        app.mods[0].execution = None;
        app.mods[0].planning.as_mut().unwrap().status = "pending".into();
        app.start_worker(0, Role::Planner).unwrap();
        let id = app.mods[0].id;
        assert!(app.auto_runs.contains(&id));
        app.toggle_worker().unwrap();
        assert!(app.mods[0].user_paused && !app.auto_runs.contains(&id));
        app.toggle_worker().unwrap();
        assert!(!app.mods[0].user_paused && app.auto_runs.contains(&id));
    }

    #[test]
    fn automatic_review_respects_version_decisions_queues_and_pause() {
        let (_data, mut app, root) = execution_app();
        let id = app.mods[0].id;
        app.auto_reviews.insert(id);
        assert!(app.review_due(0));
        for view in [
            View::Publish,
            View::Review(0),
            View::DeleteMod(0),
            View::CloseMod(0),
        ] {
            app.view = view;
            assert!(!app.review_due(0));
        }
        app.view = View::Checks(0);
        assert!(app.review_due(0));
        app.mods[0]
            .queue
            .push(app.store.enqueue(id, "an edit").unwrap());
        assert!(!app.review_due(0));
        app.mods[0].queue.clear();
        app.mods[0].steering.push("pending delivery".into());
        assert!(!app.review_due(0));
        app.mods[0].steering.clear();
        app.mods[0].closed = true;
        assert!(!app.review_due(0));
        app.mods[0].closed = false;
        app.set_paused(0, true).unwrap();
        assert!(!app.review_due(0));
        app.set_paused(0, false).unwrap();
        std::fs::write(root.join("work/hello.sh"), "changed after checks\n").unwrap();
        app.maintain_reviews().unwrap();
        assert!(app.workers.is_empty() && app.mods[0].agent_review.is_none());
        assert_eq!(app.mods[0].execution.as_ref().unwrap().status, "blocked");
        assert!(!app.review_due(0));
    }

    #[test]
    fn current_review_outcomes_never_loop_automatically() {
        let (_data, mut app, _) = execution_app();
        let m = &mut app.mods[0];
        let id = m.id;
        m.agent_review = Some(crate::review::State {
            source: "review:1".into(),
            plan_source: m.planning.as_ref().unwrap().source.clone(),
            fingerprint: m.execution.as_ref().unwrap().fingerprint.clone().unwrap(),
            status: "clean".into(),
            rounds: 0,
            report: None,
        });
        app.auto_reviews.insert(id);
        for status in [
            "clean", "findings", "blocked", "paused", "pending", "running",
        ] {
            app.mods[0].agent_review.as_mut().unwrap().status = status.into();
            assert!(!app.review_due(0), "{status}");
        }
        app.mods[0].agent_review.as_mut().unwrap().status = "fixing".into();
        assert!(app.review_due(0));
        app.auto_reviews.clear();
        assert!(!app.review_due(0));
        app.auto_reviews.insert(id);
        app.mods[0].agent_review.as_mut().unwrap().status = "stale".into();
        assert!(app.review_due(0));
    }

    #[test]
    fn implementation_checks_and_review_have_distinct_truthful_states() {
        let (_data, mut app, _) = execution_app();
        let id = app.mods[0].id;
        let e = app.mods[0].execution.as_mut().unwrap();
        e.status = "verifying".into();
        app.executing_mods.insert(id);
        assert_eq!(app.state_summary().phase, state::Phase::Checking);
        assert!(app.action_dock().primary.is_none());
        assert!(!app.action_dock().evidence.contains("Review clear"));
        app.executing_mods.clear();
        let e = app.mods[0].execution.as_mut().unwrap();
        e.status = "blocked".into();
        e.checks[0].exit_code = Some(1);
        e.checks[0].output = "AssertionError: current failure".into();
        assert_eq!(app.state_summary().phase, state::Phase::NeedsYou);
        assert!(app.action_dock().primary == Some(Action::Retry));
        assert!(app.action_dock().evidence.contains("1 failed"));
        let e = app.mods[0].execution.as_mut().unwrap();
        e.status = "review".into();
        e.checks[0].exit_code = Some(0);
        e.checks[0].output.clear();
        assert_eq!(app.state_summary().phase, state::Phase::Paused);
        assert!(app.action_dock().primary == Some(Action::Review));
        let m = &mut app.mods[0];
        m.agent_review = Some(crate::review::State {
            source: "review:1".into(),
            plan_source: m.planning.as_ref().unwrap().source.clone(),
            fingerprint: m.execution.as_ref().unwrap().fingerprint.clone().unwrap(),
            status: "clean".into(),
            rounds: 0,
            report: None,
        });
        assert_eq!(app.state_summary().phase, state::Phase::Ready);
        assert!(app.action_dock().primary == Some(Action::Publish));
        app.mods[0].execution.as_mut().unwrap().fingerprint = None;
        assert!(app.action_dock().evidence.contains("Review outdated"));
        assert!(!app.action_dock().evidence.contains("Review clear"));
    }

    #[test]
    fn details_tabs_evidence_and_mouse_scroll_preserve_the_draft() {
        let (_data, mut app, _) = execution_app();
        let e = app.mods[0].execution.as_mut().unwrap();
        e.checks[0].output = "A saved successful check output".into();
        for width in [36, 80, 120] {
            app.view = View::Chat;
            key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
            assert!(matches!(app.view, View::Tasks(0)));
            key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
            assert!(matches!(app.view, View::Checks(0)));
            let text = rows(&screen(&mut app, width, 40)).join("\n");
            assert!(
                text.contains("Checks") && text.contains("Review") && text.contains("Activity")
            );
            assert!(!text.contains("/usr/bin/true"));
            key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
            let text = rows(&screen(&mut app, width, 40)).join("\n");
            assert!(text.contains("/usr/bin/true") && text.contains("saved successful"));
            key(&mut app, KeyCode::Char('3'), KeyModifiers::NONE);
            assert!(matches!(app.view, View::Findings(0)));
            key(&mut app, KeyCode::Char('4'), KeyModifiers::NONE);
            assert!(matches!(app.view, View::History(0)));
            key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
            assert!(matches!(app.view, View::Findings(0)));
            key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
            assert!(matches!(app.view, View::Chat));
            assert_eq!(app.input.lines(), ["keep this draft"]);
            assert!(app.workers.is_empty() && app.git_jobs.is_empty());
        }
        app.mods[0].execution.as_mut().unwrap().checks[0].output = "saved evidence\n".repeat(100);
        app.view = View::Checks(0);
        screen(&mut app, 80, 24);
        let content = app.scroll.content;
        wheel(&mut app, false, content);
        assert!(matches!(app.view, View::Checks(offset) if offset > 0));
        assert_eq!(app.input.lines(), ["keep this draft"]);
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
            timeout_seconds: None,
            duration_ms: None,
            missing_runtime: None,
            task: None,
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

    #[test]
    fn task_updates_belong_to_the_current_attempt_and_split_details_stay_compact() {
        use timeline::Key;
        let (_data, mut app, _) = execution_app();
        let m = &mut app.mods[0];
        let e = m.execution.as_mut().unwrap();
        e.status = "blocked".into();
        e.checks.clear();
        let run = &mut e.tasks[0];
        let id = run.id;
        let old_source = run.source.clone();
        run.source = crate::store::task_source(id, 2);
        run.status = "blocked".into();
        run.checks.clear();
        run.check_repair = true;
        run.summary = "Muse reached its response limit before finishing the task report. Retry to continue from saved work.".into();
        run.provider = Some("muse".into());
        let source = run.source.clone();
        let old = crate::store::Message {
            source: None,
            task: Some(id),
            item_id: Some(format!("result:{old_source}")),
            role: "muse:65".into(),
            body: "✓ task complete · Earlier repair passed".into(),
            model: Some("muse-spark-1.3".into()),
            effort: Some("high".into()),
        };
        m.messages.push(old.clone());
        assert!(app.inspect_task(id).unwrap().latest.is_none());
        assert!(
            !app.timeline_items()
                .iter()
                .any(|item| item.key == Key::Failure(id))
        );
        for width in [80, 124, 160] {
            app.view = View::Chat;
            screen(&mut app, width, 42);
            app.timeline.selected = Some(Key::Repair(id));
            app.timeline.expanded.insert(Key::Repair(id), true);
            let preview = screen(&mut app, width, 42);
            let text = rows(&preview).join("\n");
            assert!(!text.contains("Earlier repair passed") && !text.contains("task complete"));
            assert!(text.contains("Retry task 1"));
        }
        app.mods[0].messages.push(crate::store::Message {
            source: Some(source),
            item_id: Some("current-update".into()),
            body: "Checking this repair".into(),
            ..old
        });
        let text = rows(&screen(&mut app, 160, 42)).join("\n");
        assert_eq!(text.matches("Checking this repair").count(), 1);
        app.view = View::TaskHistory(id, 0);
        let text = rows(&screen(&mut app, 100, 42)).join("\n");
        assert!(text.contains("Earlier repair passed") && text.contains("Checking this repair"));
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn timeline_navigation_preserves_draft_and_opens_existing_evidence() {
        use timeline::Key;
        let (_data, mut app, _) = execution_app();
        let id = app.mods[0].execution.as_ref().unwrap().tasks[0].id;
        screen(&mut app, 100, 40);
        assert!(!app.timeline_items().iter().any(|i| i.key == Key::Task(id)));
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        app.timeline.selected = Some(Key::Build);
        key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        assert!(app.timeline_items().iter().any(|i| i.key == Key::Task(id)));
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert_eq!(app.timeline.selected, Some(Key::Task(id)));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Task(task, 0) if task == id));
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert!(queued(&app).is_empty());
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Chat));
        // Removed rows cannot activate their former action or submit the draft.
        app.timeline.focus = timeline::Focus::Timeline;
        app.timeline.selected = Some(Key::Repair(id));
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Chat));
        assert!(queued(&app).is_empty());
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(app.timeline.focus == timeline::Focus::Composer && !app.quit);
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        paste(&mut app, " more");
        assert!(app.timeline.focus == timeline::Focus::Composer);
        assert_eq!(app.input.lines(), ["keep this draft more"]);
    }

    #[test]
    fn timeline_keeps_repairs_previous_failures_and_current_evidence_distinct() {
        use timeline::{Key, State};
        let (_data, mut app, _) = execution_app();
        let e = app.mods[0].execution.as_mut().unwrap();
        e.status = "running".into();
        e.fingerprint = None;
        e.checks.clear();
        let run = &mut e.tasks[0];
        let id = run.id;
        run.status = "pending".into();
        run.check_repair = true;
        run.verification_feedback = std::mem::take(&mut run.checks);
        run.verification_feedback[0].exit_code = Some(1);
        run.verification_feedback[0].output = "AssertionError: preserved evidence".into();
        app.executing_mods.insert(app.mods[0].id);
        let items = app.timeline_items();
        let failure = items.iter().find(|i| i.key == Key::Failure(id)).unwrap();
        assert!(failure.state == State::Previous && failure.action.is_none());
        assert!(failure.inspect.is_none());
        assert!(
            failure
                .detail
                .iter()
                .any(|s| s.contains("preserved evidence"))
        );
        let repair = items.iter().find(|i| i.key == Key::Repair(id)).unwrap();
        assert!(repair.state == State::Waiting && repair.title.contains("Waiting for a worker"));
        assert!(
            !repair
                .detail
                .iter()
                .any(|s| s.contains("preserved evidence"))
        );
        assert!(items.iter().find(|i| i.key == Key::Checks).unwrap().state == State::Waiting);
        assert!(!items.iter().any(|i| i.title == "Ready to publish"));
        let e = app.mods[0].execution.as_mut().unwrap();
        e.tasks[0].status = "done".into();
        e.status = "review".into();
        e.fingerprint = Some("checked-version".into());
        let m = &mut app.mods[0];
        m.agent_review = Some(crate::review::State {
            source: "review:1".into(),
            plan_source: m.planning.as_ref().unwrap().source.clone(),
            fingerprint: "checked-version".into(),
            status: "clean".into(),
            rounds: 0,
            report: None,
        });
        assert!(
            app.timeline_items()
                .iter()
                .any(|i| i.title == "Ready to publish")
        );
        app.mods[0].execution.as_mut().unwrap().fingerprint = None;
        assert!(
            !app.timeline_items()
                .iter()
                .any(|i| i.title == "Ready to publish")
        );
        assert!(
            app.timeline_items()
                .iter()
                .any(|i| i.title.contains("fresh review"))
        );
    }

    #[test]
    fn combined_checks_prioritize_failures_group_tasks_and_keep_evidence() {
        use ratatui::style::Color;
        let (_data, mut app, _) = execution_app();
        let m = &mut app.mods[0];
        m.name = "Browser-only todo app".into();
        let plan = m.planning.as_mut().unwrap().plan.as_mut().unwrap();
        plan.tasks[0].title = "Verify the static app".into();
        let mut other = plan.tasks[0].clone();
        other.id = "storage".into();
        other.title = "Preserve local storage".into();
        plan.tasks.push(other.clone());
        let e = m.execution.as_mut().unwrap();
        e.status = "blocked".into();
        e.fingerprint = None;
        e.tasks[0].status = "done".into();
        e.tasks[0].summary = "All task checks passed on the earlier run.".into();
        let first_id = e.tasks[0].id;
        let mut second = e.tasks[0].clone();
        second.id += 1;
        second.task_id = other.id;
        e.tasks.push(second);
        let entries = [
            (
                first_id,
                "Build contains the expected worker and model assets.",
                Some(0),
            ),
            (
                first_id,
                "Restart offline, perform real inference and verify semantic search with zero external requests.",
                Some(1),
            ),
            (
                first_id,
                "Restart offline, perform real inference and verify semantic search with zero external requests.",
                Some(0),
            ),
            (first_id, "Document the verified deployment.", None),
            (first_id + 1, "Retain saved tasks after reopening.", Some(0)),
            (first_id + 1, "Retain saved tasks after reopening.", Some(0)),
        ];
        e.checks = entries
            .iter()
            .enumerate()
            .map(|(i, (id, name, exit))| crate::execution::CheckResult {
                task: Some(*id),
                check: (*name).into(),
                command: vec!["/usr/bin/node".into(), format!("scenario-{i}.mjs")],
                timeout_seconds: Some(120),
                duration_ms: exit.map(|_| 27821),
                exit_code: *exit,
                missing_runtime: None,
                output: if *exit == Some(1) {
                    "error: test timed out after 27000ms\ncode: ERR_TEST_FAILURE".into()
                } else if exit.is_none() {
                    "Not run.".into()
                } else {
                    "Passed output stays hidden".into()
                },
            })
            .collect();
        for run in &mut e.tasks {
            run.checks = e
                .checks
                .iter()
                .filter(|c| c.task == Some(run.id))
                .cloned()
                .collect();
        }
        for width in [160, 124, 80] {
            app.view = View::Checks(0);
            app.show_evidence = false;
            let buffer = screen(&mut app, width, 50);
            let text = rows(&buffer).join("\n");
            assert!(text.find("Needs attention").unwrap() < text.find("Passed checks").unwrap());
            assert!(text.contains("4 passed · 1 failed · 1 not run"));
            assert!(text.contains("27.8s elapsed · 120s limit"));
            assert_eq!(
                text.matches("Retain saved tasks after reopening.").count(),
                1
            );
            assert!(text.contains("2 commands · passed"));
            assert_eq!(text.matches("Restart offline").count(), 1);
            assert!(text.contains("2 commands · 1 passed · 1 failed"));
            assert!(!text.contains("Passed output stays hidden"));
            assert!(
                buffer
                    .content
                    .iter()
                    .any(|cell| cell.symbol() == "✓" && cell.fg == Color::Green)
            );
            assert!(
                buffer
                    .content
                    .iter()
                    .any(|cell| cell.symbol() == "!" && cell.fg == Color::Red)
            );
        }
        app.show_evidence = true;
        app.view = View::Checks(0);
        let text = rows(&screen(&mut app, 120, 70)).join("\n");
        for i in 0..6 {
            assert!(text.contains(&format!("scenario-{i}.mjs")));
        }
        app.view = View::Task(first_id, 0);
        assert!(
            rows(&screen(&mut app, 100, 50))
                .join("\n")
                .contains("Earlier task report · before combined checks")
        );
        app.view = View::Chat;
        app.show_evidence = false;
        app.timeline.focus = timeline::Focus::Inspector;
        app.timeline.held = true;
        app.timeline.selected = Some(timeline::Key::Checks);
        app.timeline.reveal = true;
        let buffer = screen(&mut app, 160, 50);
        let text = rows(&buffer).join("\n");
        assert!(text.contains("Needs attention") && text.contains("27000ms"));
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn timeline_task_details_use_readable_findings_and_check_statuses() {
        use ratatui::style::Color;
        use timeline::{Focus, Key};
        let (_data, mut app, _) = execution_app();
        let m = &mut app.mods[0];
        let plan = m.planning.as_mut().unwrap().plan.as_mut().unwrap();
        plan.tasks[0].title = "Implement the durable browser repository".into();
        let mut task = plan.tasks[0].clone();
        task.id = "interface".into();
        task.title = "Connect the UI to local storage".into();
        task.checks = vec![
            "Reconnect storage after the owning worker fails.".into(),
            "Preserve drafts, selection, focus and retry state while rejecting stale edits and embeddings from another tab.".into(),
            "Keyboard access and narrow layouts remain usable.".into(),
        ];
        plan.tasks.push(task.clone());
        let e = m.execution.as_mut().unwrap();
        e.status = "running".into();
        e.checks.clear();
        e.fingerprint = None;
        let run = &mut e.tasks[0];
        let id = run.id;
        run.status = "pending".into();
        run.checks.clear();
        let findings = serde_json::json!([
            {"owner":"database", "file":"app/static/todo-store.mjs", "line":178,
             "priority":"P1", "title":"Restore the change sequence after owner replacement",
             "evidence":"After reopening, the worker returns sequence 0. A surviving tab keeps sequence 4 and rejects every refreshed list as stale.",
             "fix":"Persist and restore the change sequence before accepting requests from surviving tabs."},
            {"owner":"database", "file":"app/static/database-coordinator.mjs", "line":241,
             "priority":"P2", "title":"Preserve revision tokens across database reopen",
             "evidence":"Reopening assigns fresh revision tokens to unchanged tasks. Existing mutation receipts still contain the original tokens.",
             "fix":"Store revision tokens with each row and reuse them after reopening."}
        ]);
        run.review_feedback = Some(format!(
            "Independent review findings: {findings}\nAddress only these defects within your existing scope."
        ));
        let mut done = run.clone();
        done.id += 1;
        done.task_id = task.id;
        done.source = crate::store::task_source(done.id, 1);
        done.status = "done".into();
        done.review_feedback = None;
        done.checks = task
            .checks
            .iter()
            .map(|check| crate::execution::CheckResult {
                timeout_seconds: None,
                duration_ms: None,
                missing_runtime: None,
                task: None,
                check: check.clone(),
                command: vec!["/usr/bin/true".into()],
                exit_code: Some(0),
                output: String::new(),
            })
            .collect();
        let done_id = done.id;
        let summary = "Fixed storage reconnection after owner crashes while preserving drafts and operation IDs. Healthy quota recovery retains its worker.";
        m.messages.push(crate::store::Message {
            source: Some(done.source.clone()),
            task: Some(done_id),
            item_id: Some(format!("result:{}", done.source)),
            role: "codex:66".into(),
            body: format!("✓ task complete · {}\n{summary}", task.title),
            model: Some("gpt-6.1-sol".into()),
            effort: Some("xhigh".into()),
        });
        e.tasks.push(done);
        screen(&mut app, 160, 46);
        app.timeline.focus = Focus::Timeline;
        app.timeline.held = true;
        app.timeline.selected = Some(Key::Failure(id));
        app.timeline.expanded.insert(Key::Failure(id), true);
        for width in [160, 124, 80] {
            app.timeline.reveal = true;
            let buffer = screen(&mut app, width, 46);
            let text = rows(&buffer).join("\n");
            assert!(text.contains("Saved review") && text.contains("Evidence"));
            assert!(!text.contains("\"owner\"") && !text.contains("Address only"));
            assert!(
                buffer
                    .content
                    .iter()
                    .any(|cell| cell.symbol() == "P" && cell.fg == Color::Red)
            );
        }
        app.timeline.selected = Some(Key::Task(done_id));
        app.timeline.reveal = true;
        let buffer = screen(&mut app, 160, 46);
        let text = rows(&buffer).join("\n");
        assert!(text.contains("Worker report") && text.contains("3 passed"));
        assert!(!text.contains("task complete") && !text.contains("0 failed"));
        let pane = app.timeline.inspector.area;
        assert!(
            (pane.y..pane.bottom()).any(|y| buffer[(pane.x + 2, y)].symbol() == "✓"
                && buffer[(pane.x + 2, y)].fg == Color::Green)
        );
        app.view = View::Task(done_id, 0);
        let text = rows(&screen(&mut app, 80, 46)).join("\n");
        assert!(text.contains("Worker report") && text.contains("3 passed"));
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn timeline_scroll_crosses_task_separators_without_snapping_back() {
        use timeline::{Focus, Key};
        let (_data, mut app, _) = execution_app();
        let plan = app.mods[0]
            .planning
            .as_mut()
            .unwrap()
            .plan
            .as_mut()
            .unwrap();
        for index in 2..24 {
            let mut task = plan.tasks[0].clone();
            task.id = format!("task-{index}");
            task.title = format!("Independent task {index}");
            plan.tasks.push(task);
        }
        app.mods[0].execution = None;
        screen(&mut app, 160, 36);
        app.timeline.focus = Focus::Timeline;
        app.timeline.selected = Some(Key::Planned(0));
        app.timeline.held = true;
        app.timeline.top = 0;
        app.timeline.anchor = None;
        screen(&mut app, 160, 36);
        let maximum = app.timeline.rows.last().unwrap().2 - app.scroll.content.height as usize;
        for up in [false, true] {
            for _ in 0..maximum {
                let old = app.timeline.top;
                let expected = if up {
                    old.saturating_sub(3)
                } else {
                    (old + 3).min(maximum)
                };
                let area = app.scroll.content;
                wheel(&mut app, up, area);
                screen(&mut app, 160, 36);
                assert_eq!(app.timeline.top, expected, "wheel movement at row {old}");
                screen(&mut app, 160, 36);
                assert_eq!(
                    app.timeline.top, expected,
                    "redraw must not move the viewport"
                );
                assert_eq!(app.timeline.selected, Some(Key::Planned(0)));
            }
        }
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn timeline_scrollback_anchors_to_rows_and_jump_follows_current_work() {
        use timeline::Key;
        let (_data, mut app, _) = execution_app();
        let plan = app.mods[0]
            .planning
            .as_mut()
            .unwrap()
            .plan
            .as_mut()
            .unwrap();
        for index in 2..16 {
            let mut task = plan.tasks[0].clone();
            task.id = format!("task-{index}");
            task.title = format!("Independent task {index}");
            plan.tasks.push(task);
        }
        app.mods[0].execution = None;
        screen(&mut app, 100, 36);
        app.timeline.focus = timeline::Focus::Timeline;
        app.timeline.selected = Some(Key::Planned(10));
        app.timeline.reveal = true;
        app.timeline.held = true;
        screen(&mut app, 100, 36);
        let content = app.scroll.content;
        wheel(&mut app, true, content);
        screen(&mut app, 100, 36);
        let anchor = app.timeline.anchor;
        let top = app.timeline.top;
        app.mods[0]
            .planning
            .as_mut()
            .unwrap()
            .plan
            .as_mut()
            .unwrap()
            .tasks[0]
            .outcome = "Extra detail before the viewport. ".repeat(15);
        screen(&mut app, 100, 36);
        assert_eq!(app.timeline.anchor, anchor);
        assert!(app.timeline.top > top);
        assert!(app.timeline.held);
        let draft = app.input.lines().to_vec();
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        screen(&mut app, 100, 36);
        assert!(!app.timeline.held);
        assert_eq!(app.input.lines(), draft);
        app.mods[0].planning.as_mut().unwrap().status = "failed".into();
        screen(&mut app, 100, 36);
        assert_eq!(app.timeline.selected, Some(Key::Plan));
        app.mods[0].planning.as_mut().unwrap().source = "next-plan".into();
        screen(&mut app, 100, 36);
        assert!(app.timeline.focus == timeline::Focus::Composer && !app.timeline.held);
    }

    #[test]
    fn timeline_mouse_focus_resize_and_request_wrapping_keep_the_composer_usable() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use timeline::Key;
        let (_data, mut app, _) = execution_app();
        app.mods[0].description =
            "First request line\nSecond request line\nThird request line".into();
        app.mods[0].messages.clear();
        for width in [36, 80, 160] {
            let buffer = screen(&mut app, width, 40);
            let input = app.scroll.input;
            assert!(input.height >= 4);
            let (_, start, _) = *app
                .timeline
                .rows
                .iter()
                .find(|(k, _, _)| *k == Key::Build)
                .unwrap();
            app.timeline.top = start;
            app.timeline.held = true;
            app.timeline.anchor = Some((Key::Build, 0));
            screen(&mut app, width, 40);
            let content = app.scroll.content;
            app.handle(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: content.x,
                row: content.y + (start - app.timeline.top) as u16,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();
            assert!((app.timeline.focus != timeline::Focus::Composer));
            assert_eq!(app.timeline.selected, Some(Key::Build));
            app.handle(Event::Mouse(MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: input.x,
                row: input.y,
                modifiers: KeyModifiers::NONE,
            }))
            .unwrap();
            assert!(app.timeline.focus == timeline::Focus::Composer);
            assert_eq!(app.input.lines(), ["keep this draft"]);
            assert!(!rows(&buffer).iter().any(|line| line.contains("\n")));
        }
        app.timeline.selected = Some(Key::Request);
        app.timeline.focus = timeline::Focus::Timeline;
        app.timeline.reveal = true;
        key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        let text = rows(&screen(&mut app, 100, 40)).join("\n");
        assert_eq!(text.matches("First request line").count(), 1);
        assert!(text.contains("Second request line") && text.contains("Third request line"));
        screen(&mut app, 20, 8);
        assert!(app.scroll.content.is_empty() && app.scroll.input.is_empty());
    }

    #[test]
    fn timeline_split_focus_and_evidence_preserve_the_draft() {
        use timeline::{Focus, Key};
        let (_data, mut app, _) = execution_app();
        let id = app.mods[0].execution.as_ref().unwrap().tasks[0].id;
        screen(&mut app, 160, 45);
        assert!(!app.timeline.inspector.area.is_empty());
        assert!(
            app.scroll.input.width + 4
                >= app.scroll.content.width + app.timeline.inspector.area.width
        );
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.timeline.focus, Focus::Timeline);
        app.timeline.selected = Some(Key::Build);
        key(&mut app, KeyCode::Char(' '), KeyModifiers::NONE);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        let buffer = screen(&mut app, 160, 45);
        assert_eq!(app.timeline.inspector.key, Some(Key::Task(id)));
        assert!(rows(&buffer).join("\n").contains("▸ Timeline"));
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.timeline.focus, Focus::Inspector);
        assert!(
            rows(&screen(&mut app, 160, 45))
                .join("\n")
                .contains("▸ Details")
        );
        key(&mut app, KeyCode::Char('e'), KeyModifiers::NONE);
        assert!(app.show_evidence);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.view == View::Task(id, 0));
        assert!(queued(&app).is_empty());
        app.view = View::Chat;
        screen(&mut app, 160, 45);
        key(&mut app, KeyCode::Tab, KeyModifiers::NONE);
        assert_eq!(app.timeline.focus, Focus::Composer);
        key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(app.timeline.focus, Focus::Inspector);
        key(&mut app, KeyCode::BackTab, KeyModifiers::SHIFT);
        assert_eq!(app.timeline.focus, Focus::Timeline);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert_eq!(app.timeline.focus, Focus::Composer);
        assert!(!app.quit);
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn timeline_inspector_scroll_is_independent_and_survives_resize() {
        use timeline::{Focus, Key};
        let (_data, mut app, _) = execution_app();
        app.mods[0]
            .planning
            .as_mut()
            .unwrap()
            .plan
            .as_mut()
            .unwrap()
            .contracts = (0..60)
            .map(|i| format!("Contract {i} with concrete evidence."))
            .collect();
        screen(&mut app, 160, 42);
        app.timeline.focus = Focus::Timeline;
        app.timeline.selected = Some(Key::Plan);
        app.timeline.held = true;
        let buffer = screen(&mut app, 160, 42);
        let right = app.timeline.inspector.area;
        assert!(right.height > 0 && app.timeline.inspector.maximum > 0);
        assert!((right.y..right.bottom()).any(|y| buffer[(right.right() - 1, y)].symbol() == "┃"));
        let timeline_top = app.timeline.top;
        wheel(&mut app, false, right);
        screen(&mut app, 160, 42);
        assert_eq!(app.timeline.top, timeline_top);
        assert_eq!(app.timeline.inspector.offset, 3);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(app.timeline.focus, Focus::Inspector);
        key(&mut app, KeyCode::End, KeyModifiers::NONE);
        screen(&mut app, 160, 42);
        assert_eq!(
            app.timeline.inspector.offset,
            app.timeline.inspector.maximum
        );
        screen(&mut app, 160, 100);
        assert!(app.timeline.inspector.offset < 60);
        screen(&mut app, 80, 42);
        assert!(app.timeline.inspector.area.is_empty());
        assert_eq!(app.timeline.focus, Focus::Timeline);
        assert_eq!(app.timeline.selected, Some(Key::Plan));
        screen(&mut app, 160, 42);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        screen(&mut app, 160, 42);
        assert_eq!(app.timeline.inspector.offset, 0);
        assert_eq!(app.timeline.inspector.key, Some(Key::Build));
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn timeline_split_mouse_selection_and_changed_plan_keep_evidence_truthful() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        use timeline::{Focus, Key};
        let (_data, mut app, _) = execution_app();
        screen(&mut app, 160, 45);
        let right = app.timeline.inspector.area;
        app.handle(Event::Mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: right.x,
            row: right.y,
            modifiers: KeyModifiers::NONE,
        }))
        .unwrap();
        assert_eq!(app.timeline.focus, Focus::Inspector);
        assert!(app.timeline.held);
        // A vanished row must not open a different stage or send the draft.
        app.timeline.selected = Some(Key::Repair(-1));
        assert!(
            rows(&screen(&mut app, 160, 45))
                .join("\n")
                .contains("no longer in the current plan")
        );
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(app.view == View::Chat);
        assert!(queued(&app).is_empty());
        app.mods[0].planning.as_mut().unwrap().source = "a new plan".into();
        screen(&mut app, 160, 45);
        assert_eq!(app.timeline.focus, Focus::Composer);
        assert!(!app.timeline.held);
        assert_ne!(app.timeline.selected, Some(Key::Repair(-1)));
        assert_eq!(app.timeline.inspector.offset, 0);
        assert_eq!(app.input.lines(), ["keep this draft"]);
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
        while !app.git_jobs.is_empty() || app.project_job.is_some() {
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
        app.auto_reviews.insert(id);
        assert!(
            !app.review_due(0),
            "Publishing ends automatic review intent"
        );
        let display = rows(&screen(&mut app, 116, 40)).join("\n");
        assert!(display.contains("PR published") && display.contains("Request edits"));
        assert!(display.contains("PR published") && !display.contains("changes ready"));
        assert_eq!(app.mods[0].execution.as_ref().unwrap().status, "review");
        app.git_states.get_mut(&id).unwrap().phase = "ready".into();
        assert!(
            app.state_summary()
                .evidence
                .contains("PR open · update pending")
        );
        app.git_states.get_mut(&id).unwrap().phase = "published".into();
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
        assert!(display.contains("Closed · work saved"));
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
        let rendered = rows(&screen(&mut app, 100, 30))
            .join("\n")
            .replace('\u{a0}', " ");
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
                .contains("ctrl+r Retry final checks")
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
        let id = worker.id;
        app.workers.insert(worker.id, worker);
        let execution = app.mods[0].execution.as_mut().unwrap();
        execution.status = "running".into();
        execution.tasks[0].status = "running".into();
        execution.tasks[0].worker = Some(id);
        app.workers
            .get_mut(&id)
            .unwrap()
            .bind_test_task(execution.tasks[0].source.clone());
        app.mods[0].messages.push(crate::store::Message {
            source: None,
            task: None,
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
                "{glyph} codex · executor · w{id} · current-model · high"
            )));
            assert!(!screen.contains("Previous reply."));
            assert!(screen.contains(&format!("{glyph} 1. Greeting")));
        }
        let still = rows(&screen(&mut app, 116, 40)).join("\n");
        assert!(still.contains(&format!(
            "⠿ codex · executor · w{id} · current-model · high"
        )));
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
        let execution = app.mods[0].execution.as_mut().unwrap();
        execution.tasks[0].status = "done".into();
        execution.status = "verifying".into();
        assert!(
            app.action_dock()
                .status
                .starts_with("Working · combined checks")
        );
        app.workers.values_mut().next().unwrap().status = Status::Ready;
        let idle = rows(&screen_at(
            &mut app,
            116,
            40,
            Some(Duration::from_millis(80)),
        ))
        .join("\n");
        assert!(idle.contains(&format!(
            "◆ codex · executor · w{id} · current-model · high"
        )));
        assert!(!idle.contains('⠙'));
        key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
        let history = rows(&screen(&mut app, 116, 40)).join("\n");
        assert!(history.contains("◆ codex · executor · previous-model · low"));
        assert!(history.contains("Previous reply."));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        app.workers.values_mut().next().unwrap().role = Role::Planner;
        let planner = rows(&screen(&mut app, 116, 40)).join("\n");
        assert!(planner.contains("▤ codex · planner · current-model · high"));
    }

    #[test]
    fn retry_one_failed_task_preserves_running_peers_and_waits_for_capacity() {
        let (_data, mut app, _root) = execution_app();
        let mod_id = app.mods[0].id;
        let mut plan = app.mods[0].planning.as_ref().unwrap().plan.clone().unwrap();
        let mut ids = Vec::new();
        let mut workers = Vec::new();
        for (index, (provider, status)) in [
            ("muse", "paused"),
            ("codex", "running"),
            ("codex", "running"),
        ]
        .iter()
        .enumerate()
        {
            let mut task = plan.tasks[0].clone();
            task.id = format!("task-{index}");
            task.title = format!("Task {index}");
            task.worker = (*provider).into();
            task.files = vec![format!("task-{index}.txt")];
            let record = app
                .store
                .worker_provider(mod_id, Role::Executor, index.saturating_sub(1), provider)
                .unwrap();
            let mut worker =
                Worker::start(&app.project, &app.mods[0], record, Role::Planner, None).unwrap();
            worker.role = Role::Executor;
            worker.status = if index == 0 {
                Status::Failed
            } else {
                Status::Running
            };
            worker.enabled = index != 0;
            workers.push(worker.id);
            app.store.0.execute("INSERT INTO task_runs(mod_id,task_id,source,status,worker_id,summary) VALUES (?1,?2,?3,?4,?5,'saved evidence')",
                rusqlite::params![mod_id,task.id,format!("fixture-{index}"),status,worker.id]).unwrap();
            ids.push(app.store.0.last_insert_rowid());
            app.workers.insert(worker.id, worker);
            plan.tasks.push(task);
        }
        app.store
            .save_plan(
                mod_id,
                &app.mods[0].planning.as_ref().unwrap().source,
                &plan,
            )
            .unwrap();
        app.mods[0].planning = app.store.planning(mod_id).unwrap();
        app.mods[0].execution = app.store.execution(mod_id).unwrap();
        app.muse = true;
        app.executing_mods.insert(mod_id);
        let before = app.store.execution(mod_id).unwrap().unwrap();
        let dock = app.action_dock();
        assert_eq!(dock.status, "Task 2 needs attention");
        assert!(
            dock.actions
                .iter()
                .any(|a| a.action == Action::RetryTask(ids[0]))
        );
        assert!(dock.actions.iter().any(|a| a.action == Action::Stop));
        let visible = rows(&screen(&mut app, 120, 36)).join("\n");
        assert!(visible.contains("Task 2 needs attention"));
        key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Tasks(1)));
        let visible = rows(&screen(&mut app, 120, 36)).join("\n");
        assert!(visible.replace('\u{a0}', " ").contains("retry task"));
        key(&mut app, KeyCode::Char('r'), KeyModifiers::NONE);
        let after = app.mods[0].execution.as_ref().unwrap();
        assert_eq!(after.tasks[1].status, "pending");
        assert_ne!(after.tasks[1].source, before.tasks[1].source);
        assert_eq!(after.tasks[1].worker, Some(workers[0]));
        for index in [0, 2, 3] {
            assert_eq!(after.tasks[index].source, before.tasks[index].source);
            assert_eq!(after.tasks[index].status, before.tasks[index].status);
            assert_eq!(after.tasks[index].summary, before.tasks[index].summary);
        }
        assert!(!app.workers.contains_key(&workers[0]));
        assert!(
            workers[1..]
                .iter()
                .all(|w| app.workers[w].status == Status::Running && app.workers[w].enabled)
        );
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert_eq!(
            app.inspect_task(ids[0]).unwrap().state,
            "Waiting for a worker"
        );
        assert!(!app.can_retry_task(ids[0]));
        let selected = app
            .store
            .schedule_workers(mod_id, &plan, &workers[1..], &[], &["codex", "muse"])
            .unwrap();
        assert_eq!(selected.len(), 2);
        assert!(selected.iter().all(|w| workers[1..].contains(&w.id)));
        app.store
            .finish_task(
                mod_id,
                &before.tasks[2].source,
                "done",
                "Finished peer",
                &[],
            )
            .unwrap();
        let selected = app
            .store
            .schedule_workers(mod_id, &plan, &workers[2..], &[], &["codex", "muse"])
            .unwrap();
        assert!(selected.iter().any(|w| w.id == workers[0]));
        assert!(!app.store.retry_task(mod_id, before.tasks[0].id).unwrap());
        assert!(!app.store.retry_task(mod_id, ids[1]).unwrap());
        assert!(!app.store.retry_task(mod_id + 1000, ids[0]).unwrap());
    }

    #[test]
    fn two_workers_show_task_ownership_and_queue_target_without_losing_draft() {
        let (_data, mut app, _root) = execution_app();
        let id = app.current_mod().unwrap().id;
        let mut workers = Vec::new();
        for provider in ["codex", "muse"] {
            let record = app
                .store
                .worker_provider(id, Role::Executor, 0, provider)
                .unwrap();
            let mut worker = Worker::start(
                &app.project,
                app.current_mod().unwrap(),
                record,
                Role::Planner,
                None,
            )
            .unwrap();
            worker.role = Role::Executor;
            worker.status = Status::Running;
            worker.model = Some("test-model".into());
            worker.effort = Some("high".into());
            workers.push(worker.id);
            app.workers.insert(worker.id, worker);
        }
        let plan = app.mods[0]
            .planning
            .as_mut()
            .unwrap()
            .plan
            .as_mut()
            .unwrap();
        let mut second = plan.tasks[0].clone();
        second.id = "two".into();
        second.title = "Second task".into();
        second.worker = "muse".into();
        plan.tasks.push(second);
        let execution = app.mods[0].execution.as_mut().unwrap();
        execution.status = "running".into();
        execution.tasks[0].status = "running".into();
        execution.tasks[0].worker = Some(workers[0]);
        let mut second = execution.tasks[0].clone();
        second.id += 1;
        second.task_id = "two".into();
        second.worker = Some(workers[1]);
        second.source = crate::store::task_source(second.id, 1);
        execution.tasks.push(second);
        for run in &execution.tasks {
            app.workers
                .get_mut(&run.worker.unwrap())
                .unwrap()
                .bind_test_task(run.source.clone());
        }
        let view = rows(&screen_at(
            &mut app,
            120,
            36,
            Some(Duration::from_millis(80)),
        ))
        .join("\n");
        assert!(view.contains(&format!("codex w{} · task 1", workers[0])));
        assert!(view.contains(&format!("muse w{} · task 2", workers[1])));
        let narrow = rows(&screen(&mut app, 90, 36)).join("\n");
        assert!(narrow.contains("2 workers running"));
        assert!(view.contains("⠙ 1. Greeting · Working"), "{view}");
        assert!(view.contains("⠙ 2. Second task · Working"), "{view}");
        let queued = app.store.enqueue(id, "Only the second worker").unwrap();
        app.mods[0].queue.push(queued);
        key(&mut app, KeyCode::Char('q'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('t'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('t'), KeyModifiers::NONE);
        assert_eq!(app.steer_target, Some(workers[1]));
        let view = rows(&screen(&mut app, 120, 36))
            .join("\n")
            .replace('\u{a0}', " ");
        assert!(view.contains(&format!("target: w{}", workers[1])));
        key(&mut app, KeyCode::Char('s'), KeyModifiers::NONE);
        assert!(matches!(app.view, View::Chat));
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert!(app.store.next_steering(id, workers[0]).unwrap().is_none());
        assert!(app.store.next_steering(id, workers[1]).unwrap().is_some());
        for worker in app.workers.values_mut() {
            worker.status = Status::Ready;
        }
        key(&mut app, KeyCode::Char('r'), KeyModifiers::CONTROL);
        assert!(app.workers.values().all(|w| !w.enabled));
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
            source: None,
            task: None,
            item_id: Some("old-reply".into()),
            role: "codex".into(),
            body: "Earlier reply.".into(),
            model: Some("gpt-6.1-sol".into()),
            effort: None,
        });
        key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
        let screen = rows(&screen(&mut app, 116, 40)).join("\n");
        assert!(screen.contains("◆ codex · executor · gpt-6.1-sol · effort unknown"));
    }

    #[test]
    fn completed_versions_separate_results_details_and_history() {
        let (data, mut app, root) = execution_app();
        let id = app.mods[0].id;
        let message = crate::store::Message {
            source: None,
            task: None,
            item_id: Some("saved-worker-reply".into()),
            role: "codex:45".into(),
            body: "I will inspect the saved edits.".into(),
            model: Some("gpt-6.1-sol".into()),
            effort: Some("xhigh".into()),
        };
        app.store.save_message(id, &message).unwrap();
        app.mods[0].messages.push(message);
        let plan = app.mods[0]
            .planning
            .as_mut()
            .unwrap()
            .plan
            .as_mut()
            .unwrap();
        plan.contracts.push("Keep task order unchanged.".into());
        for (id, title) in [("two", "Build the interface"), ("three", "Verify together")] {
            let mut task = plan.tasks[0].clone();
            task.id = id.into();
            task.title = title.into();
            plan.tasks.push(task);
        }
        let execution = app.mods[0].execution.as_mut().unwrap();
        for id in ["two", "three"] {
            let mut run = execution.tasks[0].clone();
            run.task_id = id.into();
            execution.tasks.push(run);
            execution.checks.push(execution.checks[0].clone());
        }
        let mut selection = crate::router::Selection::fallback("Jev uncertain");
        selection.evidence = Some(serde_json::json!({"answers":{"risk":{"confidence":0.47}}}));
        execution.tasks[0].selection = Some(selection);
        for width in [48, 116] {
            let compact = rows(&screen(&mut app, width, 40));
            let text = compact.join("\n");
            assert!(text.contains("Checks passed · review pending"));
            assert!(text.contains("Combined checks passed") && text.contains("More"));
            assert!(app.action_dock().primary == Some(Action::Review));
            assert!(
                app.action_dock()
                    .actions
                    .iter()
                    .any(|a| a.action == Action::Publish)
            );
            assert!(text.contains("Implementation · 3/3 tasks finished"));
            assert!(!text.contains("1. Greeting") && !text.contains("2. Build the interface"));
            assert!(!text.contains("Print hello") && !text.contains("I will inspect"));
            assert!(!text.contains("Keep task order") && !text.contains("/usr/bin/true"));
            key(&mut app, KeyCode::Char('g'), KeyModifiers::CONTROL);
            let actions = rows(&screen(&mut app, width, 40)).join("\n");
            assert!(actions.contains("Details") && actions.contains("Publish PR"));
            key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        }
        let instruction = app.store.enqueue(id, "Keep the current icons").unwrap();
        app.mods[0].queue.push(instruction);
        app.store.save_draft(id, "keep this draft").unwrap();
        key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        let details = rows(&screen(&mut app, 116, 50)).join("\n");
        assert!(
            details.contains("Keep task order unchanged.") && details.contains("/usr/bin/true")
        );
        assert!(details.contains("risk confidence 0.47 < 0.80"));
        assert!(!details.contains("I will inspect"));
        key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(matches!(app.view, View::Tasks(0)));
        key(&mut app, KeyCode::Char('h'), KeyModifiers::NONE);
        let history = rows(&screen(&mut app, 116, 40)).join("\n");
        assert!(
            history.contains("all history") && history.contains("I will inspect the saved edits.")
        );
        assert!(history.contains("w45 · gpt-6.1-sol · xhigh"));
        assert!(!history.contains("/usr/bin/true") && !history.contains("Keep task order"));
        key(&mut app, KeyCode::Down, KeyModifiers::NONE);
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Tasks(0)));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        assert!(matches!(app.view, View::Chat) && app.plan_details);
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert_eq!(queued(&app), ["Keep the current icons"]);
        assert_eq!(
            std::fs::read_to_string(root.join("work/hello.sh")).unwrap(),
            "new\n"
        );
        let reopened = App::load(app.project.clone(), false, data.store()).unwrap();
        assert!(!reopened.plan_details && matches!(reopened.view, View::Chat));
        assert_eq!(reopened.input.lines(), ["keep this draft"]);
        assert_eq!(queued(&reopened), ["Keep the current icons"]);
        assert!(
            reopened.mods[0]
                .messages
                .iter()
                .any(|m| m.item_id.as_deref() == Some("saved-worker-reply"))
        );
    }

    #[test]
    fn expanded_checks_keep_code_indentation_and_the_toggle_in_view() {
        let (_data, mut app, _root) = execution_app();
        let result = &mut app.mods[0].execution.as_mut().unwrap().tasks[0].checks[0];
        result.command = vec!["/workspace/.venv/bin/python".into(), "-c".into(), "with app.test_client() as client:\n    response = client.get('/health')\n    assert response.status_code == 200, 'the server must return a successful response'".into()];
        for width in [48, 116] {
            app.plan_details = true;
            app.focus_plan = true;
            let expanded = rows(&screen(&mut app, width, 46));
            assert!(
                app.action_dock()
                    .actions
                    .iter()
                    .any(|a| a.action == Action::Details && a.label == "Hide plan details")
            );
            assert!(expanded.iter().any(|row| row.contains("More")));
            assert!(expanded.iter().any(|row| row.contains("│     response")));
            let start = expanded
                .iter()
                .position(|row| row.contains("   checks"))
                .unwrap();
            let end = expanded
                .iter()
                .position(|row| row.contains("worker report · Done"))
                .unwrap();
            let code: Vec<_> = expanded[start..end]
                .iter()
                .filter(|row| row.contains('│'))
                .collect();
            assert!(code.len() >= 4);
            assert!(code.iter().all(|row| row.find('│') == Some(9)), "{code:#?}");
            assert!(
                code.iter()
                    .all(|row| row.trim_end().chars().count() <= width as usize - 2)
            );
            key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
            let collapsed = rows(&screen(&mut app, width, 42)).join("\n");
            assert!(collapsed.contains("More"));
            assert!(
                app.action_dock()
                    .actions
                    .iter()
                    .any(|a| a.action == Action::Details && a.label == "Show plan details")
            );
            assert!(!collapsed.contains("/workspace/.venv"));
            assert!(!collapsed.contains("execution stays"));
            assert_eq!(app.input.lines(), ["keep this draft"]);
        }
    }

    #[test]
    fn provider_assignment_reasons_stay_in_plan_details() {
        let (data, mut store, id, plan) =
            crate::scheduler::tests::fixture(&["auto", "auto"], false);
        let project = data.0.join("project");
        std::fs::create_dir_all(&project).unwrap();
        store
            .schedule_workers(id, &plan, &[], &[], &["codex", "muse"])
            .unwrap();
        let mut app = App::load(project, false, store).unwrap();
        app.input.insert_str("keep this draft");
        let compact = rows(&screen(&mut app, 110, 36)).join("\n");
        assert!(!compact.contains("equally suitable"));
        app.plan_details = true;
        let expanded = rows(&screen(&mut app, 110, 36)).join("\n");
        assert!(
            expanded.contains("worker · codex · equally suitable, alternating tie"),
            "{expanded}"
        );
        assert!(
            expanded.contains("worker · muse · equally suitable, lower load"),
            "{expanded}"
        );
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn final_verification_groups_skipped_commands_and_offers_retry() {
        for width in [80, 120] {
            let (_data, mut app, _) = execution_app();
            let e = app.mods[0].execution.as_mut().unwrap();
            for task in &mut e.tasks {
                task.status = "done".into();
            }
            e.status = "blocked".into();
            e.checks = (0..16)
                .map(|i| crate::execution::CheckResult {
                    timeout_seconds: None,
                    duration_ms: None,
                    task: Some(e.tasks[0].id),
                    check: "Verify relevance".into(),
                    command: vec!["/tasks/53/.venv/bin/python".into(), format!("check-{i}")],
                    exit_code: None,
                    missing_runtime: (i == 0).then(|| "/tasks/53/.venv/bin/python".into()),
                    output: if i == 0 {
                        "Check executable is unavailable: /tasks/53/.venv/bin/python.".into()
                    } else {
                        "Not run.".into()
                    },
                })
                .collect();
            app.plan_details = true;
            let dock = app.action_dock();
            assert_eq!(dock.status, "Combined checks need attention");
            assert!(dock.primary == Some(Action::Retry));
            assert!(dock.actions.iter().any(|a| a.label == "Retry final checks"));
            assert!(dock.error.unwrap().contains("executable is unavailable"));
            let buffer = screen(&mut app, width, 55);
            let rendered = rows(&buffer).join("\n");
            assert!(rendered.contains("15 commands not run"), "{rendered}");
            assert!(!rendered.contains("Not run."), "{rendered}");
            assert_eq!(rendered.matches("! Verify relevance").count(), 1);
            assert!(!rendered.contains("Working · 3/3"));
            assert_eq!(app.input.lines(), ["keep this draft"]);
        }
    }

    #[test]
    fn verification_results_show_the_assertion_and_label_worker_claims() {
        let (_data, mut app, _) = execution_app();
        let id = app.mods[0].id;
        let mut plan = app.mods[0].planning.as_ref().unwrap().plan.clone().unwrap();
        plan.tasks[0].checks = vec!["Suite".into(), "Browser flows".into(), "Real model".into()];
        app.store
            .save_plan(id, &app.mods[0].planning.as_ref().unwrap().source, &plan)
            .unwrap();
        let source = app.mods[0].execution.as_ref().unwrap().tasks[0]
            .source
            .clone();
        let checks = vec![crate::execution::CheckResult { timeout_seconds: None, duration_ms: None, missing_runtime: None, task: None, check: "Suite".into(), command: vec!["/bin/true".into()], exit_code: Some(0), output: String::new() }, crate::execution::CheckResult { timeout_seconds: None, duration_ms: None, missing_runtime: None, task: None, check: "Browser flows".into(), command: vec!["/bin/false".into()], exit_code: Some(1), output: "DeprecationWarning: dependency\nTraceback\nAssertionError: Upload retry\nactual: undefined\nexpected: true".into() }];
        app.store
            .finish_task(id, &source, "blocked", "All checks passed", &checks)
            .unwrap();
        app.mods[0].planning = app.store.planning(id).unwrap();
        app.mods[0].execution = app.store.execution(id).unwrap();
        app.plan_details = true;
        let rendered = rows(&screen(&mut app, 120, 50)).join("\n");
        assert!(
            rendered.contains("1/3 passed · 1 failed · 1 not run"),
            "{rendered}"
        );
        assert!(rendered.contains("Real model · not run"), "{rendered}");
        assert!(
            rendered.contains("AssertionError: Upload retry"),
            "{rendered}"
        );
        assert!(
            rendered.contains("worker report · All checks passed"),
            "{rendered}"
        );
        assert!(!rendered.contains("DeprecationWarning"), "{rendered}");
        assert_eq!(app.input.lines(), ["keep this draft"]);
    }

    #[test]
    fn repeated_check_names_show_every_command_and_accurate_progress() {
        for failed in [false, true] {
            let (_data, mut app, _) = execution_app();
            let m = &mut app.mods[0];
            m.planning.as_mut().unwrap().plan.as_mut().unwrap().tasks[0].checks =
                vec!["Real model".into(), "Browser flows".into(), "Suites".into()];
            let execution = m.execution.as_mut().unwrap();
            let run = &mut execution.tasks[0];
            run.checks = [
                "Real model",
                "Real model",
                "Real model",
                "Browser flows",
                "Suites",
            ]
            .iter()
            .enumerate()
            .map(|(i, name)| crate::execution::CheckResult {
                timeout_seconds: None,
                duration_ms: None,
                missing_runtime: None,
                task: Some(run.id),
                check: (*name).into(),
                command: vec!["/bin/check".into(), format!("--scenario-{i}")],
                exit_code: if failed && i > 1 {
                    None
                } else {
                    Some(i64::from(failed && i == 1))
                },
                output: if failed && i == 1 {
                    "AssertionError: relevance".into()
                } else {
                    String::new()
                },
            })
            .collect();
            run.status = if failed { "blocked" } else { "done" }.into();
            execution.status = if failed { "blocked" } else { "review" }.into();
            execution.checks = run.checks.clone();
            app.plan_details = true;
            let rendered = rows(&screen(&mut app, 120, 70)).join("\n");
            for i in 0..5 {
                assert!(rendered.contains(&format!("--scenario-{i}")), "{rendered}");
            }
            assert!(!rendered.contains("5/3"), "{rendered}");
            if failed {
                assert!(
                    rendered.contains("1/5 passed · 1 failed · 3 not run"),
                    "{rendered}"
                );
                assert!(rendered.contains("! Real model · incomplete"), "{rendered}");
                assert!(!rendered.contains("✓ Real model"));
                assert!(rendered.contains("AssertionError: relevance"));
            } else {
                assert!(rendered.contains("✓ 5/5 checks passed"), "{rendered}");
                assert!(rendered.contains("verification · 5/5 passed"), "{rendered}");
                assert!(rendered.contains("✓ Real model"), "{rendered}");
            }
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
                    source: None,
                    task: None,
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
            .planning_model(code_mod.id, &Selection::planner())
            .unwrap();
        store.save_plan(code_mod.id, &source, &plan).unwrap();
        let mut app = App::load(project.clone(), false, store).unwrap();
        paste(&mut app, "queued question");
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        paste(&mut app, "keep this draft\nsecond line");

        let compact = rows(&screen(&mut app, 100, 42)).join("\n");
        assert!(compact.contains("1. Build the API") && compact.contains("after task 1"));
        assert!(compact.contains("1. Build the API"));
        assert!(!compact.contains("app/main.py") && !compact.contains("gpt-6-astra"));
        assert!(!compact.contains("I'll inspect"));
        assert!(compact.contains("ctrl+q manage"));

        key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        let expanded = rows(&screen(&mut app, 100, 48)).join("\n");
        assert!(
            expanded.contains("app/main.py") && expanded.contains("Adding and deleting works.")
        );
        assert!(expanded.contains("gpt-6-astra · xhigh") && expanded.contains("quality first"));
        assert_eq!(app.input.lines(), ["keep this draft", "second line"]);
        assert_eq!(queued(&app), ["queued question"]);
        assert_eq!(app.current_mod().unwrap().messages.len(), 3);

        key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        let narrow = rows(&screen(&mut app, 48, 24)).join("\n");
        assert!(
            narrow.contains("▤ Plan") && narrow.contains("1. Build the API"),
            "{narrow}"
        );
        key(&mut app, KeyCode::PageDown, KeyModifiers::NONE);
        screen(&mut app, 48, 24);
        key(&mut app, KeyCode::Char('o'), KeyModifiers::CONTROL);
        let collapsed = rows(&screen(&mut app, 48, 24)).join("\n");
        assert!(collapsed.contains("Timeline") && !collapsed.contains("app/main.py"));
        assert!(
            app.timeline_items()
                .iter()
                .any(|i| i.title.contains("1. Build the API"))
        );
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
                        source: None,
                        task: None,
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
        key(&mut app, KeyCode::Char('t'), KeyModifiers::CONTROL);
        assert!(matches!(app.view, View::History(_)));
        assert_eq!(app.input.lines(), ["draft stays"]);
        for width in [36, 60, 100] {
            app.view = View::History(0);
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
            let background = buffer[(4, first)].bg;
            assert_ne!(background, ratatui::style::Color::Reset);
            assert_eq!(buffer[(6, first)].fg, ratatui::style::Color::White);
            assert!(
                !buffer[(6, first)]
                    .modifier
                    .contains(ratatui::style::Modifier::BOLD)
            );
            for y in first..next - 1 {
                assert_eq!(buffer[(4, y)].symbol(), if y == first { ">" } else { " " });
                assert_eq!(buffer[(width - 7, y)].bg, background);
            }
            for y in [next - 1, agent - 1] {
                assert!(
                    rows[y as usize]
                        .chars()
                        .skip(4)
                        .take((width - 8) as usize)
                        .collect::<String>()
                        .trim()
                        .is_empty()
                );
            }
            assert_ne!(buffer[(4, agent)].bg, background);
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
