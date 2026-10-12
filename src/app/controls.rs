use super::{
    Action, App, View,
    timeline::{Focus, Key},
};
use ratatui::{
    crossterm::event::{KeyCode, KeyEvent, KeyModifiers},
    layout::{Position, Rect},
};

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum EvidenceTab {
    #[default]
    Summary,
    Checks,
    Changes,
    Activity,
}

impl EvidenceTab {
    pub const ALL: [Self; 4] = [Self::Summary, Self::Checks, Self::Changes, Self::Activity];

    pub fn label(self) -> &'static str {
        match self {
            Self::Summary => "Summary",
            Self::Checks => "Checks",
            Self::Changes => "Changes",
            Self::Activity => "Activity",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Control {
    Action(Action),
    MenuAction(Action),
    PickMod(usize),
    Format,
    FormatItem(super::formatting::Format),
    Submit,
    Newline,
    More,
    Inspect,
    FullEvidence,
    Current,
    Overview,
    Stage(Key),
    Drill(Key),
    Tab(EvidenceTab),
    Finding(u32),
    Evidence,
    Back,
    Expand,
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
                if matches!(self.engine.view, View::Mods(_)) && index <= self.picker_indices().len()
                {
                    self.picker_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), index)?;
                }
            }
            Control::Format => self.open_formatting(),
            Control::FormatItem(format) => self.choose_format(format),
            Control::Submit => {
                self.timeline.focus = Focus::Composer;
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
                    self.timeline.focus = Focus::Composer;
                    self.input.insert_newline();
                }
            }
            Control::More => self.open_actions(),
            Control::Inspect => self.perform_action(Action::History)?,
            Control::FullEvidence => self.open_timeline_details()?,
            Control::Current => self.timeline.jump(),
            Control::Overview => {
                self.timeline.overview = !self.timeline.overview;
                if matches!(
                    self.timeline.selected,
                    Some(Key::Task(_) | Key::Planned(_) | Key::Failure(_))
                ) {
                    self.timeline.expanded.insert(Key::Build, true);
                }
                self.timeline.focus = Focus::Timeline;
                self.timeline.top = 0;
                self.timeline.anchor = None;
                self.timeline.reveal = true;
            }
            Control::Stage(key) => {
                if matches!(key, Key::Task(_) | Key::Planned(_) | Key::Failure(_)) {
                    self.timeline.expanded.insert(Key::Build, true);
                }
                if self.timeline_items().iter().any(|item| item.key == key) {
                    self.timeline.selected = Some(key);
                    self.timeline.inspector.select(Some(key));
                    self.timeline.focus = Focus::Timeline;
                    self.timeline.held = true;
                }
            }
            Control::Drill(key) => {
                self.activate_control(Control::Stage(key))?;
                self.timeline.focus = Focus::Inspector;
            }
            Control::Tab(tab) => {
                self.timeline.inspector.set_tab(tab);
                self.timeline.focus = Focus::Inspector;
                self.timeline.held = true;
            }
            Control::Finding(id) => {
                if self.review_finding_ids().contains(&id) {
                    self.timeline.inspector.finding = Some(id);
                    self.timeline.inspector.offset = 0;
                    self.timeline.focus = Focus::Inspector;
                    self.timeline.held = true;
                }
            }
            Control::Evidence => self.show_evidence = !self.show_evidence,
            Control::Back => self.timeline.focus = Focus::Timeline,
            Control::Expand => {
                self.timeline.focus = Focus::Timeline;
                self.timeline_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE))?;
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

    pub fn local_status(&self) -> String {
        if matches!(self.composer_view(), View::NewMod) {
            return "New codemod · not submitted".into();
        }
        let workers = self.active_workers().count();
        if workers > 0 {
            return format!(
                "{workers} {} · work in progress",
                if workers == 1 { "worker" } else { "workers" }
            );
        }
        if self.execution_busy()
            || self.current_mod().is_some_and(|m| {
                self.engine.auto_plans.contains(&m.id)
                    || self.engine.auto_runs.contains(&m.id)
                    || self.engine.auto_reviews.contains(&m.id)
            })
        {
            return "Starting work…".into();
        }
        if self.engine.project_job.is_some()
            || self
                .current_mod()
                .is_some_and(|m| self.engine.git_jobs.contains_key(&m.id))
        {
            return "Operation in progress".into();
        }
        match self.current_mod() {
            Some(m)
                if matches!(self.engine.view, View::Chat)
                    && self.input.lines().join("\n") != m.draft =>
            {
                "Saving draft…".into()
            }
            Some(_) => "Work saved locally".into(),
            None => "New codemod".into(),
        }
    }
}
