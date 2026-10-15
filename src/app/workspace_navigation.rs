use super::{
    App, View,
    controls::{Control, WorkspaceView},
};
use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::Rect,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Key {
    Task(i64),
    Planned(usize),
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Composer,
    Content,
}

#[derive(Default)]
pub struct Inspector {
    pub key: Option<Key>,
    pub area: Rect,
    pub offset: u16,
    pub maximum: u16,
    pub tab: WorkspaceView,
    pub offsets: [u16; 5],
    pub finding: Option<u32>,
}

impl Inspector {
    pub fn set_tab(&mut self, tab: WorkspaceView) {
        self.offsets[self.tab as usize] = self.offset;
        self.tab = tab;
        self.offset = self.offsets[tab as usize];
    }

    pub fn select(&mut self, key: Option<Key>) {
        if self.key != key {
            self.key = key;
            self.offset = 0;
            self.offsets = [0; 5];
        }
    }

    pub fn scroll(&mut self, delta: i32) {
        self.offset = (i32::from(self.offset) + delta).clamp(0, i32::from(self.maximum)) as u16;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum AgentDetail {
    ActivityHidden(i64),
    AllTimings,
}

#[derive(Default)]
pub struct Workspace {
    pub context: Option<(i64, String)>,
    pub focus: Focus,
    pub inspector: Inspector,
    pub selected: Option<Key>,
    pub history: bool,
    pub message_toggles: std::collections::HashSet<(u128, usize)>,
    pub message_cursor: Option<(u128, usize)>,
    pub message_controls: Vec<(u128, usize, usize)>,
    pub agent_toggles: std::collections::HashSet<AgentDetail>,
    pub agent_cursor: Option<AgentDetail>,
    pub agent_controls: Vec<(AgentDetail, usize)>,
}

impl Workspace {
    pub fn reset_for(&mut self, context: (i64, String)) {
        if self.context.as_ref().map(|value| value.0) != Some(context.0) {
            *self = Self::default();
        } else if self.context.as_ref() != Some(&context) {
            self.selected = None;
            self.inspector.select(None);
        }
        self.context = Some(context);
    }

    pub fn jump(&mut self) {
        self.selected = None;
        self.inspector = Inspector::default();
        self.focus = Focus::Content;
    }
}

impl App {
    pub fn has_workspace(&self) -> bool {
        self.engine.view == View::Chat && !self.plan_details && self.current_mod().is_some()
    }

    pub fn valid_target(&self, target: Key) -> bool {
        match target {
            Key::Task(id) => self.inspect_task(id).is_some(),
            Key::Planned(index) => self
                .current_mod()
                .and_then(|m| m.planning.as_ref())
                .and_then(|p| p.plan.as_ref())
                .is_some_and(|p| index < p.tasks.len()),
        }
    }

    pub(super) fn workspace_key(&mut self, key: KeyEvent) -> rusqlite::Result<bool> {
        if !self.has_workspace() {
            return Ok(false);
        }
        if key.code == KeyCode::Char('l') && key.modifiers == KeyModifiers::CONTROL {
            self.activate_control(Control::Current)?;
            return Ok(true);
        }
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.workspace.focus = if self.workspace.focus == Focus::Composer {
                Focus::Content
            } else {
                Focus::Composer
            };
            return Ok(true);
        }
        if !key.modifiers.is_empty() || self.workspace.focus == Focus::Composer {
            return Ok(false);
        }
        match key.code {
            KeyCode::Char(c @ '1'..='5') => {
                self.activate_control(Control::Tab(WorkspaceView::ALL[c as usize - '1' as usize]))?
            }
            KeyCode::Char('h') if self.workspace.inspector.tab == WorkspaceView::Conversation => {
                self.activate_control(Control::History)?
            }
            KeyCode::Char('e') => self.show_evidence = !self.show_evidence,
            KeyCode::Char('[' | ']')
                if self.workspace.inspector.tab == WorkspaceView::Conversation =>
            {
                let controls = &self.workspace.message_controls;
                let current = controls.iter().position(|(message, segment, _)| {
                    self.workspace.message_cursor == Some((*message, *segment))
                });
                let next = match (current, key.code) {
                    (Some(index), KeyCode::Char('[')) => index.saturating_sub(1),
                    (Some(index), _) => (index + 1).min(controls.len().saturating_sub(1)),
                    _ => 0,
                };
                if let Some(&(message, segment, row)) = controls.get(next) {
                    self.workspace.message_cursor = Some((message, segment));
                    let inspector = &mut self.workspace.inspector;
                    if row < usize::from(inspector.offset)
                        || row >= usize::from(inspector.offset) + usize::from(inspector.area.height)
                    {
                        inspector.offset = (row as u16).min(inspector.maximum);
                    }
                }
            }
            KeyCode::Char('[' | ']') if self.workspace.inspector.tab == WorkspaceView::Agents => {
                let controls = &self.workspace.agent_controls;
                let current = controls
                    .iter()
                    .position(|(detail, _)| self.workspace.agent_cursor == Some(*detail));
                let index = match (current, key.code) {
                    (Some(index), KeyCode::Char('[')) => index.saturating_sub(1),
                    (Some(index), _) => (index + 1).min(controls.len().saturating_sub(1)),
                    _ => 0,
                };
                if let Some(&(detail, row)) = controls.get(index) {
                    self.workspace.agent_cursor = Some(detail);
                    let inspector = &mut self.workspace.inspector;
                    if row < usize::from(inspector.offset)
                        || row >= usize::from(inspector.offset) + usize::from(inspector.area.height)
                    {
                        inspector.offset = (row as u16).min(inspector.maximum);
                    }
                }
            }
            KeyCode::Char('[' | ']') if self.workspace.inspector.tab == WorkspaceView::Findings => {
                let ids = self.review_finding_ids();
                let index = ids
                    .iter()
                    .position(|id| Some(*id) == self.workspace.inspector.finding)
                    .unwrap_or(0);
                let next = if key.code == KeyCode::Char('[') {
                    index.saturating_sub(1)
                } else {
                    index.saturating_add(1)
                };
                if let Some(id) = ids.get(next.min(ids.len().saturating_sub(1))) {
                    self.activate_control(Control::Finding(*id))?;
                }
            }
            KeyCode::Up => self.workspace.inspector.scroll(-1),
            KeyCode::Down => self.workspace.inspector.scroll(1),
            KeyCode::PageUp => self
                .workspace
                .inspector
                .scroll(-i32::from(self.workspace.inspector.area.height)),
            KeyCode::PageDown => self
                .workspace
                .inspector
                .scroll(i32::from(self.workspace.inspector.area.height)),
            KeyCode::Home => self.workspace.inspector.offset = 0,
            KeyCode::End => self.workspace.inspector.offset = self.workspace.inspector.maximum,
            KeyCode::Left | KeyCode::Esc if self.workspace.selected.is_some() => {
                self.activate_control(Control::Back)?
            }
            KeyCode::Esc => self.workspace.focus = Focus::Composer,
            KeyCode::Enter
                if self.workspace.inspector.tab == WorkspaceView::Conversation
                    && self
                        .workspace
                        .message_controls
                        .iter()
                        .any(|(message, segment, _)| {
                            self.workspace.message_cursor == Some((*message, *segment))
                        }) =>
            {
                let (message, segment) = self.workspace.message_cursor.unwrap();
                self.activate_control(Control::Message(message, segment))?;
            }
            KeyCode::Enter if self.workspace.inspector.tab == WorkspaceView::Agents => {
                if let Some(detail) = self.workspace.agent_cursor.filter(|key| {
                    self.workspace
                        .agent_controls
                        .iter()
                        .any(|(detail, _)| detail == key)
                }) {
                    self.activate_control(Control::AgentDetail(detail))?;
                }
            }
            KeyCode::Enter => self.open_workspace_details()?,
            _ => {
                self.workspace.focus = Focus::Composer;
                return Ok(false);
            }
        }
        Ok(true)
    }

    pub(super) fn open_workspace_details(&mut self) -> rusqlite::Result<()> {
        if let Some(Key::Task(id)) = self
            .workspace
            .selected
            .filter(|key| self.valid_target(*key))
        {
            self.engine.view = View::Task(id, 0);
        }
        Ok(())
    }
}
