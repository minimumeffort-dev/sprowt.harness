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
    sprout,
    store::{CodeMod, Store, source_id},
    ui,
    worker::{Status, Worker},
};

#[derive(Clone, Copy)]
pub enum View {
    Chat,
    Mods(usize),
    NewMod,
    Queue(usize),
    EditQueue(usize),
}

pub struct App {
    pub project: PathBuf,
    pub input: TextArea<'static>,
    pub mods: Vec<CodeMod>,
    pub view: View,
    pub history_offset: u16,
    pub page_size: u16,
    pub queue_selection: BTreeSet<i64>,
    workers: BTreeMap<i64, Worker>,
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
            queue_selection: BTreeSet::new(),
            workers: BTreeMap::new(),
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
                    self.input.insert_str(text.replace(['\r', '\n'], " "));
                }
                View::Mods(_) | View::Queue(_) => {}
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
                    View::Queue(index) => self.queue_key(key, index)?,
                    View::EditQueue(index) => self.edit_key(key, index)?,
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
                        KeyCode::Char('j') if ctrl => {}
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
        let name = self.input.lines().join(" ");
        let name = name.trim();
        if name.is_empty() {
            return Ok(());
        }
        let code_mod = self.store.create_mod(self.project_id, name)?;
        self.mods.push(code_mod);
        self.active = Some(self.mods.len() - 1);
        self.notice = None;
        self.history_offset = 0;
        self.view = View::Chat;
        self.restore_input();
        self.celebrate();
        Ok(())
    }

    pub fn current_worker(&self) -> Option<&Worker> {
        let mod_id = self.current_mod()?.id;
        self.workers.values().find(|worker| worker.mod_id == mod_id)
    }

    pub fn worker_error(&self) -> Option<&str> {
        self.current_worker()
            .and_then(|worker| worker.error.as_deref())
            .or(self.notice.as_deref())
    }

    fn toggle_worker(&mut self) -> Result<()> {
        let Some(mod_id) = self.current_mod().map(|code_mod| code_mod.id) else {
            return Ok(());
        };
        if let Some(worker) = self
            .workers
            .values_mut()
            .find(|worker| worker.mod_id == mod_id)
            && worker.status != Status::Failed
        {
            worker.toggle();
            return Ok(());
        }
        let record = self.store.worker(mod_id)?;
        self.workers.remove(&record.id);
        match Worker::start(&self.project, mod_id, record) {
            Ok(worker) => {
                self.workers.insert(worker.id, worker);
                self.notice = None;
            }
            Err(error) => self.notice = Some(error.to_string()),
        }
        Ok(())
    }

    fn poll_workers(&mut self) -> Result<()> {
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
        for worker in self.workers.values_mut() {
            let code_mod = self
                .mods
                .iter_mut()
                .find(|code_mod| code_mod.id == worker.mod_id)
                .unwrap();
            worker.poll(&mut self.store, code_mod, editing_mod != Some(code_mod.id))?;
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
        assert!(app.current_mod().unwrap().messages.is_empty());
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
        assert!(reopened.current_mod().unwrap().messages.is_empty());

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
        assert!(reopened.current_mod().unwrap().messages.is_empty());
    }
}
