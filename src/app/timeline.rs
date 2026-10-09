use super::{Action, App, View};
use crate::{plan::Role, worker::Status};
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Key {
    Request,
    Plan,
    Build,
    Planned(usize),
    Task(i64),
    Failure(i64),
    Repair(i64),
    Checks,
    Review,
    Publish,
    Git,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum State {
    Done,
    Active,
    Attention,
    Waiting,
    Previous,
}

pub struct Item {
    pub key: Key,
    pub title: String,
    pub state: State,
    pub branch: bool,
    pub detail: Vec<String>,
    pub inspect: Option<View>,
    pub action: Option<Action>,
}

impl Item {
    pub fn expanded_by_default(&self) -> bool {
        matches!(self.state, State::Active | State::Attention)
            || self.branch && self.state == State::Waiting
            || self.key == Key::Build && self.state != State::Done
    }

    fn new(key: Key, title: impl Into<String>, state: State) -> Self {
        Self {
            key,
            title: title.into(),
            state,
            branch: false,
            detail: Vec::new(),
            inspect: None,
            action: None,
        }
    }
}

#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub enum Focus {
    #[default]
    Composer,
    Timeline,
    Inspector,
}

#[derive(Default)]
pub struct Inspector {
    pub key: Option<Key>,
    pub area: ratatui::layout::Rect,
    pub offset: u16,
    pub maximum: u16,
}

impl Inspector {
    pub fn scroll(&mut self, delta: i32) {
        self.offset = (i32::from(self.offset) + delta).clamp(0, i32::from(self.maximum)) as u16;
    }
}

#[derive(Default)]
pub struct Timeline {
    pub context: Option<(i64, String)>,
    pub focus: Focus,
    pub inspector: Inspector,
    pub selected: Option<Key>,
    pub expanded: BTreeMap<Key, bool>,
    pub top: usize,
    pub held: bool,
    pub reveal: bool,
    pub rows: Vec<(Key, usize, usize)>,
    pub anchor: Option<(Key, usize)>,
    pub jump_area: ratatui::layout::Rect,
}

impl Timeline {
    pub fn reset_for(&mut self, context: (i64, String)) {
        if self.context.as_ref() != Some(&context) {
            *self = Self {
                context: Some(context),
                ..Self::default()
            };
        }
    }

    pub fn scroll(&mut self, delta: i32) {
        self.top = self.top.saturating_add_signed(delta as isize);
        self.held = true;
        self.reveal = false;
        self.anchor = self
            .rows
            .iter()
            .rev()
            .find(|(_, start, _)| *start <= self.top)
            .map(|(key, start, _)| (*key, self.top - start));
    }

    pub fn jump(&mut self) {
        self.held = false;
        self.anchor = None;
        self.selected = None;
        self.reveal = true;
    }
}

impl App {
    pub fn has_timeline(&self) -> bool {
        self.view == View::Chat
            && !self.plan_details
            && self.current_mod().is_some_and(|m| m.planning.is_some())
    }

