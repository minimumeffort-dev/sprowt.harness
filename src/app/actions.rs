use super::{App, View};
use crate::{plan::Role, worker::Status};
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyEventKind, KeyModifiers};
use rusqlite::Result;

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Run,
    Stop,
    Retry,
    RetryGit,
    Reopen,
    Network,
    Details,
    Failure,
    Diff,
    Review,
    Findings,
    FixIssues,
    Publish,
    Update,
    Queue,
    History,
    Mods,
    NewMod,
    Close,
    Delete,
}

impl Action {
    pub fn shortcut(self) -> Option<char> {
        match self {
            Self::Run | Self::Stop | Self::Retry | Self::RetryGit | Self::Reopen => Some('r'),
            Self::Network => Some('n'),
            Self::Details => Some('o'),
            Self::Diff => Some('d'),
            Self::Review => Some('e'),
            Self::Publish => Some('s'),
            Self::Update => Some('u'),
            Self::Queue => Some('q'),
            Self::History => Some('t'),
            Self::Mods => Some('p'),
            _ => None,
        }
    }

    pub(super) fn has_shortcut(key: char) -> bool {
        "rnodesuqtp".contains(key)
    }

    pub fn menu_shortcut(self) -> Option<char> {
        match self {
            Self::NewMod => Some('n'),
            Self::Close => Some('c'),
            Self::Delete => Some('d'),
            Self::Failure => Some('f'),
            Self::Findings => Some('i'),
            Self::FixIssues => Some('x'),
            _ => None,
        }
    }
}

pub struct ActionItem {
    pub action: Action,
    pub label: String,
}

impl ActionItem {
    fn new(action: Action, label: impl Into<String>) -> Self {
        Self {
            action,
            label: label.into(),
        }
    }
}

#[derive(Clone, Copy)]
pub enum Tone {
    Quiet,
    Busy,
    Attention,
    Ready,
}

pub struct ActionDock {
    pub status: String,
    pub error: Option<String>,
    pub tone: Tone,
    pub primary: Option<Action>,
    pub actions: Vec<ActionItem>,
}

impl App {
    pub(super) fn findings_key(&mut self, key: KeyEvent, scroll: u16) -> Result<()> {
        match key.code {
            KeyCode::Esc => self.view = View::Chat,
            KeyCode::Up => self.view = View::Findings(scroll.saturating_sub(1)),
            KeyCode::Down => self.view = View::Findings(scroll.saturating_add(1)),
            KeyCode::PageUp => self.view = View::Findings(scroll.saturating_sub(self.page_size)),
            KeyCode::PageDown => self.view = View::Findings(scroll.saturating_add(self.page_size)),
            KeyCode::Char('x') if key.modifiers.is_empty() && key.kind == KeyEventKind::Press => {
                self.perform_action(Action::FixIssues)?;
            }
            KeyCode::Char('e')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.kind == KeyEventKind::Press =>
            {
                self.perform_action(Action::Review)?;
            }
            KeyCode::Char('r')
                if key.modifiers.contains(KeyModifiers::CONTROL)
                    && key.kind == KeyEventKind::Press =>
            {
                self.action_shortcut('r')?;
            }
            KeyCode::Char('h') if key.modifiers.is_empty() => {
                self.history_origin = Some(View::Findings(scroll));
                self.view = View::History(0);
            }
            _ => {}
        }
        Ok(())
    }

    pub fn composer_view(&self) -> View {
        if matches!(self.view, View::Actions(_)) {
            self.action_origin.unwrap_or(View::Chat)
        } else {
            self.view
        }
    }

    pub fn action_error(&self) -> Option<&str> {
        let origin = if matches!(self.view, View::Actions(_) | View::Failure(_)) {
            self.action_origin.unwrap_or(View::Chat)
        } else {
            self.view
        };
        if matches!(origin, View::NewMod) {
            self.notice.as_deref().or(self.project_error.as_deref())
        } else {
            self.worker_error()
        }
    }

