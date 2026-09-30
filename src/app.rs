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
    plan::Role,
    router::Router,
    sprout,
    store::{CodeMod, Store, source_id},
    ui,
    worker::{Status, Worker},
    workspace::{self, Review},
};

#[derive(Clone, Copy)]
pub enum View {
    Chat,
    Mods(usize),
    DeleteMod(usize),
    NewMod,
    Queue(usize),
    EditQueue(usize),
    Review(u16),
    Apply,
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
    pub queue_selection: BTreeSet<i64>,
    workers: BTreeMap<i64, Worker>,
    auto_plans: BTreeSet<i64>,
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
        Self::load(project.canonicalize()?, motion, Store::local()?).map_err(io::Error::other)
    }

    fn load(project: PathBuf, motion: bool, store: Store) -> Result<Self> {
        let state = store.load_project(&project)?;
        let active = state
            .mods
            .iter()
            .position(|code_mod| Some(code_mod.id) == state.active_mod_id)
            .or_else(|| (!state.mods.is_empty()).then_some(0));
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
            queue_selection: BTreeSet::new(),
            workers: BTreeMap::new(),
            auto_plans: BTreeSet::new(),
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

    pub fn current_mod(&self) -> Option<&CodeMod> {
        self.active.map(|index| &self.mods[index])
    }

    pub fn run(&mut self, terminal: &mut DefaultTerminal) -> io::Result<()> {
        let started = Instant::now();
        let mut last_frame = started;
        while !self.quit {
            self.poll_workers().map_err(io::Error::other)?;
            let now = Instant::now();
            let elapsed = now.duration_since(last_frame);
            last_frame = now;
            let (pose, next_pose) = sprout::animation(
                now.duration_since(started),
                self.happy_since.map(|since| now.duration_since(since)),
            );

            terminal.draw(|frame| {
                let header = ui::draw(
                    frame,
                    self,
                    if self.motion {
                        pose
                    } else {
                        sprout::Pose::Idle
                    },
                );
                self.welcome
                    .process(elapsed.into(), frame.buffer_mut(), header);
            })?;

            if self.welcome.running() || self.motion || !self.workers.is_empty() {
                let timeout = if self.welcome.running() {
                    Duration::from_millis(33)
                } else {
                    if self.workers.is_empty() {
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
                View::Chat | View::EditQueue(_) => {
                    self.input
                        .insert_str(text.replace("\r\n", "\n").replace('\r', "\n"));
                }
                View::NewMod => {
                    self.input
                        .insert_str(text.replace("\r\n", "\n").replace('\r', "\n"));
                }
                View::Mods(_)
                | View::DeleteMod(_)
                | View::Queue(_)
                | View::Review(_)
                | View::Apply => {}
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
                        KeyCode::Esc => self.view = View::Mods(index),
                        KeyCode::Enter
                            if key.modifiers.is_empty() && key.kind == KeyEventKind::Press =>
                        {
                            self.delete_mod(index)?;
                        }
                        _ => {}
                    },
                    View::Queue(index) => self.queue_key(key, index)?,
                    View::EditQueue(index) => self.edit_key(key, index)?,
                    View::Review(scroll) => match key.code {
                        KeyCode::Esc => {
                            self.view = View::Chat;
                            self.review = None;
                        }
                        KeyCode::Down => self.view = View::Review(scroll.saturating_add(1)),
                        KeyCode::Up => self.view = View::Review(scroll.saturating_sub(1)),
                        KeyCode::PageDown => {
                            self.view = View::Review(scroll.saturating_add(self.page_size))
                        }
                        KeyCode::PageUp => {
                            self.view = View::Review(scroll.saturating_sub(self.page_size))
                        }
                        KeyCode::Char('a') if key.modifiers.is_empty() && self.can_apply() => {
                            self.view = View::Apply
                        }
                        _ => {}
                    },
                    View::Apply => match key.code {
                        KeyCode::Esc => self.view = View::Review(0),
                        KeyCode::Enter
                            if key.modifiers.is_empty() && key.kind == KeyEventKind::Press =>
                        {
                            self.apply_changes()?
                        }
                        _ => {}
                    },
                    View::NewMod => match key.code {
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
        if matches!(self.view, View::Chat) {
            self.save_draft()?;
        }
        Ok(())
    }

    fn chat_key(&mut self, key: KeyEvent) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => self.quit = true,
            KeyCode::Char('p') if ctrl => self.view = View::Mods(self.active.unwrap_or(0)),
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
                self.input.input(key);
            }
        }
        Ok(())
    }

    fn picker_key(&mut self, key: KeyEvent, index: usize) -> Result<()> {
        match key.code {
            KeyCode::Up => self.view = View::Mods(index.saturating_sub(1)),
            KeyCode::Down => self.view = View::Mods((index + 1).min(self.mods.len())),
            KeyCode::Esc => self.view = View::Chat,
            KeyCode::Char('p') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.view = View::Chat
            }
            KeyCode::Char('d') if key.modifiers.is_empty() && index < self.mods.len() => {
                self.view = View::DeleteMod(index);
            }
            KeyCode::Enter if index == self.mods.len() => {
                self.view = View::NewMod;
                self.input = ui::name_input();
            }
            KeyCode::Enter => {
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
        if matches!(self.view, View::Chat | View::Queue(_))
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
        let code_mod = self.store.create_mod(self.project_id, name)?;
        self.auto_plans.insert(code_mod.id);
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

    fn delete_mod(&mut self, index: usize) -> Result<()> {
        let mod_id = self.mods[index].id;
        let previous = self.current_mod().map(|code_mod| code_mod.id);
        let workspace = self.mods[index]
            .execution
            .as_ref()
            .map(|execution| execution.workspace.clone());
        self.auto_plans.remove(&mod_id);
        self.workers.retain(|_, worker| worker.mod_id != mod_id);
        let selected = self.store.delete_mod(self.project_id, mod_id)?;
        self.mods.remove(index);
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
                "Mod deleted; working folder cleanup failed: {error}"
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

    fn toggle_worker(&mut self) -> Result<()> {
        let Some(active) = self.active else {
            return Ok(());
        };
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
                && let Err(error) = self.prepare_execution(active)
            {
                self.notice = Some(error.to_string());
                return Ok(());
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

    fn start_worker(&mut self, index: usize, role: Role) -> Result<()> {
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
                    &self.project,
                    &self.mods[index].description,
                ))
            })
        } else {
            None
        };
        match Worker::start(&self.project, &self.mods[index], record, role, routing) {
            Ok(worker) => {
                self.workers.insert(worker.id, worker);
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
        self.current_worker().is_some_and(|worker| {
            worker.role == Role::Executor
                && (worker.enabled
                    || matches!(
                        worker.status,
                        Status::Connecting
                            | Status::Starting
                            | Status::Running
                            | Status::Stopping
                            | Status::Checking
                    ))
        })
    }

    pub fn can_apply(&self) -> bool {
        !self.execution_busy()
            && self
                .review
                .as_ref()
                .is_some_and(|review| review.count() > 0)
            && self
                .current_mod()
                .and_then(|m| m.execution.as_ref())
                .is_some_and(|execution| {
                    execution.complete()
                        && execution.status == "review"
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

    fn apply_changes(&mut self) -> Result<()> {
        if !self.can_apply() {
            self.view = View::Review(0);
            return Ok(());
        }
        let code_mod = self.current_mod().unwrap();
        let id = code_mod.id;
        let root = &code_mod.execution.as_ref().unwrap().workspace;
        match self.review.as_ref().unwrap().apply(&self.project, root) {
            Ok(()) => {
                self.store.execution_status(id, "applied")?;
                let index = self.active.unwrap();
                self.mods[index].execution = self.store.execution(id)?;
                self.workers
                    .retain(|_, worker| worker.mod_id != id || worker.role != Role::Executor);
                self.review = None;
                self.view = View::Chat;
                self.notice = None;
            }
            Err(error) => {
                self.notice = Some(error.to_string());
                self.view = View::Review(0);
            }
        }
        Ok(())
    }

    fn poll_workers(&mut self) -> Result<()> {
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
            View::DeleteMod(index) => Some(self.mods[index].id),
            _ => None,
        };
        let reviewing_mod = matches!(self.view, View::Review(_) | View::Apply)
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
                    && reviewing_mod != Some(code_mod.id),
                &self.project,
            )?;
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::store::test_support::TestData;

    fn key(app: &mut App, code: KeyCode, modifiers: KeyModifiers) {
        app.handle(Event::Key(KeyEvent::new(code, modifiers)))
            .unwrap();
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
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(width, height)).unwrap();
        terminal
            .draw(|frame| {
                ui::draw(frame, app, sprout::Pose::Idle);
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
        let root = data.0.join("workspace");
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

    #[test]
    fn review_requires_confirmation_preserves_draft_and_refuses_project_conflicts() {
        let (_data, mut app, _root) = execution_app();
        key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(app.can_apply());
        assert!(
            rows(&screen(&mut app, 100, 30))
                .join("\n")
                .contains("changes · 1 files")
        );
        key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        paste(&mut app, "\n");
        let mut repeat = KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE);
        repeat.kind = KeyEventKind::Repeat;
        app.handle(Event::Key(repeat)).unwrap();
        assert_eq!(
            std::fs::read_to_string(app.project.join("hello.sh")).unwrap(),
            "old\n"
        );
        let narrow = rows(&screen(&mut app, 48, 24)).join("\n");
        assert!(narrow.contains("block applying"));
        key(&mut app, KeyCode::Esc, KeyModifiers::NONE);
        key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        std::fs::write(app.project.join("hello.sh"), "user\n").unwrap();
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert!(
            app.worker_error()
                .unwrap()
                .contains("changed in the project")
        );
        assert_eq!(
            std::fs::read_to_string(app.project.join("hello.sh")).unwrap(),
            "user\n"
        );
        std::fs::write(app.project.join("hello.sh"), "old\n").unwrap();
        key(&mut app, KeyCode::Char('a'), KeyModifiers::NONE);
        key(&mut app, KeyCode::Enter, KeyModifiers::NONE);
        assert_eq!(
            std::fs::read_to_string(app.project.join("hello.sh")).unwrap(),
            "new\n"
        );
        assert_eq!(
            app.current_mod()
                .unwrap()
                .execution
                .as_ref()
                .unwrap()
                .status,
            "applied"
        );
        assert_eq!(app.input.lines(), ["keep this draft"]);
        assert!(matches!(app.view, View::Chat));
    }

    #[test]
    fn changes_after_final_verification_require_rechecking_even_after_reopening() {
        let (data, app, root) = execution_app();
        let project = app.project.clone();
        drop(app);
        std::fs::write(root.join("work/another.txt"), "not verified").unwrap();
        let mut app = App::load(project, false, data.store()).unwrap();
        key(&mut app, KeyCode::Char('d'), KeyModifiers::CONTROL);
        assert!(!app.can_apply());
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
        assert!(compact.contains("execution writes only to the mod's working folder"));
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
        let project = PathBuf::from("/project");
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
        let project = PathBuf::from("/project");
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
        let mut app = App::load(PathBuf::from("/project"), false, data.store()).unwrap();
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
        let project = PathBuf::from("/project");
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
            queued(&App::load(PathBuf::from("/project"), false, data.store()).unwrap()).is_empty()
        );
    }
    #[test]
    fn marked_instructions_steer_in_queue_order_and_close_an_empty_manager() {
        let data = TestData::new();
        let project = PathBuf::from("/steering-project");
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
        let mut app = App::load(PathBuf::from("/cancel-delete"), false, data.store()).unwrap();
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
        let project = PathBuf::from("/delete-mods");
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