    pub fn timeline_items(&self) -> Vec<Item> {
        use State::*;
        let Some(m) = self.current_mod() else {
            return Vec::new();
        };
        let Some(planning) = m.planning.as_ref() else {
            return Vec::new();
        };
        let execution = m.execution.as_ref();
        let dock = self.action_dock();
        let request = m
            .messages
            .iter()
            .rev()
            .find(|message| message.role == "user" && message.task.is_none())
            .map_or(m.description.as_str(), |message| message.body.as_str());
        let mut asked = Item::new(Key::Request, request, Done);
        asked.inspect = Some(View::History(0));
        let Some(plan) = planning
            .plan
            .as_ref()
            .filter(|_| planning.status == "ready")
        else {
            let worker = self.active_workers().find(|w| w.role == Role::Planner);
            let mut planning = Item::new(
                Key::Plan,
                dock.status,
                if worker.is_some() {
                    Active
                } else if dock.error.is_some() {
                    Attention
                } else {
                    Waiting
                },
            );
            planning.inspect = Some(View::History(0));
            planning.detail.extend(dock.error);
            if let Some(worker) = worker {
                planning.detail.push(worker.label(None));
                planning.detail.push(worker.timeline_timing());
            }
            if let Some(latest) = m.messages.iter().rev().find(|msg| msg.role == "planner") {
                planning.detail.push(latest.body.clone());
            }
            planning.action = dock.primary;
            return vec![asked, planning];
        };
        let mut planned = Item::new(
            Key::Plan,
            format!("Plan ready · {} tasks", plan.tasks.len()),
            Done,
        );
        planned.detail.push(plan.summary.clone());
        let done = execution.map_or(0, |e| e.tasks.iter().filter(|t| t.status == "done").count());
        let mut build = Item::new(
            Key::Build,
            format!(
                "Implementation · {done}/{} tasks finished",
                plan.tasks.len()
            ),
            if execution.is_some_and(|e| e.complete()) {
                Done
            } else if self.execution_busy() {
                Active
            } else {
                Waiting
            },
        );
        build.inspect = Some(View::Tasks(0));
        build.action = dock.primary.filter(|a| *a == Action::Run && !m.user_paused);
        let active = self
            .active_workers()
            .filter(|w| w.role == Role::Executor && w.task_run_id().is_some())
            .count();
        if active > 1 {
            build
                .detail
                .push(format!("{active} tasks running in parallel"));
        }
        let show_tasks = self
            .timeline
            .expanded
            .get(&Key::Build)
            .copied()
            .unwrap_or(build.state != Done);
        let mut items = vec![asked, planned, build];
        let mut repairs = Vec::new();
        for (index, planned) in plan.tasks.iter().enumerate() {
            let dependencies = planned
                .depends_on
                .iter()
                .filter_map(|id| plan.tasks.iter().position(|t| &t.id == id))
                .map(|i| (i + 1).to_string())
                .collect::<Vec<_>>();
            let dependency = format!(
                "after task{} {}",
                if dependencies.len() == 1 { "" } else { "s" },
                dependencies.join(", ")
            );
            let Some(run) =
                execution.and_then(|e| e.tasks.iter().find(|r| r.task_id == planned.id))
            else {
                if show_tasks {
                    let mut item = Item::new(
                        Key::Planned(index),
                        format!("{}. {} · Waiting", index + 1, planned.title),
                        Waiting,
                    );
                    item.branch = true;
                    item.detail.push(planned.outcome.clone());
                    if !dependencies.is_empty() {
                        item.detail.push(dependency);
                    }
                    items.push(item);
                }
                continue;
            };
            let Some(task) = self.inspect_task(run.id) else {
                continue;
            };
            let worker = run
                .worker
                .and_then(|id| self.workers.get(&id))
                .filter(|w| w.task_run_id() == Some(run.id) || task.state == "Connecting worker");
            let state = if task.error.is_some()
                || matches!(
                    task.state,
                    "Check failed"
                        | "Final checks failed"
                        | "Worker stopped"
                        | "Blocked"
                        | "Needs your answer"
                        | "Needs network access"
                ) {
                Attention
            } else if worker.is_some_and(|w| w.busy()) {
                Active
            } else if run.status == "done" {
                Done
            } else {
                Waiting
            };
            let repairing = run.status != "done"
                && (run.check_repair
                    || run.review_feedback.is_some()
                    || run.conflict.is_some()
                    || !run.verification_feedback.is_empty());
            let mut item = Item::new(
                if repairing {
                    Key::Repair(run.id)
                } else {
                    Key::Task(run.id)
                },
                format!(
                    "{}{}. {} · {}",
                    if repairing { "Repair " } else { "" },
                    task.number,
                    task.task.title,
                    task.state
                ),
                state,
            );
            item.branch = true;
            item.inspect = Some(View::Task(run.id, 0));
            item.detail.push(task.identity);
            if let Some(worker) = worker.filter(|w| w.busy()) {
                item.detail.push(worker.timeline_timing());
                if let Some(progress) = worker.progress() {
                    item.detail.push(progress.into());
                }
            }
            if !task.note.is_empty() {
                item.detail.push(task.note.clone());
            }
            if !dependencies.is_empty() && task.note.is_empty() {
                item.detail.push(dependency);
            }
            if let Some(error) = task.error.filter(|error| *error != task.note) {
                item.detail.push(error.into());
            }
            if let Some(check) = task
                .checks
                .iter()
                .find(|c| c.failed())
                .filter(|_| !repairing)
            {
                item.detail
                    .push(format!("{}: {}", task.checks_label, check.brief()));
            }
            if let Some(latest) = task.latest {
                item.detail.push(format!(
                    "Latest update · {}",
                    latest.body.lines().take(2).collect::<Vec<_>>().join(" ")
                ));
            } else {
                item.detail.push(task.task.outcome.clone());
            }
            item.action = match dock.primary {
                Some(Action::RetryTask(id) | Action::FixCheck(id)) if id == run.id => dock.primary,
                Some(Action::Network)
                    if self.pending_network().is_some_and(|a| a.task == run.id) =>
                {
                    dock.primary
                }
                _ => None,
            };
            if repairing {
                if let Some(evidence) = run
                    .verification_feedback
                    .iter()
                    .find(|c| c.failed())
                    .map(|c| c.brief())
                    .or_else(|| run.review_feedback.clone())
                    .filter(|text| !text.trim().is_empty())
                {
                    let mut failure = Item::new(
                        Key::Failure(run.id),
                        format!("Task {} · previous failure", task.number),
                        Previous,
                    );
                    failure.branch = true;
                    failure.detail.push(evidence);
                    repairs.push(failure);
                }
                repairs.push(item);
            } else if show_tasks || matches!(state, Active | Attention) {
                items.push(item);
            }
        }
        items.extend(repairs);
        let checked = execution
            .is_some_and(|e| e.complete() && e.status == "review" && e.fingerprint.is_some());
        let checking = execution.is_some_and(|e| e.status == "verifying")
            && self.active_workers().any(|w| w.status == Status::Checking);
        let failed = execution.is_some_and(|e| e.checks.iter().any(|c| c.failed()));
        let mut checks = Item::new(
            Key::Checks,
            if checked {
                "Combined checks passed"
            } else if checking {
                "Combined checks running"
            } else if failed {
                "Combined checks failed"
            } else {
                "Combined checks · after implementation / repair"
            },
            if checked {
                Done
            } else if checking {
                Active
            } else if failed {
                Attention
            } else {
                Waiting
            },
        );
        checks.inspect = Some(View::Checks(0));
        if let Some(e) = execution
            && !e.checks.is_empty()
        {
            checks.detail.push(format!(
                "{} passed · {} failed · {} not run{}",
                e.checks.iter().filter(|c| c.exit_code == Some(0)).count(),
                e.checks.iter().filter(|c| c.failed()).count(),
                e.checks.iter().filter(|c| c.skipped()).count(),
                if !checked && !checking {
                    " · saved results"
                } else {
                    ""
                }
            ));
            checks.detail.extend(
                e.checks
                    .iter()
                    .filter(|c| c.failed())
                    .take(1)
                    .map(|c| c.brief()),
            );
        }
        if checking
            && let Some(worker) = self.active_workers().find(|w| w.status == Status::Checking)
        {
            checks.detail.push(worker.timeline_timing());
            if let Some(progress) = worker.progress() {
                checks.detail.push(progress.into());
            }
        }
        checks.action = dock
            .primary
            .filter(|a| matches!(a, Action::FixCheck(_) | Action::Retry) && failed);
        items.push(checks);
        let review = m.agent_review.as_ref();
        let current = review.is_some_and(|r| r.current(m));
        let reviewing = self.active_workers().any(|w| w.role == Role::Reviewer);
        let review_state = if reviewing {
            Active
        } else if current && review.is_some_and(|r| r.status == "clean") {
            Done
        } else if current
            && review
                .is_some_and(|r| matches!(r.status.as_str(), "findings" | "paused" | "blocked"))
        {
            Attention
        } else {
            Waiting
        };
        let mut reviewed = Item::new(
            Key::Review,
            if reviewing {
                "Reviewing changes".into()
            } else if let Some(r) = review {
                if current {
                    r.label()
                } else {
                    "Review · changes need a fresh review".into()
                }
            } else {
                "Review · after combined checks".into()
            },
            review_state,
        );
        reviewed.inspect = Some(View::Findings(0));
        if let Some(report) = review.and_then(|r| r.report.as_ref()) {
            reviewed.detail.push(format!(
                "{}{}",
                if current { "" } else { "Previous review: " },
                report.summary
            ));
        }
        if let Some(worker) = self.active_workers().find(|w| w.role == Role::Reviewer) {
            reviewed.detail.push(worker.label(None));
            reviewed.detail.push(worker.timeline_timing());
        }
        reviewed.action = dock
            .primary
            .filter(|a| matches!(a, Action::FixIssues | Action::Findings | Action::Review));
        items.push(reviewed);
        let mut published = Item::new(
            Key::Publish,
            if self.published() {
                "PR published"
            } else if self.git_state().is_some_and(|g| g.pr.is_some()) {
                "PR open · update pending"
            } else if checked && review_state == Done {
                "Ready to publish"
            } else {
                "Publish PR · after checks and review"
            },
            if self.published() { Done } else { Waiting },
        );
        if let Some(url) = self.git_state().and_then(|g| g.pr.clone()) {
            published.detail.push(url);
        }
        published.action = dock.primary.filter(|a| *a == Action::Publish);
        published.inspect = Some(View::History(0));
        items.push(published);
        if let Some(activity) = self
            .git_activity()
            .filter(|a| *a != "checking target branch")
        {
            items.push(Item::new(Key::Git, activity, Active));
        } else if m.closed
            || m.user_paused
            || matches!(
                dock.primary,
                Some(Action::Update | Action::RetryGit | Action::Failure)
            )
        {
            let mut status = Item::new(
                Key::Git,
                dock.status,
                if dock.error.is_some() {
                    Attention
                } else {
                    Waiting
                },
            );
            status.detail.extend(dock.error);
            status.action = dock.primary;
            items.push(status);
        }
        items
    }