    pub fn action_dock(&self) -> ActionDock {
        use Action::*;
        let mut dock = ActionDock {
            status: String::new(),
            error: None,
            tone: Tone::Quiet,
            primary: None,
            actions: Vec::new(),
        };
        let new_mod = matches!(self.composer_view(), View::NewMod);
        let code_mod = self.current_mod().filter(|_| !new_mod);
        if let Some(m) = code_mod {
            let busy = self.execution_busy();
            let active = self.workers.values().any(|w| w.mod_id == m.id && w.busy());
            let worker = self.current_worker();
            let details = m
                .planning
                .as_ref()
                .is_some_and(|p| p.status == "ready" && p.plan.is_some());
            let changed = self.targets.get(&m.id).filter(|t| {
                t.pr_state.as_deref().is_none_or(|s| s == "OPEN")
                    && self.git_states.get(&m.id).is_some_and(|s| s.base != t.head)
            });
            let running = self.workers.values().any(|w| w.mod_id == m.id && w.enabled);
            let retry = self.git_retry_pending();
            let failed_checks = m.execution.as_ref().is_some_and(|e| {
                e.checks
                    .iter()
                    .chain(e.tasks.iter().flat_map(|t| &t.checks))
                    .any(|c| c.failed())
            });
            let final_blocked = m
                .execution
                .as_ref()
                .is_some_and(|e| e.complete() && e.status == "blocked" && !e.checks.is_empty());
            let run = !m.closed
                && (running
                    || retry
                    || self.worker_error().is_some()
                    || !m.queue.is_empty()
                    || worker.is_some_and(|w| w.status != Status::Complete)
                    || m.planning.as_ref().is_some_and(|p| p.status != "ready")
                    || m.planning.as_ref().is_some_and(|p| p.status == "ready")
                        && m.execution
                            .as_ref()
                            .is_none_or(|e| !["review", "applied"].contains(&e.status.as_str())));
            let worker_action = (self.git_activity().is_none()
                && (run || m.closed && m.git_root.is_some()))
            .then_some(if m.closed {
                Reopen
            } else if running {
                Stop
            } else if retry {
                RetryGit
            } else if failed_checks
                || self.worker_error().is_some()
                || worker.is_some_and(|w| w.error.is_some())
                || m.planning.as_ref().is_some_and(|p| p.status != "ready")
            {
                Retry
            } else {
                Run
            });
            if let Some(action) = worker_action {
                dock.actions.push(ActionItem::new(
                    action,
                    match action {
                        Reopen => "Reopen codemod",
                        Stop => "Stop workers",
                        RetryGit => "Retry Git operation",
                        Retry if final_blocked => "Retry final checks",
                        Retry => "Retry work",
                        _ => "Run work",
                    },
                ));
            }
            if self.pending_network().is_some() && !m.closed {
                dock.actions
                    .push(ActionItem::new(Network, "Review domains"));
            }
            if details {
                dock.actions.push(ActionItem::new(
                    Details,
                    if self.plan_details {
                        "Hide plan details"
                    } else {
                        "Show plan details"
                    },
                ));
            }
            if self.worker_error().is_some() || failed_checks || self.failed_task().is_some() {
                dock.actions.push(ActionItem::new(
                    Failure,
                    if self.failed_task().is_some() {
                        "Inspect failure"
                    } else {
                        "Show full error"
                    },
                ));
            }
            if m.execution.is_some() && !busy {
                dock.actions.push(ActionItem::new(Diff, "View diff"));
            }
            if self.version_ready() {
                dock.actions
                    .push(ActionItem::new(Review, "Ask agent to review"));
                dock.actions.push(ActionItem::new(Publish, "Publish PR"));
            }
            if m.agent_review.as_ref().is_some_and(|r| r.report.is_some()) {
                dock.actions
                    .push(ActionItem::new(Findings, "View review findings"));
            }
            if !busy
                && !running
                && self.git_activity().is_none()
                && m.agent_review
                    .as_ref()
                    .is_some_and(|r| r.status == "findings" && r.rounds < 2 && r.current(m))
            {
                dock.actions
                    .push(ActionItem::new(FixIssues, "Fix review issues"));
            }
            if !m.queue.is_empty() && !m.closed {
                dock.actions.push(ActionItem::new(
                    Queue,
                    format!("Manage queue ({})", m.queue.len()),
                ));
            }
            if m.has_worker_history() || !self.task_ids().is_empty() {
                dock.actions
                    .push(ActionItem::new(History, "Tasks and history"));
            }
            if m.closed {
                dock.status = "Closed · work saved".into();
                dock.primary = worker_action;
            } else if m.question().is_some() {
                dock.status = "Worker needs your answer".into();
                dock.tone = Tone::Attention;
            } else if self.pending_network().is_some() {
                dock.status = "Network access needed".into();
                dock.tone = Tone::Attention;
                dock.primary = Some(Network);
            } else if let Some(activity) = self.git_activity().filter(|activity| {
                *activity != "checking target branch"
                    || !active
                        && !self.auto_plans.contains(&m.id)
                        && !self.auto_runs.contains(&m.id)
            }) {
                dock.status = activity.into();
                dock.tone = Tone::Busy;
            } else if retry {
                dock.status = "Git operation paused".into();
                dock.tone = Tone::Attention;
                dock.primary = Some(RetryGit);
            } else if active || self.auto_plans.contains(&m.id) || self.auto_runs.contains(&m.id) {
                dock.status = if worker.is_some_and(|w| w.role == Role::Planner) {
                    "Planning".into()
                } else if worker.is_some_and(|w| w.role == Role::Reviewer) {
                    "Reviewing changes".into()
                } else {
                    m.execution.as_ref().map_or("Working".into(), |e| {
                        if e.status == "verifying" {
                            return "Final verification".into();
                        }
                        if e.tasks
                            .iter()
                            .any(|t| t.status != "done" && t.restoring_runtime())
                        {
                            return "Restoring task environment".into();
                        }
                        format!(
                            "Working · {}/{} tasks done",
                            e.tasks.iter().filter(|t| t.status == "done").count(),
                            e.tasks.len()
                        )
                    })
                };
                dock.tone = Tone::Busy;
            } else if busy {
                dock.status = "Workers ready".into();
            } else if let Some(target) = self.targets.get(&m.id)
                && matches!(target.pr_state.as_deref(), Some("MERGED" | "CLOSED"))
            {
                dock.status = if target.pr_state.as_deref() == Some("MERGED") {
                    "PR merged"
                } else {
                    "PR closed"
                }
                .into();
                dock.primary = Some(NewMod);
            } else if let Some(target) = changed {
                dock.status = format!("{} changed · combined checks needed", target.branch);
                dock.primary = Some(Update);
                dock.tone = Tone::Attention;
            } else if final_blocked {
                dock.status = "Final verification blocked".into();
                dock.primary = if self.failed_task().is_some() {
                    Some(Failure)
                } else {
                    worker_action
                };
                dock.tone = Tone::Attention;
                dock.error = m
                    .execution
                    .as_ref()
                    .and_then(|e| e.checks.iter().find(|c| c.failed()).map(|c| c.brief()));
            } else if self.worker_error().is_some() || failed_checks || self.failed_task().is_some()
            {
                dock.status = "Work paused".into();
                dock.primary = if self.failed_task().is_some() || self.worker_error().is_some() {
                    Some(Failure)
                } else {
                    worker_action
                };
                dock.tone = Tone::Attention;
            } else if let Some(review) = m
                .agent_review
                .as_ref()
                .filter(|r| r.status == "findings" && r.current(m))
            {
                let count = review.report.as_ref().map_or(0, |r| r.findings.len());
                dock.status = format!(
                    "{count} review issue{} found",
                    if count == 1 { "" } else { "s" }
                );
                dock.primary = Some(Findings);
                dock.tone = Tone::Attention;
            } else if self.published() {
                dock.status = "PR published".into();
                dock.tone = Tone::Ready;
            } else if self.version_ready() {
                let review = m.agent_review.as_ref().filter(|r| r.current(m));
                match review.map(|r| r.status.as_str()) {
                    Some("clean") => {
                        dock.status = "Review passed · changes ready".into();
                        dock.primary = Some(Publish);
                    }
                    Some("paused" | "blocked") => {
                        dock.status = "Review paused".into();
                        dock.primary = Some(Review);
                    }
                    _ => {
                        dock.status = "Changes ready".into();
                        dock.primary = Some(Review);
                    }
                }
                dock.tone = Tone::Ready;
            } else {
                dock.status = "Work paused".into();
                dock.primary = worker_action;
            }
            if matches!(
                dock.status.as_str(),
                "Work paused" | "Changes ready" | "PR published" | "Review passed · changes ready"
            ) && let Some(execution) = &m.execution
            {
                dock.status.push_str(&format!(
                    " · {}/{} tasks done",
                    execution
                        .tasks
                        .iter()
                        .filter(|task| task.status == "done")
                        .count(),
                    execution.tasks.len()
                ));
            }
            if !m.closed && self.git_activity().is_none() {
                dock.actions.push(ActionItem::new(Close, "Close codemod"));
            }
            if self.git_activity().is_none() {
                dock.actions.push(ActionItem::new(Delete, "Delete codemod"));
            }
        } else {
            dock.status = "New codemod".into();
            if let Some(activity) = self.project_activity() {
                dock.status = activity.into();
                dock.tone = Tone::Busy;
            }
            if self.action_error().is_some() {
                dock.tone = Tone::Attention;
                dock.actions
                    .push(ActionItem::new(Failure, "Show full error"));
                dock.primary = Some(Failure);
            }
        }
        let update_label = code_mod
            .and_then(|m| self.targets.get(&m.id))
            .filter(|t| self.git_state().is_some_and(|s| s.base != t.head))
            .map_or("Sync project / target".into(), |t| {
                format!("Update from {}", t.branch)
            });
        dock.actions.push(ActionItem::new(
            Update,
            if code_mod.is_some() {
                update_label
            } else {
                "Sync project".into()
            },
        ));
        if !self.mods.is_empty() {
            dock.actions.push(ActionItem::new(Mods, "Switch codemod"));
        }
        if !new_mod {
            dock.actions.push(ActionItem::new(NewMod, "New codemod"));
        }
        if let Some(error) = self
            .action_error()
            .filter(|_| !matches!(dock.tone, Tone::Busy))
        {
            dock.error = Some(error.lines().next().unwrap_or(error).trim().into());
        }
        if let Some(primary) = dock.primary
            && let Some(index) = dock.actions.iter().position(|a| a.action == primary)
        {
            let item = dock.actions.remove(index);
            dock.actions.insert(0, item);
        }
        dock
    }

