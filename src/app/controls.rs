use super::{
    Action, App, View,
    workspace_navigation::{Focus, Key},
};
use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::{Position, Rect},
};

pub use crate::engine::workspace_view::WorkspaceView;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Action(Action),
    MenuAction(Action),
    PickMod(usize),
    ProjectNew(i64),
    ProjectMenu(i64),
    Format,
    FormatItem(super::formatting::Format),
    Submit,
    Newline,
    More,
    Inspect,
    History,
    Message(u128, usize),
    FullEvidence,
    Current,
    Drill(Key),
    Tab(WorkspaceView),
    TaskChecks(i64),
    Finding(u32),
    Evidence,
    Back,
    NavigateBack,
    NavigateForward,
}

#[derive(Default)]
pub struct Controls {
    pub hits: Vec<(Rect, Control)>,
    view: Option<View>,
}

impl Controls {
    pub fn begin(&mut self, view: View) {
        self.hits.clear();
        self.view = Some(view);
    }

    pub fn add(&mut self, area: Rect, control: Control) {
        if !area.is_empty() {
            self.hits.push((area, control));
        }
    }

    pub fn at(&self, view: View, position: Position) -> Option<Control> {
        (self.view == Some(view))
            .then(|| {
                self.hits
                    .iter()
                    .rev()
                    .find(|(r, _)| r.contains(position))
                    .map(|(_, c)| *c)
            })
            .flatten()
    }
}

impl App {
    pub(super) fn activate_control(&mut self, control: Control) -> rusqlite::Result<()> {
        match control {
            Control::Action(action) => self.perform_action(action)?,
            Control::MenuAction(action) => {
                if matches!(self.engine.view, View::Actions(_)) {
                    self.actions_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), action)?;
                }
            }
            Control::PickMod(index) => {
                if matches!(self.engine.view, View::Mods(_)) && index < self.project_rows().len() {
                    self.engine.view = View::Mods(index);
                    self.picker_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), index)?;
                }
            }
            Control::ProjectNew(id) => {
                self.project_command(crate::engine::api::Command::BeginProjectCreation {
                    project_id: id,
                })
            }
            Control::ProjectMenu(id) => {
                self.project_picker = super::projects::PickerPanel::ProjectMenu(id);
            }
            Control::Format => self.open_formatting(),
            Control::FormatItem(format) => self.choose_format(format),
            Control::Submit => {
                self.workspace.focus = Focus::Composer;
                match self.engine.view {
                    View::Chat => self.submit()?,
                    View::NewMod => self.create_mod()?,
                    _ => {}
                }
            }
            Control::Newline => {
                if self.engine.view == View::NewMod
                    || self.engine.view == View::Chat && !self.read_only()
                {
                    self.workspace.focus = Focus::Composer;
                    self.input.insert_newline();
                }
            }
            Control::More => self.open_actions(),
            Control::Inspect => self.perform_action(Action::History)?,
            Control::History => self.workspace.history = !self.workspace.history,
            Control::Message(message, segment) => {
                let key = (message, segment);
                if !self.workspace.message_toggles.remove(&key) {
                    self.workspace.message_toggles.insert(key);
                }
                self.workspace.message_cursor = Some(key);
                self.workspace.focus = Focus::Content;
            }
            Control::FullEvidence => self.open_workspace_details()?,
            Control::Current => self.workspace.jump(),
            Control::Drill(key) => {
                if self.valid_target(key) {
                    self.workspace.selected = Some(key);
                    self.workspace.inspector.select(Some(key));
                    self.workspace
                        .inspector
                        .set_tab(WorkspaceView::Conversation);
                    self.workspace.focus = Focus::Content;
                }
            }
            Control::Tab(tab) => {
                self.workspace.selected = None;
                self.workspace.inspector.select(None);
                self.workspace.inspector.set_tab(tab);
                self.workspace.focus = Focus::Content;
            }
            Control::TaskChecks(id) => {
                if self.valid_target(Key::Task(id)) {
                    self.workspace.selected = Some(Key::Task(id));
                    self.workspace.inspector.select(Some(Key::Task(id)));
                    self.workspace.inspector.set_tab(WorkspaceView::Checks);
                    self.workspace.focus = Focus::Content;
                }
            }
            Control::Finding(id) => {
                if self.review_finding_ids().contains(&id) {
                    self.workspace.inspector.finding = Some(id);
                    self.workspace.inspector.offset = 0;
                    self.workspace.focus = Focus::Content;
                }
            }
            Control::Evidence => self.show_evidence = !self.show_evidence,
            Control::NavigateBack => {
                self.with_engine(|engine| engine.navigate(true))?;
            }
            Control::NavigateForward => {
                self.with_engine(|engine| engine.navigate(false))?;
            }
            Control::Back => {
                self.workspace.selected = None;
                self.workspace.inspector.select(None);
                self.workspace.focus = Focus::Content;
            }
        }
        self.controls.hits.clear();
        Ok(())
    }

    pub fn review_finding_ids(&self) -> Vec<u32> {
        self.current_mod()
            .and_then(|m| m.agent_review.as_ref())
            .map_or_else(Vec::new, |review| {
                if review.issues.is_empty() {
                    review.report.as_ref().map_or_else(Vec::new, |report| {
                        report
                            .findings
                            .iter()
                            .enumerate()
                            .map(|(i, finding)| finding.id.unwrap_or(i as u32 + 1))
                            .collect()
                    })
                } else {
                    review.ordered_issues().map(|issue| issue.id).collect()
                }
            })
    }
}