    pub(super) fn timeline_key(&mut self, key: KeyEvent) -> rusqlite::Result<bool> {
        if !self.has_timeline() {
            return Ok(false);
        }
        let split = !self.timeline.inspector.area.is_empty();
        if matches!(key.code, KeyCode::Tab | KeyCode::BackTab) {
            self.timeline.focus = match (self.timeline.focus, key.code == KeyCode::BackTab, split) {
                (Focus::Composer, true, true) | (Focus::Timeline, false, true) => Focus::Inspector,
                (Focus::Composer, _, _) | (Focus::Inspector, true, _) => Focus::Timeline,
                _ => Focus::Composer,
            };
            self.timeline.reveal = self.timeline.focus == Focus::Timeline;
            if self.timeline.focus == Focus::Inspector {
                self.timeline.held = true;
            }
            return Ok(true);
        }
        if !key.modifiers.is_empty() {
            if key.code == KeyCode::Char('j') {
                self.timeline.focus = Focus::Composer;
            }
            return Ok(false);
        }
        if self.timeline.focus == Focus::Inspector {
            let inspector = &mut self.timeline.inspector;
            match key.code {
                KeyCode::Up => inspector.scroll(-1),
                KeyCode::Down => inspector.scroll(1),
                KeyCode::PageUp => inspector.scroll(-i32::from(inspector.area.height)),
                KeyCode::PageDown => inspector.scroll(i32::from(inspector.area.height)),
                KeyCode::Home => inspector.offset = 0,
                KeyCode::End => inspector.offset = inspector.maximum,
                KeyCode::Left => self.timeline.focus = Focus::Timeline,
                KeyCode::Esc => self.timeline.focus = Focus::Composer,
                KeyCode::Char('e')
                    if matches!(
                        self.timeline.selected,
                        Some(Key::Task(_) | Key::Repair(_) | Key::Checks)
                    ) =>
                {
                    self.show_evidence = !self.show_evidence
                }
                KeyCode::Enter => self.open_timeline_details()?,
                _ => {
                    self.timeline.focus = Focus::Composer;
                    return Ok(false);
                }
            }
            return Ok(true);
        }
        if key.code == KeyCode::End && self.timeline.focus == Focus::Timeline {
            self.timeline.jump();
            return Ok(true);
        }
        if matches!(key.code, KeyCode::PageUp | KeyCode::PageDown) {
            self.timeline.scroll(if key.code == KeyCode::PageUp {
                -(self.page_size as i32)
            } else {
                self.page_size as i32
            });
            return Ok(true);
        }
        if self.timeline.focus != Focus::Timeline {
            return Ok(false);
        }
        let items = self.timeline_items();
        let selected = self.timeline.selected;
        let index = items
            .iter()
            .position(|i| Some(i.key) == selected)
            .unwrap_or(0);
        match key.code {
            KeyCode::Esc => self.timeline.focus = Focus::Composer,
            KeyCode::Up | KeyCode::Down | KeyCode::Home => {
                let next = match key.code {
                    KeyCode::Up => index.saturating_sub(1),
                    KeyCode::Home => 0,
                    _ => (index + 1).min(items.len().saturating_sub(1)),
                };
                self.timeline.selected = items.get(next).map(|i| i.key);
                self.timeline.held = true;
                self.timeline.reveal = true;
            }
            KeyCode::Right if split => {
                self.timeline.focus = Focus::Inspector;
                self.timeline.held = true;
            }
            KeyCode::Enter => {
                if items.iter().any(|i| Some(i.key) == selected) {
                    if split {
                        self.timeline.focus = Focus::Inspector;
                        self.timeline.held = true;
                    } else {
                        self.open_timeline_details()?;
                    }
                }
            }
            KeyCode::Char(' ') => {
                if let Some(item) = items.iter().find(|i| Some(i.key) == selected) {
                    let expanded = self.timeline.expanded.entry(item.key).or_insert(
                        if item.key == Key::Build {
                            item.state != State::Done
                        } else {
                            !split && item.expanded_by_default()
                        },
                    );
                    *expanded = !*expanded;
                    self.timeline.held = true;
                    self.timeline.reveal = true;
                }
            }
            _ => {
                self.timeline.focus = Focus::Composer;
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn open_timeline_details(&mut self) -> rusqlite::Result<()> {
        if let Some(item) = self
            .timeline_items()
            .iter()
            .find(|i| Some(i.key) == self.timeline.selected)
        {
            if item.key == Key::Plan && item.inspect.is_none() {
                self.perform_action(Action::Details)?;
            } else if let Some(view) = item.inspect {
                self.view = view;
            } else if self.timeline.inspector.area.is_empty() {
                let expanded = self
                    .timeline
                    .expanded
                    .entry(item.key)
                    .or_insert(item.expanded_by_default());
                *expanded = !*expanded;
                self.timeline.held = true;
                self.timeline.reveal = true;
            }
        }
        Ok(())
    }
}