    pub(super) fn open_actions(&mut self) {
        self.action_origin = Some(self.view);
        if let Some(first) = self.action_dock().actions.first() {
            self.view = View::Actions(first.action);
        }
    }

    pub(super) fn actions_key(&mut self, key: KeyEvent, selected: Action) -> Result<()> {
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        let actions = self.action_dock().actions;
        let index = actions
            .iter()
            .position(|a| a.action == selected)
            .unwrap_or(0);
        match key.code {
            KeyCode::Esc | KeyCode::Char('g') if key.code == KeyCode::Esc || ctrl => {
                self.view = self.action_origin.take().unwrap_or(View::Chat);
            }
            KeyCode::Up | KeyCode::Down => {
                let next = if key.code == KeyCode::Up {
                    index.saturating_sub(1)
                } else {
                    (index + 1).min(actions.len().saturating_sub(1))
                };
                if let Some(item) = actions.get(next) {
                    self.view = View::Actions(item.action);
                }
            }
            KeyCode::Enter if key.modifiers.is_empty() && key.kind == KeyEventKind::Press => {
                // A changed state must not redirect Enter to a different action.
                if actions.iter().any(|a| a.action == selected) {
                    self.view = self.action_origin.take().unwrap_or(View::Chat);
                    self.perform_action(selected)?;
                }
            }
            KeyCode::Char(c) if ctrl && Action::has_shortcut(c) => {
                self.view = self.action_origin.take().unwrap_or(View::Chat);
                self.action_shortcut(c)?;
            }
            KeyCode::Char(c) if key.modifiers.is_empty() && key.kind == KeyEventKind::Press => {
                if let Some(item) = actions.iter().find(|a| a.action.menu_shortcut() == Some(c)) {
                    self.view = self.action_origin.take().unwrap_or(View::Chat);
                    self.perform_action(item.action)?;
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn action_shortcut(&mut self, key: char) -> Result<()> {
        if let Some(item) = self
            .action_dock()
            .actions
            .into_iter()
            .find(|a| a.action.shortcut() == Some(key))
        {
            self.perform_action(item.action)?;
        }
        Ok(())
    }

    pub(super) fn perform_action(&mut self, action: Action) -> Result<()> {
        if !self
            .action_dock()
            .actions
            .iter()
            .any(|a| a.action == action)
        {
            return Ok(());
        }
        match action {
            Action::Run | Action::Stop | Action::Retry | Action::RetryGit | Action::Reopen => {
                self.toggle_worker()?
            }
            Action::Network => {
                if let Some(request) = self.pending_network() {
                    self.view = View::Network(request.id, 0);
                }
            }
            Action::Details => {
                self.plan_details = !self.plan_details;
                self.focus_plan = true;
            }
            Action::Failure => {
                self.action_origin = Some(self.view);
                self.view = self
                    .failed_task()
                    .map_or(View::Failure(0), |id| View::Task(id, 0));
            }
            Action::Diff => self.open_review()?,
            Action::Review => {
                if let Some(index) = self.active {
                    self.begin_agent_review(index)?;
                }
            }
            Action::Findings => self.view = View::Findings(0),
            Action::FixIssues => {
                if let Some(index) = self.active {
                    let id = self.mods[index].id;
                    if self.store.review_fixes(&self.mods[index])? {
                        self.mods[index].execution = self.store.execution(id)?;
                        self.executing_mods.insert(id);
                    }
                    self.mods[index].agent_review = self.store.review_state(id)?;
                    self.view = View::Findings(0);
                }
            }
            Action::Publish => {
                self.review = None;
                self.publish_after_review = true;
                self.open_review()?;
                if self.review.is_some() && self.can_publish() {
                    self.publish_after_review = false;
                    self.open_publication()?;
                }
            }
            Action::Update => {
                self.project_check = None;
                self.maintain_project();
                if matches!(self.view, View::Chat)
                    && let Some(index) = self.active
                    && !self.mods[index].closed
                    && self.mods[index].execution.is_some()
                {
                    self.check_target(index, false);
                }
            }
            Action::Queue => self.view = View::Queue(0),
            Action::History => {
                self.history_origin = None;
                self.view = if self.task_ids().is_empty() {
                    View::History(0)
                } else {
                    View::Tasks(0)
                };
            }
            Action::Mods => {
                self.show_closed = self.current_mod().is_some_and(|m| m.closed);
                self.view = View::Mods(
                    self.picker_indices()
                        .iter()
                        .position(|i| Some(*i) == self.active)
                        .unwrap_or(0),
                );
            }
            Action::NewMod => {
                self.view = View::NewMod;
                self.input = crate::ui::name_input();
            }
            Action::Close | Action::Delete => {
                if let Some(index) = self.active {
                    self.action_origin = Some(View::Chat);
                    self.view = if action == Action::Close {
                        View::CloseMod(index)
                    } else {
                        View::DeleteMod(index)
                    };
                }
            }
        }
        Ok(())
    }
}
